//! `up`/`ls`/`down` against a fake `vastai` on PATH. Never runs the real CLI.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;

/// A fake `vastai` that logs its args and answers `search` with the recorded fixture.
fn fake_vastai(dir: &Path) {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/vast/search-4090.json"
    );
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n  search) cat '{fixture}' ;;\n  show) echo '[]' ;;\n  *) echo 'unexpected' >&2; exit 1 ;;\nesac\n",
        log = dir.join("calls.log").display(),
    );
    let p = dir.join("vastai");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn hopbox(dir: &Path, args: &[&str]) -> std::process::Output {
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
    Command::cargo_bin("hopbox")
        .unwrap()
        .args(args)
        .env("PATH", path)
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CONFIG_HOME", dir.join("cfg"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn up_without_yes_spend_and_no_tty_rents_nothing() {
    let d = tempfile::tempdir().unwrap();
    fake_vastai(d.path());
    let o = hopbox(
        d.path(),
        &[
            "up", "vast", "--gpu", "RTX_4090", "--ttl", "1h", "--name", "gpu1",
        ],
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(125), "{stderr}");
    assert!(
        stderr.contains("--yes-spend") && stderr.contains("$0.45/h"),
        "{stderr}"
    );
    let calls = std::fs::read_to_string(d.path().join("calls.log")).unwrap();
    assert!(calls.starts_with("search offers"), "{calls}");
    assert!(!calls.contains("create"), "{calls}");
}

#[test]
fn up_requires_ttl() {
    let d = tempfile::tempdir().unwrap();
    fake_vastai(d.path());
    let o = hopbox(d.path(), &["up", "vast", "--gpu", "RTX_4090"]);
    assert_eq!(o.status.code(), Some(125));
    assert!(
        !d.path().join("calls.log").exists(),
        "nothing called without a TTL"
    );
}

#[test]
fn ls_with_no_instances() {
    let d = tempfile::tempdir().unwrap();
    fake_vastai(d.path());
    let o = hopbox(d.path(), &["ls"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "no hopbox instances\n");
}

#[test]
fn down_unknown_name_125() {
    let d = tempfile::tempdir().unwrap();
    fake_vastai(d.path());
    let o = hopbox(d.path(), &["down", "nope"]);
    assert_eq!(o.status.code(), Some(125));
}
