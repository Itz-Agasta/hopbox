mod common;

use std::process::{Command, Output, Stdio};

use assert_cmd::cargo::CommandCargoExt;

fn doctor(s: &common::Sshd, config: &str, ssh_config: &std::path::Path, args: &[&str]) -> Output {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cfg/hopbox")).unwrap();
    std::fs::write(tmp.path().join("cfg/hopbox/config.toml"), config).unwrap();
    let _ = s;
    Command::cargo_bin("hopbox")
        .unwrap()
        .current_dir(tmp.path())
        .arg("doctor")
        .args(args)
        .env("XDG_CONFIG_HOME", tmp.path().join("cfg"))
        .env("XDG_RUNTIME_DIR", tmp.path().join("run"))
        .env("HOPBOX_SSH_CONFIG", ssh_config)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn doctor_on_container() {
    let Some(s) = common::sshd() else { return };
    let cfg = format!(
        "default_host = \"test\"\n[hosts.test]\nssh = \"{}\"\ncost = \"free\"\n",
        s.host
    );
    let o = doctor(&s, &cfg, &s.ssh_config, &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{out}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    for row in [
        "ok    ssh",
        "ok    rsync",
        "ok    bash",
        "ok    flock",
        "warn  caps",
        "info  arch",
        "info  memory",
        "info  disk",
        "info  path",
        "info  cost      free",
    ] {
        assert!(
            out.lines().any(|l| l.starts_with(row)),
            "missing {row:?} in:\n{out}"
        );
    }
}

#[test]
fn doctor_names_host_argument() {
    let Some(s) = common::sshd() else { return };
    let cfg = format!("[hosts.other]\nssh = \"{}\"\n", s.host);
    let o = doctor(&s, &cfg, &s.ssh_config, &["other"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with("hopbox doctor: other (ssh hopbox-test)")
    );
}

#[test]
fn doctor_unknown_host_125() {
    let Some(s) = common::sshd() else { return };
    let o = doctor(&s, "[hosts.a]\n", &s.ssh_config, &["nope"]);
    assert_eq!(o.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&o.stderr).contains("nope"));
}

#[test]
fn doctor_unreachable_255() {
    let Some(s) = common::sshd() else { return };
    let d = tempfile::tempdir().unwrap();
    let bad = d.path().join("ssh_config");
    std::fs::write(&bad, "Host nowhere\n  HostName 127.0.0.1\n  Port 1\n").unwrap();
    let o = doctor(&s, "[hosts.gone]\nssh = \"nowhere\"\n", &bad, &["gone"]);
    assert_eq!(o.status.code(), Some(255));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot reach"));
}
