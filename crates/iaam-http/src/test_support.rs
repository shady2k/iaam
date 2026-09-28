//! Loopback HTTP fixtures for transport-level acceptance tests.
//!
//! This module exists only with `cfg(test)` or the `test-support` feature. It
//! keeps the real `HttpClient` below the gateway while replacing only the
//! destination base URL with a loopback listener.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::client::{HttpClient, REQUEST_TIMEOUT};
use crate::gateway::Transport;
use crate::{HttpError, HttpRequest, HttpResponse};

#[derive(Debug, Clone, Copy)]
enum BodyEnding {
    Complete,
    Truncated,
    Stall,
}

/// One scripted HTTP response from [`LoopbackServer`].
#[derive(Debug, Clone)]
pub struct LoopbackReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    ending: BodyEnding,
}

impl LoopbackReply {
    /// A complete response with the given status and body.
    #[must_use]
    pub fn complete(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            ending: BodyEnding::Complete,
        }
    }

    /// A redirect response naming `location`.
    #[must_use]
    pub fn redirect(status: u16, location: &str) -> Self {
        Self::complete(status, Vec::new()).with_header("Location", location)
    }

    /// A response whose declared body is cut short when the connection closes.
    #[must_use]
    pub fn truncated_body(status: u16, body_prefix: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body_prefix.into(),
            ending: BodyEnding::Truncated,
        }
    }

    /// A response whose declared body remains open until the server is dropped.
    #[must_use]
    pub fn stalled_body(status: u16, body_prefix: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body_prefix.into(),
            ending: BodyEnding::Stall,
        }
    }

    /// Add a response header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// A real HTTP/1.1 server bound to an ephemeral loopback port.
pub struct LoopbackServer {
    address: SocketAddr,
    received: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    request_targets: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    stall_release: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    /// Start a server that answers requests in script order.
    pub fn start(replies: impl IntoIterator<Item = LoopbackReply>) -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let received = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let request_targets = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let (stall_release, stall_wait) = mpsc::channel();
        let mut replies: VecDeque<_> = replies.into_iter().collect();

        let thread_received = Arc::clone(&received);
        let thread_accepted = Arc::clone(&accepted);
        let thread_targets = Arc::clone(&request_targets);
        let thread_stopping = Arc::clone(&stopping);
        let handle = thread::spawn(move || {
            for accepted in listener.incoming() {
                let mut stream = accepted.unwrap_or_else(|error| {
                    panic!("loopback HTTP server could not accept a request: {error}")
                });
                if thread_stopping.load(Ordering::SeqCst) {
                    break;
                }
                thread_accepted.fetch_add(1, Ordering::SeqCst);
                let target = read_request_target(&mut stream).unwrap_or_else(|error| {
                    panic!("loopback HTTP server could not read a request: {error}")
                });
                thread_received.fetch_add(1, Ordering::SeqCst);
                thread_targets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(target);
                let reply = replies
                    .pop_front()
                    .unwrap_or_else(|| LoopbackReply::complete(500, "unscripted request"));
                write_reply(&mut stream, &reply, &stall_wait).unwrap_or_else(|error| {
                    panic!("loopback HTTP server could not write a response: {error}")
                });
            }
        });

        Ok(Self {
            address,
            received,
            accepted,
            request_targets,
            stopping,
            stall_release,
            thread: Some(handle),
        })
    }

    /// Base URL accepted by this server.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Number of request status lines the server received.
    #[must_use]
    pub fn requests_received(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }

    /// Number of TCP connections the server accepted.
    #[must_use]
    pub fn connections_accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Request targets in arrival order.
    #[must_use]
    pub fn request_targets(&self) -> Vec<String> {
        self.request_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.stall_release.send(());
        drop(TcpStream::connect(self.address));
        let Some(handle) = self.thread.take() else {
            return;
        };
        if let Err(panic) = handle.join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

/// The production HTTP transport pointed at a [`LoopbackServer`].
pub struct HttpClientHarness {
    client: HttpClient,
    base_url: String,
    timeout: Duration,
}

impl HttpClientHarness {
    /// Use the real client with the production request timeout.
    #[must_use]
    pub fn new(server: &LoopbackServer) -> Self {
        Self {
            client: HttpClient::new(),
            base_url: server.base_url(),
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// Shorten the real request timeout for a deliberately stalled fixture.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Send through the real client to the loopback destination.
    pub async fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.client
            .send_to_base(request, &self.base_url, self.timeout)
            .await
    }
}

impl Transport for HttpClientHarness {
    fn send<'a>(
        &'a self,
        request: &'a HttpRequest,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        Self::send(self, request)
    }
}

fn read_request_target(stream: &mut TcpStream) -> std::io::Result<String> {
    read_request_target_observed(stream, |_| {})
}

fn read_request_target_observed(
    stream: &mut TcpStream,
    mut incomplete: impl FnMut(usize),
) -> std::io::Result<String> {
    const MAX_REQUEST_HEADERS: usize = 64 * 1024;

    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "loopback request ended before its headers",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        {
            if header_end > MAX_REQUEST_HEADERS {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "loopback request headers exceed 64 KiB",
                ));
            }
            break;
        }
        if request.len() >= MAX_REQUEST_HEADERS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "loopback request headers exceed 64 KiB",
            ));
        }
        incomplete(request.len());
    }
    let request_line = request
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request line")
        })?;
    request_line
        .split_ascii_whitespace()
        .nth(1)
        .map(str::to_owned)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "missing request target")
        })
}

