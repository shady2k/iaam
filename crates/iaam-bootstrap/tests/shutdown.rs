#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    home: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn command(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_iaam"));
    command
        .env_clear()
        .env("HOME", home)
        .env("RUST_LOG", "info")
        .env("NO_COLOR", "1");
    command
}

fn answers_http(address: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    if stream
        .write_all(b"GET /v1/openapi.json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut response = [0; 12];
    stream.read_exact(&mut response).is_ok() && response.starts_with(b"HTTP/1.1 ")
}

#[test]
fn serve_exits_successfully_on_sigterm() {
    let home = std::env::temp_dir().join(format!("iaam-sigterm-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let claim = command(&home).arg("claim").output().unwrap();
    assert!(claim.status.success(), "{claim:?}");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let log_path = home.join("serve.log");
    let log = std::fs::File::create(&log_path).unwrap();
    drop(listener);
    let child = command(&home)
        .arg("serve")
        .env("IAAM_LISTEN", address.to_string())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let mut server = Server { child, home };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !answers_http(address) {
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "serve exited before readiness"
        );
        assert!(Instant::now() < deadline, "serve did not answer HTTP");
        std::thread::sleep(Duration::from_millis(20));
    }

    let sent = Command::new("kill")
        .args(["-TERM", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "serve did not stop within five seconds"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = std::fs::read_to_string(log_path).unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "serve failed: {status}; logs: {log}"
    );
    assert!(
        log.contains("shutdown signal received") && log.contains("SIGTERM"),
        "{log}"
    );
}
