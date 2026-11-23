mod common;

use common::{hopbox, repo};

/// A rented-host state entry pointing at the test container is usable as `--host`.
#[test]
fn state_host_runs_jobs() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let cfg = std::fs::read_to_string(&s.ssh_config).unwrap();
    let key = cfg
        .lines()
        .find_map(|l| l.trim().strip_prefix("IdentityFile "))
        .unwrap()
        .to_owned();
    // Only the key comes from ssh config; host, port, user and host-key policy come from state.
    let own = s.dir.path().join("rented_ssh_config");
    std::fs::write(
        &own,
        format!("Host 127.0.0.1\n  IdentityFile {key}\n  IdentitiesOnly yes\n  LogLevel ERROR\n"),
    )
    .unwrap();
    std::fs::create_dir_all(r.state.join("hopbox")).unwrap();
    std::fs::write(
        r.state.join("hopbox/instances.toml"),
        format!(
            "[[instance]]\nname = \"box\"\nprovider = \"vast\"\nid = 1\nssh_user = \"hopbox\"\n\
             ssh_host = \"127.0.0.1\"\nssh_port = {}\ndeadline = 4000000000\ndph = 0.5\n",
            s.port
        ),
    )
    .unwrap();
    let o = hopbox(&s, &r, &["run", "--host", "box", "--", "echo hi"])
        .env("HOPBOX_SSH_CONFIG", &own)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "hi\n",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(0));
    let known = std::fs::read_to_string(r.state.join("hopbox/known_hosts")).unwrap();
    assert!(
        known.contains("127.0.0.1"),
        "host key goes to hopbox's known_hosts"
    );
}
