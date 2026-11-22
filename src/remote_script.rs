//! Remote paths and the scripts hopbox sends over ssh.
//!
//! Dynamic values are quoted with [`shell_quote`]; everything else is a fixed literal.
//! Send every script through [`wrap_for_ssh`] so the remote login shell (zsh on some hosts)
//! never parses hopbox syntax itself.

use crate::config::MemSize;
use crate::quote::shell_quote;

/// Printed on stderr when the workspace lock is busy.
pub const LOCK_WAIT_LINE: &str = "hopbox: waiting for lock…";
/// Printed on stderr right before the script exits 124 on lock timeout.
/// The run step matches this exact line to report `source=hopbox exit=124`.
pub const LOCK_TIMEOUT_LINE: &str = "hopbox: lock wait timed out";

/// A path on the remote host, either under the remote user's home or absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemotePath {
    /// Relative to `$HOME`; empty means `$HOME` itself.
    Home(String),
    Abs(String),
}

impl RemotePath {
    /// `~` and `~/x` become `Home`; `/x` becomes `Abs`; any other relative path is taken as
    /// relative to home, which is also what ssh does with a relative path.
    pub fn parse(root: &str) -> RemotePath {
        if root == "~" {
            RemotePath::Home(String::new())
        } else if let Some(rel) = root.strip_prefix("~/") {
            RemotePath::Home(rel.trim_end_matches('/').to_owned())
        } else if root.starts_with('/') {
            let trimmed = root.trim_end_matches('/');
            RemotePath::Abs(if trimmed.is_empty() { "/" } else { trimmed }.to_owned())
        } else {
            RemotePath::Home(root.trim_end_matches('/').to_owned())
        }
    }

    pub fn join(&self, seg: &str) -> RemotePath {
        let join = |base: &str| match base {
            "" => seg.to_owned(),
            "/" => format!("/{seg}"),
            _ => format!("{base}/{seg}"),
        };
        match self {
            RemotePath::Home(rel) => RemotePath::Home(join(rel)),
            RemotePath::Abs(path) => RemotePath::Abs(join(path)),
        }
    }

    /// The path as rsync's remote side expects it: relative paths resolve against home.
    pub fn rsync_arg(&self) -> String {
        match self {
            RemotePath::Home(rel) if rel.is_empty() => ".".to_owned(),
            RemotePath::Home(rel) => rel.clone(),
            RemotePath::Abs(path) => path.clone(),
        }
    }

    /// The path text without rendering: home-relative or absolute.
    pub fn raw(&self) -> &str {
        match self {
            RemotePath::Home(p) | RemotePath::Abs(p) => p,
        }
    }

    /// Render for `sh`: `$HOME` expands on the remote, the rest is quoted.
    pub fn render(&self) -> String {
        match self {
            RemotePath::Home(rel) if rel.is_empty() => r#""$HOME""#.to_owned(),
            RemotePath::Home(rel) => format!(r#""$HOME"/{}"#, shell_quote(rel)),
            RemotePath::Abs(path) => shell_quote(path).into_owned(),
        }
    }
}

/// A transient systemd unit name, `hopbox-<8 hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitName(String);

impl UnitName {
    pub fn random() -> UnitName {
        use std::hash::{BuildHasher, Hasher};
        // RandomState carries per-process random keys that change on every `new()`.
        let mut h = std::hash::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_u32(std::process::id());
        UnitName(format!("hopbox-{:08x}", h.finish() as u32))
    }

