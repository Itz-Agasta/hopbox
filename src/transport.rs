//! ssh with a shared ControlMaster connection per host.
//!
//! BatchMode is always on: hopbox never answers a password or host-key prompt.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};

use crate::quote::shell_quote;
use crate::remote_script::{RemotePath, preflight_script, wrap_for_ssh};

/// Internal hook: an ssh config file passed as `-F`. Used by the integration tests.
pub const SSH_CONFIG_ENV: &str = "HOPBOX_SSH_CONFIG";

#[derive(Debug, Clone)]
pub struct Transport {
    dest: String,
    ssh_config: Option<PathBuf>,
    control_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("cannot run ssh: {0}")]
    Spawn(#[from] io::Error),
    #[error("cannot reach {dest}: {stderr}")]
    Unreachable { dest: String, stderr: String },
    #[error("preflight on {dest} failed: {stderr}")]
    Preflight { dest: String, stderr: String },
}

/// What the preflight learned about the remote.
#[derive(Debug, PartialEq, Eq)]
pub struct Preflight {
    /// `systemd-run --user --scope` works, so jobs can run under caps.
    pub caps: bool,
}

impl Transport {
    /// Transport with the default control directory and the optional `HOPBOX_SSH_CONFIG`.
    pub fn new(dest: &str) -> Result<Transport, TransportError> {
        let ssh_config = std::env::var_os(SSH_CONFIG_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Transport::with_config(dest, ssh_config, default_control_dir())
    }

    pub fn with_config(
        dest: &str,
        ssh_config: Option<PathBuf>,
        control_dir: PathBuf,
    ) -> Result<Transport, TransportError> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&control_dir)?;
        // `mode` only applies to newly created dirs; tighten an existing one too.
        std::fs::set_permissions(&control_dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Transport {
            dest: dest.to_owned(),
            ssh_config,
            control_dir,
        })
    }

    pub fn dest(&self) -> &str {
        &self.dest
    }

    fn base_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(cfg) = &self.ssh_config {
            args.push("-F".into());
            args.push(cfg.into());
        }
        let mut control_path = OsString::from("ControlPath=");
        control_path.push(self.control_dir.join("%C"));
        for opt in [
            OsString::from("ControlMaster=auto"),
            control_path,
            "ControlPersist=5m".into(),
            "BatchMode=yes".into(),
            "ConnectTimeout=10".into(),
        ] {
            args.push("-o".into());
            args.push(opt);
        }
        args
    }

    fn command(&self, script: &str, tty: bool) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.args(self.base_args())
            .arg(if tty { "-tt" } else { "-T" })
            .arg("--")
            .arg(&self.dest)
            .arg(wrap_for_ssh(script));
        cmd
    }

    /// Run a script and collect its output. stdin is closed.
    pub fn capture(&self, script: &str) -> Result<Output, TransportError> {
        Ok(self.command(script, false).stdin(Stdio::null()).output()?)
    }

    /// Start a script with stdin and stdout piped and stderr inherited (the lock holder).
    pub fn spawn_holder(&self, script: &str) -> Result<Child, TransportError> {
        Ok(self
            .command(script, false)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?)
    }

    /// Start a script with stdout and stderr piped and stdin inherited.
    pub fn spawn(&self, script: &str, tty: bool) -> Result<Child, TransportError> {
        Ok(self
            .command(script, tty)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?)
    }

    /// The `-e` value for rsync, so it rides the same ControlMaster connection.
    pub fn rsync_rsh(&self) -> String {
        let mut rsh = String::from("ssh");
        for arg in self.base_args() {
            rsh.push(' ');
            rsh.push_str(&shell_quote(&arg.to_string_lossy()));
        }
        rsh
    }

    /// Open the connection, create the workspace, and probe for resource caps.
    pub fn preflight(&self, ws: &RemotePath) -> Result<Preflight, TransportError> {
        let out = self.capture(&preflight_script(ws))?;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        match out.status.code() {
            Some(0) => {}
            Some(255) => {
                return Err(TransportError::Unreachable {
                    dest: self.dest.clone(),
                    stderr,
                });
            }
            _ => {
                return Err(TransportError::Preflight {
                    dest: self.dest.clone(),
                    stderr,
                });
            }
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(Preflight {
            caps: stdout.lines().any(|l| l.trim() == "caps=1"),
        })
    }
}

/// `$XDG_RUNTIME_DIR/hopbox`, else `$HOME/.cache/hopbox/cm`, else a per-user dir in /tmp.
fn default_control_dir() -> PathBuf {
    let non_empty = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(run) = non_empty("XDG_RUNTIME_DIR") {
        return run.join("hopbox");
    }
    if let Some(home) = non_empty("HOME") {
        return home.join(".cache/hopbox/cm");
    }
    std::env::temp_dir().join(format!("hopbox-cm-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn base_args_contain_connection_opts() {
        let d = tempfile::tempdir().unwrap();
        let t = Transport::with_config("h", None, d.path().join("cm")).unwrap();
        let args = strings(t.base_args());
        let cp = format!("ControlPath={}/%C", d.path().join("cm").display());
        let want = [
            "-o",
            "ControlMaster=auto",
            "-o",
            cp.as_str(),
            "-o",
            "ControlPersist=5m",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
        ];
        assert_eq!(args, want);

        let t = Transport::with_config("h", Some("/c fg".into()), d.path().join("cm")).unwrap();
        let args = strings(t.base_args());
        assert_eq!(&args[..2], ["-F", "/c fg"]);
    }

    #[test]
    fn control_dir_is_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let cm = d.path().join("a/cm");
        Transport::with_config("h", None, cm.clone()).unwrap();
        let mode = std::fs::metadata(&cm).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn rsync_rsh_quotes_paths() {
        let d = tempfile::tempdir().unwrap();
        let t = Transport::with_config("h", Some("/c fg".into()), d.path().join("cm")).unwrap();
        let rsh = t.rsync_rsh();
        assert!(
            rsh.starts_with("ssh -F '/c fg' -o ControlMaster=auto "),
            "{rsh}"
        );
        assert!(rsh.ends_with("-o ConnectTimeout=10"), "{rsh}");
    }
}
