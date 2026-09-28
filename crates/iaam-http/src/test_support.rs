//! Loopback HTTP fixtures for transport-level acceptance tests.
//!
//! This module exists only with `cfg(test)` or the `test-support` feature. It
//! keeps the real `HttpClient` below the gateway while replacing only the
//! destination base URL with a loopback listener.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
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
    hold_before_status: bool,
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
            hold_before_status: false,
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
            hold_before_status: false,
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
            hold_before_status: false,
        }
    }

    /// A complete response held before its status line until the server is released.
    #[must_use]
    pub fn held(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            ending: BodyEnding::Complete,
            hold_before_status: true,
        }
    }

    /// Add a response header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

#[derive(Default)]
struct ReplyGate {
    permits: Mutex<usize>,
    ready: Condvar,
}

impl ReplyGate {
    fn wait(&self, stopping: &AtomicBool) {
        let mut permits = self
            .permits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *permits == 0 && !stopping.load(Ordering::SeqCst) {
            permits = self
                .ready
                .wait(permits)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if *permits != 0 {
            *permits -= 1;
        }
    }

    fn release(&self) {
        *self
            .permits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        self.ready.notify_one();
    }

    fn stop(&self) {
        self.ready.notify_all();
    }
}

/// A real HTTP/1.1 server bound to an ephemeral loopback port.
pub struct LoopbackServer {
    address: SocketAddr,
    received: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    request_targets: Arc<Mutex<Vec<String>>>,
    stopping: Arc<AtomicBool>,
    reply_gate: Arc<ReplyGate>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    /// Start a server that answers requests in script order.
    pub fn start(replies: impl IntoIterator<Item = LoopbackReply>) -> std::io::Result<Self> {
        Self::start_observed(replies, |_, _| {})
    }

    /// Start a server and observe each complete request at the TCP receiver.
    ///
    /// The observer runs after the request headers arrive and before the
    /// scripted status is written. It therefore counts wire arrivals rather
    /// than gateway or transport calls.
    pub fn start_observed(
        replies: impl IntoIterator<Item = LoopbackReply>,
        mut observe: impl FnMut(&str, u16) + Send + 'static,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let received = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::new(AtomicUsize::new(0));
        let request_targets = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let reply_gate = Arc::new(ReplyGate::default());
        let mut replies: VecDeque<_> = replies.into_iter().collect();

        let thread_received = Arc::clone(&received);
        let thread_accepted = Arc::clone(&accepted);
        let thread_targets = Arc::clone(&request_targets);
        let thread_stopping = Arc::clone(&stopping);
        let thread_gate = Arc::clone(&reply_gate);
        let handle = thread::spawn(move || {
            let mut handlers = Vec::new();
            for accepted in listener.incoming() {
                let mut stream = accepted.unwrap_or_else(|error| {
                    panic!("loopback HTTP server could not accept a request: {error}")
                });
                if thread_stopping.load(Ordering::SeqCst) {
                    break;
                }
                thread_accepted.fetch_add(1, Ordering::SeqCst);
                let target = match read_request_target(&mut stream) {
                    Ok(target) => target,
                    Err(error)
                        if thread_stopping.load(Ordering::SeqCst)
                            && error.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        break;
                    }
                    Err(error) => {
                        panic!("loopback HTTP server could not read a request: {error}");
                    }
                };
                thread_received.fetch_add(1, Ordering::SeqCst);
                thread_targets
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(target.clone());
                let reply = replies
                    .pop_front()
                    .unwrap_or_else(|| LoopbackReply::complete(500, "unscripted request"));
                observe(&target, reply.status);
                let handler_gate = Arc::clone(&thread_gate);
                let handler_stopping = Arc::clone(&thread_stopping);
                handlers.push(thread::spawn(move || {
                    write_reply(&mut stream, &reply, &handler_gate, &handler_stopping)
                        .unwrap_or_else(|error| {
                            panic!("loopback HTTP server could not write a response: {error}")
                        });
                }));
            }
            for handler in handlers {
                if let Err(panic) = handler.join() {
                    std::panic::resume_unwind(panic);
                }
            }
        });