    #[cfg(test)]
    pub(crate) fn from_id(id: &str) -> UnitName {
        UnitName(format!("hopbox-{id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 8-hex id without the `hopbox-` prefix.
    pub fn id(&self) -> &str {
        self.0.trim_start_matches("hopbox-")
    }

    /// The full unit name. systemctl needs the `.scope` suffix: a bare name resolves to `.service`.
    pub fn scope(&self) -> String {
        format!("{}.scope", self.0)
    }
}

/// Resource caps applied through `systemd-run --user --scope`.
#[derive(Debug, Clone)]
pub struct Caps {
    pub unit: UnitName,
    pub memory_max: Option<MemSize>,
    pub cpu_weight: Option<u16>,
}

/// Printed on stdout by [`LockScript`] once it holds the workspace lock.
pub const LOCKED_LINE: &str = "locked";

/// Takes the workspace lock and holds it until its stdin closes. hopbox runs it in its own
/// ssh session before syncing, so sync, job and fetch all happen under one lock, and nothing
/// the job starts ever holds the lock fd.
#[derive(Debug)]
pub struct LockScript<'a> {
    pub workspace: &'a RemotePath,
    pub wait_secs: u64,
}

impl LockScript<'_> {
    pub fn render(&self) -> String {
        format!(
            "cd {ws} && touch .hopbox-last-used && exec 9>.hopbox.lock && \
             {{ flock -n 9 || {{ echo {wait} >&2; flock -w {secs} 9 || \
             {{ echo {timeout} >&2; exit 124; }}; }}; }} && \
             echo {locked} && exec cat >/dev/null",
            ws = self.workspace.render(),
            wait = shell_quote(LOCK_WAIT_LINE),
            secs = self.wait_secs,
            timeout = shell_quote(LOCK_TIMEOUT_LINE),
            locked = LOCKED_LINE,
        )
    }
}

/// The script that runs the user's command in the workspace (the lock is held separately).
#[derive(Debug)]
pub struct RunScript<'a> {
    pub workspace: &'a RemotePath,
    /// Path of the current subdirectory relative to the worktree root, `/`-separated; empty at the root.
    pub subdir: &'a str,
    /// `None` runs uncapped (no working `systemd-run --user` on the host).
    pub caps: Option<&'a Caps>,
    pub shell: &'a [String],
    pub cmd: &'a str,
    /// Env file in the workspace written by [`env_write_script`]: sourced, then deleted.
    pub env_file: Option<&'a str>,
}

impl RunScript<'_> {
    pub fn render(&self) -> String {
        let mut s = format!("cd {} && ", self.workspace.render());
        if let Some(f) = self.env_file {
            let f = shell_quote(f);
            s += &format!(". ./{f} && rm -f ./{f} && ");
        }
        if !self.subdir.is_empty() {
            // `./` stops a leading `-` being read as an option and keeps CDPATH out of it.
            s += &format!("cd ./{} && ", shell_quote(self.subdir));
        }
        s += "exec ";
        if let Some(caps) = self.caps {
            s += &format!(
                "systemd-run --user --scope --quiet --unit {} ",
                shell_quote(caps.unit.as_str())
            );
            if let Some(m) = &caps.memory_max {
                s += &format!(
                    "-p MemoryMax={} -p MemorySwapMax=0 ",
                    shell_quote(m.as_str())
                );
            }
            if let Some(w) = caps.cpu_weight {
                s += &format!("-p CPUWeight={w} ");
            }
            s += "-- ";
        }
        for word in self.shell {
            s += &shell_quote(word);
            s += " ";
        }
        s += &shell_quote(self.cmd);
        s
    }
}

/// Read the scope's result after the job ends, then clear the failed unit.
/// Prints `oom-kill` when the memory cap killed the job.
pub fn oom_check_script(unit: &UnitName) -> String {
    let scope = shell_quote(&unit.scope()).into_owned();
    format!(
        "systemctl --user show -p Result --value {scope}; \
         systemctl --user reset-failed {scope} 2>/dev/null; true"
    )
}

/// Stop the job's scope, killing every process in its cgroup.
pub fn stop_script(unit: &UnitName) -> String {
    format!("systemctl --user stop {}", shell_quote(&unit.scope()))
}

/// Write stdin to `<ws>/<name>` readable only by the remote user. Env values travel on
/// stdin, never on a command line.
pub fn env_write_script(ws: &RemotePath, name: &str) -> String {
    format!(
        "cd {} && umask 077 && cat > {}",
        ws.render(),
        shell_quote(name)
    )
}

