//! Throwaway sshd container for integration tests. Never touches a real host.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub const IMAGE: &str = "hopbox-test-sshd";
pub const HOST: &str = "hopbox-test";

pub struct Sshd {
    pub port: u16,
    pub ssh_config: PathBuf,
    pub host: &'static str,
    pub dir: TempDir,
    container: String,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn docker_required() -> bool {
    std::env::var("HOPBOX_REQUIRE_DOCKER").as_deref() == Ok("1")
}

fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn build_image() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sshd");
        let out = Command::new("docker")
            .args(["build", "-q", "-t", IMAGE])
            .arg(&dir)
            .output()
            .expect("run docker build");
        assert!(
            out.status.success(),
            "docker build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Start a fresh sshd container. `None` when docker is missing and not required.
pub fn sshd() -> Option<Sshd> {
    if !docker_available() {
        assert!(
            !docker_required(),
            "HOPBOX_REQUIRE_DOCKER=1 but docker is unavailable"
        );
        eprintln!("skipping: docker is not available");
        return None;
    }
    build_image();

    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key));
    let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();

    let container = run(Command::new("docker").args([
        "run",
        "-d",
        "--rm",
        "-p",
        "127.0.0.1::22",
        "-e",
        &format!("AUTHORIZED_KEY={}", pubkey.trim()),
        IMAGE,
    ]));
    let port_line = run(Command::new("docker").args(["port", &container, "22/tcp"]));
    let port: u16 = port_line
        .lines()
        .next()
        .and_then(|l| l.rsplit(':').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("cannot parse docker port output {port_line:?}"));

    let ssh_config = dir.path().join("ssh_config");
    std::fs::write(
        &ssh_config,
        format!(
            "Host {HOST}\n  HostName 127.0.0.1\n  Port {port}\n  User hopbox\n  IdentityFile {}\n  \
             IdentitiesOnly yes\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n  LogLevel ERROR\n",
            key.display()
        ),
    )
    .unwrap();

    let sshd = Sshd {
        port,
        ssh_config,
        host: HOST,
        dir,
        container,
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if sshd.ssh("true").status.success() {
            return Some(sshd);
        }
        assert!(
            Instant::now() < deadline,
            "sshd container never accepted ssh"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl Sshd {
    /// Run a shell command in the container as user `hopbox`.
    pub fn ssh(&self, script: &str) -> Output {
        Command::new("ssh")
            .arg("-F")
            .arg(&self.ssh_config)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-T",
                HOST,
                script,
            ])
            .stdin(Stdio::null())
            .output()
            .expect("spawn ssh")
    }
}
