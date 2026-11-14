//! The operator's log, as a test's own thread writes it.
//!
//! A test holds a [`Log`] and asserts on the lines its own thread emitted, so
//! what an operator would read is checked, not the code that meant to write it.

thread_local! {
    /// What this test's thread logged, while a `Log` holds it.
    static CAPTURED: std::cell::RefCell<Option<Vec<u8>>> =
        const { std::cell::RefCell::new(None) };
}

/// Everything this thread logs while it is held, as the operator's log
/// shows it.
///
/// One global subscriber writing into a per-thread buffer, rather than a
/// subscriber set per test: tracing caches each event's interest for the
/// whole process, and a test thread with no subscriber of its own can
/// cache "never" for an event another thread's scoped subscriber wants.
pub(crate) struct Log;

struct Sink;

impl std::io::Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        CAPTURED.with(|captured| {
            if let Some(lines) = captured.borrow_mut().as_mut() {
                lines.extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Log {
    pub(crate) fn capture() -> Self {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(|| Sink)
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::INFO)
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other test installs a subscriber");
        });
        CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
        Self
    }

    pub(crate) fn text(&self) -> String {
        let lines = CAPTURED.with(|captured| captured.borrow().clone().unwrap_or_default());
        String::from_utf8(lines).expect("UTF-8")
    }

    /// The one line holding `message`, which must hold every fragment.
    pub(crate) fn assert_line(&self, level: &str, message: &str, fragments: &[&str]) {
        let text = self.text();
        let line = text
            .lines()
            .find(|line| line.contains(message))
            .unwrap_or_else(|| panic!("no {message:?} in the log:\n{text}"));
        assert!(line.contains(level), "{line}");
        for fragment in fragments {
            assert!(line.contains(fragment), "no {fragment:?} in {line}");
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        CAPTURED.with(|captured| *captured.borrow_mut() = None);
    }
}