        Ok(Self {
            address,
            received,
            accepted,
            request_targets,
            stopping,
            reply_gate,
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

    /// Release one response held before its status or while its body stalls.
    pub fn release_one(&self) {
        self.reply_gate.release();
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.reply_gate.stop();
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

    fn send_observed<'a>(
        &'a self,
        request: &'a HttpRequest,
        observe: Box<dyn FnOnce(u16, Option<Duration>) + Send + 'a>,
    ) -> impl Future<Output = Result<HttpResponse, HttpError>> + Send + 'a {
        self.client
            .send_to_base_observed(request, &self.base_url, self.timeout, observe)
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
    reply_gate: &ReplyGate,
    stopping: &AtomicBool,
) -> std::io::Result<()> {
    if reply.hold_before_status {
        reply_gate.wait(stopping);
        if stopping.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
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
            reply_gate.wait(stopping);
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
    fn a_complete_header_past_the_bound_is_refused() {
        let (mut writer, reader) = tcp_pair();
        let (events, handle) = parse_in_thread(reader);
        let mut request = b"GET /too-large HTTP/1.1\r\n".to_vec();
        request.resize(64 * 1024 - 3, b'a');
        request.extend_from_slice(b"\r\n\r\n");
        writer.write_all(&request).expect("oversized request");

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
    fn reply_gate_consumes_exactly_one_permit_per_wait() {
        let gate = Arc::new(ReplyGate::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_gate = Arc::clone(&gate);
        let worker_stopping = Arc::clone(&stopping);
        let (events, observed) = mpsc::channel();
        let handle = thread::spawn(move || {
            worker_gate.wait(&worker_stopping);
            events.send(1).expect("first wake observer");
            worker_gate.wait(&worker_stopping);
            events.send(2).expect("second wake observer");
        });

        assert!(
            observed.recv_timeout(Duration::from_millis(20)).is_err(),
            "a waiter woke without a permit"
        );
        gate.release();
        assert_eq!(
            observed
                .recv_timeout(Duration::from_secs(1))
                .expect("first permit wakes one wait"),
            1
        );
        assert!(
            observed.recv_timeout(Duration::from_millis(20)).is_err(),
            "one permit woke two waits"
        );
        gate.release();
        assert_eq!(
            observed
                .recv_timeout(Duration::from_secs(1))
                .expect("second permit wakes second wait"),
            2
        );
        handle.join().expect("reply gate worker");
    }

    #[test]
    fn reply_gate_stop_wakes_a_waiter_without_a_permit() {
        let gate = Arc::new(ReplyGate::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_gate = Arc::clone(&gate);
        let worker_stopping = Arc::clone(&stopping);
        let (events, observed) = mpsc::channel();
        let handle = thread::spawn(move || {
            worker_gate.wait(&worker_stopping);
            events.send(()).expect("stop observer");
        });

        assert!(
            observed.recv_timeout(Duration::from_millis(20)).is_err(),
            "a waiter woke before stop"
        );
        stopping.store(true, Ordering::SeqCst);
        gate.stop();
        let woke = observed.recv_timeout(Duration::from_secs(1));
        if woke.is_err() {
            gate.release();
        }
        handle.join().expect("reply gate worker");
        woke.expect("stop wakes a waiter");
    }

    #[test]
    fn release_one_delivers_one_held_response() {
        let server =
            LoopbackServer::start([LoopbackReply::held(200, "held")]).expect("loopback server");
        let address = server.address;
        let (events, observed) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).expect("connect held request");
            stream
                .write_all(b"GET /held HTTP/1.1\r\nHost: loopback\r\n\r\n")
                .expect("write held request");
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("bound held read");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("read held response");
            events.send(response).expect("held response observer");
        });
        while server.requests_received() == 0 {
            thread::yield_now();
        }
        assert!(
            observed.recv_timeout(Duration::from_millis(20)).is_err(),
            "held response arrived before release"
        );
        server.release_one();
        let response = observed
            .recv_timeout(Duration::from_secs(1))
            .expect("released response arrives");
        handle.join().expect("held response worker");
        assert!(
            response.starts_with(b"HTTP/1.1 200 "),
            "unexpected response: {}",
            String::from_utf8_lossy(&response)
        );
    }

    #[test]
    fn dropping_an_idle_server_is_clean() {
        let server = LoopbackServer::start([]).expect("loopback server");
        drop(server);
    }

    #[test]
    fn an_incomplete_request_failure_is_propagated_when_the_server_is_dropped() {
        let server =
            LoopbackServer::start([LoopbackReply::complete(200, "")]).expect("loopback server");
        let mut stream = TcpStream::connect(server.address).expect("connect incomplete request");
        stream
            .write_all(b"GET /partial HTTP/1.1\r\n")
            .expect("write incomplete request");
        stream
            .shutdown(Shutdown::Write)
            .expect("finish incomplete request");
        while server.accepted.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(20));

        let dropped = std::panic::catch_unwind(AssertUnwindSafe(|| drop(server)));

        assert!(dropped.is_err(), "incomplete request failure was discarded");
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
