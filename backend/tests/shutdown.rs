//! The process-level contract: a stop request must shut the market down
//! gracefully, not kill it.
//!
//! `docker stop`, `systemctl stop` and Kubernetes send SIGTERM. Until the
//! binary handled it, the process died on the spot and the in-memory counter
//! batch was lost — this test runs the real binary to keep that from coming
//! back.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A port nobody is using right now: bind, read, drop.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a probe port");
    listener.local_addr().expect("probe port").port()
}

fn start_market(db_dir: &std::path::Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_offline-market-backend"))
        .env("MARKET_BIND", format!("127.0.0.1:{port}"))
        .env("MARKET_DB", db_dir.join("market.sqlite3"))
        .env("MARKET_PUBLIC_URL", format!("http://127.0.0.1:{port}"))
        .env("ADMIN_PASSWORD", "shutdown-test-pw")
        .env("MARKET_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the market binary")
}

/// Reads `/health` over a raw socket; `None` while the port is not answering.
fn health(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

fn wait_until_serving(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if health(port).is_some_and(|reply| reply.starts_with("HTTP/1.1 200")) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the market never started serving on port {port}");
}

fn signal(pid: u32, name: &str) {
    let status = Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} {pid} failed");
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll the child") {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("the market ignored the stop signal and had to be killed");
}

/// Both stop signals take the graceful path: stop accepting, let in-flight
/// requests finish, flush the counter batch, then exit with success.
#[test]
fn a_stop_signal_shuts_down_cleanly() {
    for name in ["TERM", "INT"] {
        let dir =
            std::env::temp_dir().join(format!("market-shutdown-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let port = free_port();
        let mut child = start_market(&dir, port);
        wait_until_serving(port);

        signal(child.id(), name);
        let status = wait_for_exit(&mut child);

        assert_eq!(
            status.code(),
            Some(0),
            "SIG{name} must exit through the graceful path, got {status:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A stop signal must not leave the database locked or half-written: the next
/// process has to be able to open it and serve immediately.
#[test]
fn a_restart_after_a_stop_signal_reuses_the_database() {
    let dir = std::env::temp_dir().join(format!("market-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let port = free_port();

    let mut first = start_market(&dir, port);
    wait_until_serving(port);
    signal(first.id(), "TERM");
    assert_eq!(wait_for_exit(&mut first).code(), Some(0));

    let mut second = start_market(&dir, port);
    wait_until_serving(port);
    signal(second.id(), "TERM");
    assert_eq!(wait_for_exit(&mut second).code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}