fn write_reply(
    stream: &mut TcpStream,
    reply: &LoopbackReply,
    stall_wait: &mpsc::Receiver<()>,
) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {} Test\r\n", reply.status)?;
    for (name, value) in &reply.headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    let declared_length = match reply.ending {
        BodyEnding::Complete => reply.body.len(),
        BodyEnding::Truncated | BodyEnding::Stall => reply.body.len().saturating_add(1024),
    };
    write!(
        stream,
        "Content-Length: {declared_length}\r\nConnection: close\r\n\r\n"
    )?;
    stream.write_all(&reply.body)?;
    stream.flush()?;

    match reply.ending {
        BodyEnding::Complete => Ok(()),
        BodyEnding::Truncated => stream.shutdown(Shutdown::Both),
        BodyEnding::Stall => {
            let _ = stall_wait.recv();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Shutdown;
    use std::panic::AssertUnwindSafe;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    enum ParseEvent {
        Incomplete(usize),
        Complete(Result<String, std::io::ErrorKind>),
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind loopback pair");
        let writer = TcpStream::connect(listener.local_addr().expect("pair address"))
            .expect("connect loopback pair");
        let (reader, _) = listener.accept().expect("accept loopback pair");
        (writer, reader)
    }

    fn parse_in_thread(
        mut reader: TcpStream,
    ) -> (mpsc::Receiver<ParseEvent>, thread::JoinHandle<()>) {
        let (events, observed) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = read_request_target_observed(&mut reader, |len| {
                events
                    .send(ParseEvent::Incomplete(len))
                    .expect("parser observer remains");
            })
            .map_err(|error| error.kind());
            events
                .send(ParseEvent::Complete(result))
                .expect("parser observer remains");
        });
        (observed, handle)
    }

    #[test]
    fn a_fragmented_request_is_not_recorded_before_its_headers_end() {
        let (mut writer, reader) = tcp_pair();
        let (events, handle) = parse_in_thread(reader);
        let first = b"GET /fragmented HTTP/1.1\r\n";
        writer.write_all(first).expect("first request fragment");

        loop {
            match events.recv().expect("parser event") {
                ParseEvent::Incomplete(len) if len >= first.len() => break,
                ParseEvent::Incomplete(_) => {}
                ParseEvent::Complete(result) => {
                    panic!("request completed from a partial header: {result:?}")
                }
            }
        }

        writer
            .write_all(b"Host: loopback\r\n\r\n")
            .expect("last request fragment");
        let target = loop {
            match events.recv().expect("parser result") {
                ParseEvent::Incomplete(_) => {}
                ParseEvent::Complete(result) => break result.expect("valid request"),
            }
        };
        handle.join().expect("parser thread");
        assert_eq!(target, "/fragmented");
    }

    #[test]
    fn a_complete_header_at_the_bound_is_accepted() {
        let (mut writer, reader) = tcp_pair();
        let (events, handle) = parse_in_thread(reader);
        let mut request = b"GET /boundary HTTP/1.1\r\n".to_vec();
        request.resize(64 * 1024 - 4, b'a');
        request.extend_from_slice(b"\r\n\r\n");
        writer.write_all(&request).expect("bounded request");

        let target = loop {
            match events.recv().expect("parser result") {
                ParseEvent::Incomplete(_) => {}
                ParseEvent::Complete(result) => break result.expect("valid bounded request"),
            }
        };
        handle.join().expect("parser thread");
        assert_eq!(target, "/boundary");
    }

    #[test]
    fn a_header_without_a_terminator_cannot_reach_the_bound() {
        let (mut writer, reader) = tcp_pair();
        let (events, handle) = parse_in_thread(reader);
        let mut request = b"GET /oversized HTTP/1.1\r\n".to_vec();
        request.resize(64 * 1024, b'a');
        writer.write_all(&request).expect("oversized request");
        writer.shutdown(Shutdown::Write).expect("finish request");

        let error = loop {
            match events.recv().expect("parser result") {
                ParseEvent::Incomplete(_) => {}
                ParseEvent::Complete(result) => break result.expect_err("oversized header"),
            }
        };
        handle.join().expect("parser thread");
        assert_eq!(error, std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn eof_before_the_header_terminator_is_refused() {
        let (mut writer, reader) = tcp_pair();
        let (events, handle) = parse_in_thread(reader);
        writer
            .write_all(b"GET /partial HTTP/1.1\r\n")
            .expect("partial request");
        writer.shutdown(Shutdown::Write).expect("finish request");

        let error = loop {
            match events.recv().expect("parser result") {
                ParseEvent::Incomplete(_) => {}
                ParseEvent::Complete(result) => break result.expect_err("incomplete header"),
            }
        };
        handle.join().expect("parser thread");
        assert_eq!(error, std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn a_server_thread_failure_is_propagated_when_the_server_is_dropped() {
        let server =
            LoopbackServer::start([LoopbackReply::complete(200, "")]).expect("loopback server");
        let mut stream = TcpStream::connect(server.address).expect("connect malformed request");
        stream
            .write_all(b"invalid\r\n\r\n")
            .expect("write malformed request");
        stream
            .shutdown(Shutdown::Write)
            .expect("finish malformed request");
        while server.accepted.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }

        let dropped = std::panic::catch_unwind(AssertUnwindSafe(|| drop(server)));

        assert!(dropped.is_err(), "server thread failure was discarded");
    }
}
