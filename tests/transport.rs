mod common;

use std::time::{Duration, Instant};

use hopbox::remote_script::RemotePath;
use hopbox::transport::{Transport, TransportError};

fn transport(s: &common::Sshd) -> Transport {
    Transport::with_config(s.host, Some(s.ssh_config.clone()), s.dir.path().join("cm")).unwrap()
}

#[test]
fn capture_runs_remote_script() {
    let Some(s) = common::sshd() else { return };
    let out = transport(&s).capture("echo hello; echo err >&2").unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "hello\n");
    assert_eq!(String::from_utf8(out.stderr).unwrap(), "err\n");
}

#[test]
fn preflight_reports_no_caps_in_container() {
    let Some(s) = common::sshd() else { return };
    let ws = RemotePath::parse("~/.cache/hopbox").join("ws").join("r-1");
    let p = transport(&s).preflight(&ws).unwrap();
    assert!(!p.caps);
    assert!(s.ssh("test -d ~/.cache/hopbox/ws/r-1").status.success());
}

#[test]
fn unreachable_host_is_reported() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("ssh_config");
    std::fs::write(
        &cfg,
        "Host nowhere\n  HostName 127.0.0.1\n  Port 1\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n",
    )
    .unwrap();
    let t = Transport::with_config("nowhere", Some(cfg), d.path().join("cm")).unwrap();
    let start = Instant::now();
    let err = t.preflight(&RemotePath::parse("~/x")).unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(15));
    match err {
        TransportError::Unreachable { dest, stderr } => {
            assert_eq!(dest, "nowhere");
            assert!(!stderr.is_empty());
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

/// Paths with spaces (a control dir, a known_hosts file) must reach ssh as one value each.
#[test]
fn paths_with_spaces_work() {
    let Some(s) = common::sshd() else { return };
    let known = s.dir.path().join("k h").join("known_hosts");
    std::fs::create_dir_all(known.parent().unwrap()).unwrap();
    let t = Transport::with_config(s.host, Some(s.ssh_config.clone()), s.dir.path().join("c m"))
        .unwrap()
        .with_options(
            None,
            &[
                "StrictHostKeyChecking=accept-new".to_owned(),
                hopbox::transport::ssh_path_option("UserKnownHostsFile", &known),
            ],
        );
    let out = t.capture("echo ok").unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let k = std::fs::read_to_string(&known).unwrap_or_default();
    assert!(
        k.contains("127.0.0.1"),
        "host key must land in {}",
        known.display()
    );
}