/// Create the workspace and report whether `systemd-run --user --scope` works: prints `caps=1` or `caps=0`.
pub fn preflight_script(ws: &RemotePath) -> String {
    format!(
        "mkdir -p {} && if systemd-run --user --scope --quiet true >/dev/null 2>&1; \
         then echo caps=1; else echo caps=0; fi",
        ws.render()
    )
}

/// Wrap a script as one `sh -c` command line for ssh.
pub fn wrap_for_ssh(script: &str) -> String {
    format!("sh -c {}", shell_quote(script))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| w.to_string()).collect()
    }

    fn have_flock() -> bool {
        Command::new("flock")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn remote_path_tilde_root() {
        assert_eq!(
            RemotePath::parse("~/.cache/hopbox").render(),
            r#""$HOME"/.cache/hopbox"#
        );
    }

    #[test]
    fn remote_path_bare_tilde_and_relative() {
        assert_eq!(RemotePath::parse("~"), RemotePath::Home(String::new()));
        assert_eq!(RemotePath::parse("~").render(), r#""$HOME""#);
        assert_eq!(RemotePath::parse("~/").join("ws").render(), r#""$HOME"/ws"#);
        assert_eq!(RemotePath::parse("cache"), RemotePath::Home("cache".into()));
    }

    #[test]
    fn remote_path_with_space() {
        assert_eq!(
            RemotePath::parse("~/my cache").join("ws").render(),
            r#""$HOME"/'my cache/ws'"#
        );
    }

    #[test]
    fn remote_path_absolute() {
        assert_eq!(
            RemotePath::parse("/srv/hb").join("ws").render(),
            "/srv/hb/ws"
        );
        assert_eq!(
            RemotePath::parse("/srv/hb/").join("ws").render(),
            "/srv/hb/ws"
        );
        assert_eq!(RemotePath::parse("/").join("ws").render(), "/ws");
    }

    #[test]
    fn remote_path_tilde_user_is_not_home() {
        // `~other` is another user's home; never expanded, kept literal and quoted.
        assert_eq!(
            RemotePath::parse("~other/x"),
            RemotePath::Home("~other/x".into())
        );
        assert_eq!(
            RemotePath::parse("~other/x").render(),
            r#""$HOME"/'~other/x'"#
        );
    }

    #[test]
    fn unit_name_shape() {
        let u = UnitName::random();
        let id = u.as_str().strip_prefix("hopbox-").expect("prefix");
        assert_eq!(id.len(), 8);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(u.scope(), format!("{}.scope", u.as_str()));
        assert_ne!(u, UnitName::random());
    }

    #[test]
    fn run_script_capped_golden() {
        let ws = RemotePath::parse("~/.cache/hopbox")
            .join("ws")
            .join("martin-1a2b3c4d");
        let caps = Caps {
            unit: UnitName::from_id("deadbeef"),
            memory_max: Some(MemSize::parse("6G").unwrap()),
            cpu_weight: Some(50),
        };
        let shell = words(&["bash", "-lc"]);
        let script = RunScript {
            workspace: &ws,
            subdir: "",
            caps: Some(&caps),
            shell: &shell,
            cmd: "cargo nextest run",
            env_file: None,
        };
        assert_eq!(
            script.render(),
            concat!(
                r#"cd "$HOME"/.cache/hopbox/ws/martin-1a2b3c4d && "#,
                r#"exec systemd-run --user --scope --quiet --unit hopbox-deadbeef "#,
                r#"-p MemoryMax=6G -p MemorySwapMax=0 -p CPUWeight=50 -- bash -lc 'cargo nextest run'"#,
            )
        );
    }

    fn uncapped<'a>(
        ws: &'a RemotePath,
        subdir: &'a str,
        shell: &'a [String],
        cmd: &'a str,
    ) -> RunScript<'a> {
        RunScript {
            workspace: ws,
            subdir,
            caps: None,
            shell,
            cmd,
            env_file: None,
        }
    }

    #[test]
    fn run_script_uncapped_omits_systemd_run() {
        let ws = RemotePath::parse("/w");
        let shell = words(&["bash", "-lc"]);
        let s = uncapped(&ws, "", &shell, "cargo test").render();
        assert!(s.ends_with("cd /w && exec bash -lc 'cargo test'"), "{s}");
        assert!(!s.contains("systemd-run"), "{s}");
    }

    #[test]
    fn run_script_caps_without_mem_or_weight_omit_props() {
        let ws = RemotePath::parse("/w");
        let shell = words(&["bash", "-lc"]);
        let caps = Caps {
            unit: UnitName::from_id("deadbeef"),
            memory_max: None,
            cpu_weight: None,
        };
        let s = RunScript {
            caps: Some(&caps),
            ..uncapped(&ws, "", &shell, "true")
        }
        .render();
        assert!(s.contains("--unit hopbox-deadbeef -- bash -lc true"), "{s}");
        assert!(
            !s.contains("MemoryMax") && !s.contains("MemorySwapMax") && !s.contains("CPUWeight"),
            "{s}"
        );
        assert!(!s.contains("--collect"), "{s}");
    }

    #[test]
    fn run_script_subdir_root_has_no_cd() {
        let ws = RemotePath::parse("/w");
        let shell = words(&["sh", "-c"]);
        let s = uncapped(&ws, "", &shell, "true").render();
        assert_eq!(s.matches("cd ").count(), 1, "{s}");
    }

    #[test]
    fn run_script_subdir_with_space() {
        let ws = RemotePath::parse("/w");
        let shell = words(&["sh", "-c"]);
        let s = uncapped(&ws, "crates/my crate", &shell, "true").render();
        assert!(
            s.contains("cd /w && cd ./'crates/my crate' && exec sh -c true"),
            "{s}"
        );
    }

    #[test]
    fn run_script_dash_subdir_is_not_an_option() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("-x")).unwrap();
        let ws = RemotePath::Abs(dir.path().to_str().unwrap().to_owned());
        let shell = words(&["sh", "-c"]);
        let out = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(
                &uncapped(&ws, "-x", &shell, r#"basename "$PWD""#).render(),
            ))
            .env("CDPATH", dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // CDPATH must not make cd echo a path or pick another dir.
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "-x\n");
    }

    #[test]
    fn preflight_script_golden() {
        let ws = RemotePath::parse("~/my cache").join("ws");
        assert_eq!(
            preflight_script(&ws),
            r#"mkdir -p "$HOME"/'my cache/ws' && if systemd-run --user --scope --quiet true >/dev/null 2>&1; then echo caps=1; else echo caps=0; fi"#
        );
    }

    #[test]
    fn run_script_sources_and_removes_env_file() {
        let ws = RemotePath::parse("/w");
        let shell = words(&["sh", "-c"]);
        let s = RunScript {
            env_file: Some(".hopbox-env-deadbeef"),
            ..uncapped(&ws, "sub", &shell, "true")
        }
        .render();
        assert!(
            s.contains(
                "cd /w && . ./.hopbox-env-deadbeef && rm -f ./.hopbox-env-deadbeef && cd ./sub && exec "
            ),
            "{s}"
        );
        assert_eq!(
            env_write_script(&ws, ".hopbox-env-deadbeef"),
            "cd /w && umask 077 && cat > .hopbox-env-deadbeef"
        );
    }

    #[test]
    fn wrap_for_ssh_quotes_whole_script() {
        assert_eq!(wrap_for_ssh("echo x"), "sh -c 'echo x'");
    }

    #[test]
    fn oom_and_stop_scripts_use_scope_suffix() {
        let u = UnitName::from_id("deadbeef");
        assert_eq!(
            oom_check_script(&u),
            "systemctl --user show -p Result --value hopbox-deadbeef.scope; \
             systemctl --user reset-failed hopbox-deadbeef.scope 2>/dev/null; true"
        );
        assert_eq!(
            stop_script(&u),
            "systemctl --user stop hopbox-deadbeef.scope"
        );
    }

    fn run_local(script: &str) -> std::process::Output {
        Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(script))
            .output()
            .expect("run sh")
    }

    #[test]
    fn run_script_runs_in_real_sh() {
        if !have_flock() {
            eprintln!("skipping: flock not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub dir")).unwrap();
        let ws = RemotePath::Abs(dir.path().to_str().unwrap().to_owned());
        let shell = words(&["sh", "-c"]);
        let cmd = r#"printf '%s|' a 'b c' "d'e" "$(basename "$PWD")""#;
        let out = run_local(&uncapped(&ws, "sub dir", &shell, cmd).render());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "a|b c|d'e|sub dir|");
    }

    #[test]
    fn run_script_passes_exit_code_through() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ws = RemotePath::Abs(dir.path().to_str().unwrap().to_owned());
        let shell = words(&["sh", "-c"]);
        let out = run_local(&uncapped(&ws, "", &shell, "exit 42").render());
        assert_eq!(out.status.code(), Some(42));
    }

    #[test]
    fn lock_script_golden() {
        let ws = RemotePath::parse("/w");
        assert_eq!(
            LockScript {
                workspace: &ws,
                wait_secs: 30
            }
            .render(),
            concat!(
                "cd /w && touch .hopbox-last-used && exec 9>.hopbox.lock && ",
                "{ flock -n 9 || { echo 'hopbox: waiting for lock…' >&2; flock -w 30 9 || ",
                "{ echo 'hopbox: lock wait timed out' >&2; exit 124; }; }; } && ",
                "echo locked && exec cat >/dev/null",
            )
        );
    }

    fn hold_lock(lock: &Path) -> KillOnDrop {
        let holder = KillOnDrop(
            // `exec` keeps one pid holding the lock fd, so killing it frees the lock.
            Command::new("sh")
                .arg("-c")
                .arg(r#"exec 9>"$1"; flock 9; exec sleep 30"#)
                .arg("sh")
                .arg(lock)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_until_locked(lock);
        holder
    }

    fn lock_free(lock: &Path) -> bool {
        Command::new("flock")
            .arg("-n")
            .arg(lock)
            .arg("true")
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn lock_script_timeout_exits_124() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let holder = hold_lock(&dir.path().join(".hopbox.lock"));
        let ws = RemotePath::Abs(dir.path().to_str().unwrap().to_owned());
        let out = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(
                &LockScript {
                    workspace: &ws,
                    wait_secs: 0,
                }
                .render(),
            ))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        drop(holder);
        assert_eq!(out.status.code(), Some(124));
        assert!(out.stdout.is_empty(), "must not report locked");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(
            stderr.lines().collect::<Vec<_>>(),
            [LOCK_WAIT_LINE, LOCK_TIMEOUT_LINE]
        );
    }

    #[test]
    fn lock_script_holds_until_stdin_closes() {
        use std::io::{BufRead, BufReader};
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(".hopbox.lock");
        let ws = RemotePath::Abs(dir.path().to_str().unwrap().to_owned());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(
                &LockScript {
                    workspace: &ws,
                    wait_secs: 5,
                }
                .render(),
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, format!("{LOCKED_LINE}\n"));
        assert!(dir.path().join(".hopbox-last-used").exists());
        assert!(!lock_free(&lock), "lock must be held while stdin is open");
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert!(lock_free(&lock), "lock must be released when stdin closes");
    }

    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_until_locked(lock: &Path) {
        for _ in 0..200 {
            let free = Command::new("flock")
                .args(["-n"])
                .arg(lock)
                .arg("true")
                .status()
                .unwrap()
                .success();
            if !free {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("lock holder never acquired {}", lock.display());
    }
}
