mod common;

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use common::{hopbox, repo};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn port_forward_reaches_remote() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let local = free_port();
    let spec = format!("{local}:9123");
    let mut child = hopbox(
        &s,
        &r,
        &[
            "run",
            "--port",
            &spec,
            "--",
            "socat TCP-LISTEN:9123,reuseaddr,fork SYSTEM:'echo pong' & sleep 4; kill $!",
        ],
    )
    .spawn()
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut got = String::new();
    while Instant::now() < deadline {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", local)) {
            got.clear();
            let _ = c.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = c.read_to_string(&mut got);
            if got == "pong\n" {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = child.wait().unwrap();
    assert_eq!(got, "pong\n");
    assert_eq!(status.code(), Some(0));
    assert!(
        TcpStream::connect(("127.0.0.1", local)).is_err(),
        "forward must be cancelled after the run"
    );
}

#[test]
fn port_in_use_125() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let busy = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port().to_string();
    let o = hopbox(&s, &r, &["run", "--port", &port, "--", "echo ran"])
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(125),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!String::from_utf8_lossy(&o.stdout).contains("ran"));
}

#[test]
fn port_rejects_bad_specs_and_bg() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    for args in [
        vec!["run", "--port", "0", "--", "true"],
        vec!["run", "--port", "70000", "--", "true"],
        vec!["run", "--port", "a:b", "--", "true"],
        vec!["run", "--port", "3000", "--bg", "--", "true"],
    ] {
        let o = hopbox(&s, &r, &args).output().unwrap();
        assert_eq!(o.status.code(), Some(125), "{args:?}");
    }
}

fn pong(port: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) {
            let mut got = String::new();
            let _ = c.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = c.read_to_string(&mut got);
            if got == "pong\n" {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn same_port_second_run_125_and_first_keeps_its_forward() {
    let Some(s) = common::sshd() else { return };
    let (r1, r2) = (repo(&s), repo(&s));
    let local = free_port();
    let spec = format!("{local}:9124");
    let mut first = hopbox(
        &s,
        &r1,
        &[
            "run",
            "--port",
            &spec,
            "--",
            "socat TCP-LISTEN:9124,reuseaddr,fork SYSTEM:'echo pong' & sleep 6; kill $!",
        ],
    )
    .spawn()
    .unwrap();
    assert!(
        pong(local, Duration::from_secs(5)),
        "first forward never came up"
    );
    let o = hopbox(&s, &r2, &["run", "--port", &spec, "--", "true"])
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(125),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        pong(local, Duration::from_secs(1)),
        "second run broke the first forward"
    );
    assert_eq!(first.wait().unwrap().code(), Some(0));
}
