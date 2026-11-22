mod common;

#[test]
fn container_accepts_ssh() {
    let Some(s) = common::sshd() else { return };
    let out = s.ssh("id -u; command -v rsync flock bash");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    for want in ["1001", "rsync", "flock", "bash"] {
        assert!(stdout.contains(want), "missing {want} in {stdout}");
    }
}
