mod common;

use std::process::{Command, Output, Stdio};

use assert_cmd::cargo::CommandCargoExt;

fn gc(s: &common::Sshd, args: &[&str]) -> Output {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cfg/hopbox")).unwrap();
    std::fs::write(
        tmp.path().join("cfg/hopbox/config.toml"),
        format!(
            "default_host = \"test\"\n[hosts.test]\nssh = \"{}\"\n",
            s.host
        ),
    )
    .unwrap();
    Command::cargo_bin("hopbox")
        .unwrap()
        .current_dir(tmp.path())
        .arg("gc")
        .args(args)
        .env("XDG_CONFIG_HOME", tmp.path().join("cfg"))
        .env("XDG_RUNTIME_DIR", tmp.path().join("run"))
        .env("HOPBOX_SSH_CONFIG", &s.ssh_config)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn make_workspaces(s: &common::Sshd) {
    let o = s.ssh(
        "cd && mkdir -p .cache/hopbox/ws/old-1 .cache/hopbox/ws/new-1 && \
         touch -d '30 days ago' .cache/hopbox/ws/old-1/.hopbox-last-used && \
         touch .cache/hopbox/ws/new-1/.hopbox-last-used",
    );
    assert!(o.status.success());
}

fn remaining(s: &common::Sshd) -> String {
    String::from_utf8(s.ssh("ls ~/.cache/hopbox/ws").stdout).unwrap()
}

#[test]
fn gc_removes_old_keeps_recent() {
    let Some(s) = common::sshd() else { return };
    make_workspaces(&s);
    let o = gc(&s, &[]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "removed old-1\n");
    assert_eq!(remaining(&s), "new-1\n");
}

#[test]
fn gc_dry_run_deletes_nothing() {
    let Some(s) = common::sshd() else { return };
    make_workspaces(&s);
    let o = gc(&s, &["--dry-run", "--older-than", "7d"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "would remove old-1 (unused 30d)\n"
    );
    assert_eq!(remaining(&s), "new-1\nold-1\n");
}

#[test]
fn gc_skips_locked() {
    let Some(s) = common::sshd() else { return };
    make_workspaces(&s);
    let held = s.ssh(
        "mkdir -p ~/.cache/hopbox/locks; nohup flock ~/.cache/hopbox/locks/old-1.lock sleep 60 </dev/null >/dev/null 2>&1 & sleep 0.5",
    );
    assert!(held.status.success());
    let o = gc(&s, &[]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "skipped old-1: in use\n"
    );
    assert_eq!(remaining(&s), "new-1\nold-1\n");
}

#[test]
fn gc_with_no_workspaces() {
    let Some(s) = common::sshd() else { return };
    let o = gc(&s, &[]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "nothing to remove\n");
}

#[test]
fn gc_bad_age_is_125() {
    let Some(s) = common::sshd() else { return };
    let o = gc(&s, &["--older-than", "soon"]);
    assert_eq!(o.status.code(), Some(125));
}
