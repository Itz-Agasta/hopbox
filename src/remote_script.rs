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
/// the job starts ever holds the lock fd. The lock file lives in `locks`, outside the
/// workspace, so `gc` can delete a workspace without unlinking a lock someone else may take.
#[derive(Debug)]
pub struct LockScript<'a> {
    pub workspace: &'a RemotePath,
    /// `<workspace_root>/locks`; the lock file is `<locks>/<name>.lock`.
    pub locks: &'a RemotePath,
    pub name: &'a str,
    pub wait_secs: u64,
    /// For `run --bg`: once stdin sends `go`, reserve the job dir, start the wrapper detached
    /// (it inherits the lock fd, so the lock stays held until the job ends) and print `started`.
    pub bg: Option<BgStart<'a>>,
}

/// What [`LockScript`] needs to start a background job.
#[derive(Debug, Clone, Copy)]
pub struct BgStart<'a> {
    /// `<workspace_root>/jobs`; the job dir is `<jobs>/<id>`.
    pub jobs: &'a RemotePath,
    pub id: &'a str,
    /// `key=value` lines for the job's `meta` file (`start=<epoch>` is appended).
    pub meta: &'a [String],
    /// From [`bg_wrapper_script`].
    pub wrapper: &'a str,
}

impl LockScript<'_> {
    pub fn render(&self) -> String {
        format!(
            "mkdir -p {locks} && exec 9>{lock} && \
             {{ flock -n 9 || {{ echo {wait} >&2; flock -w {secs} 9 || \
             {{ echo {timeout} >&2; exit 124; }}; }}; }} && \
             mkdir -p {ws} && cd {ws} && touch .hopbox-last-used && \
             echo {locked} && {tail}",
            locks = self.locks.render(),
            lock = self.locks.join(&format!("{}.lock", self.name)).render(),
            ws = self.workspace.render(),
            wait = shell_quote(LOCK_WAIT_LINE),
            secs = self.wait_secs,
            timeout = shell_quote(LOCK_TIMEOUT_LINE),
            locked = LOCKED_LINE,
            tail = match self.bg {
                None => "exec cat >/dev/null".to_owned(),
                Some(bg) => bg.render(),
            },
        )
    }
}

impl BgStart<'_> {
    /// The lock script's tail: plain `mkdir` reserves the job dir (an existing one is never
    /// reused), and `meta` and `pid` are written before `started` is printed, so a printed id
    /// always has a job dir that `wait`, `logs` and `kill` can use.
    fn render(&self) -> String {
        let jd = self.jobs.join(self.id).render();
        let meta: Vec<String> = self
            .meta
            .iter()
            .map(|m| shell_quote(m).into_owned())
            .collect();
        format!(
            "IFS= read -r go && [ \"$go\" = go ] && mkdir -p {jobs} && mkdir {jd} && \
             {{ printf '%s\\n' {meta}; echo \"start=$(date +%s)\"; }} > {jd}/meta && \
             {{ setsid nohup sh -c {wrapper} </dev/null >/dev/null 2>&1 & \
             echo $! > {jd}/pid && \
             echo \"$(cat /proc/sys/kernel/random/boot_id) $(awk '{{print $22}}' /proc/$!/stat)\" \
             > {jd}/ident && echo {BG_STARTED_LINE}; }}",
            jobs = self.jobs.render(),
            meta = meta.join(" "),
            wrapper = shell_quote(self.wrapper),
        )
    }
}

/// Printed by [`LockScript`] after it starts a background job.
pub const BG_STARTED_LINE: &str = "started";

/// The detached wrapper of a background job (its pid is also its process group, thanks to
/// `setsid`): runs `run` from `<job_dir>` with the lock fd closed and its output in `log`,
/// then writes `exit` atomically (137 plus an `oom` file when the memory cap killed it).
/// It survives a TERM to its process group, so a killed job still gets an `exit` file.
pub fn bg_wrapper_script(job_dir: &RemotePath, run: &str, unit: Option<&UnitName>) -> String {
    let mut s = format!(
        "trap true TERM; cd {jd} && {{ ( {run} ) 9>&- >log 2>&1; rc=$?; ",
        jd = job_dir.render(),
    );
    if let Some(u) = unit {
        let scope = shell_quote(&u.scope()).into_owned();
        s += &format!(
            "r=$(systemctl --user show -p Result --value {scope} 2>/dev/null); \
             systemctl --user reset-failed {scope} 2>/dev/null; \
             if [ \"$r\" = oom-kill ]; then rc=137; touch oom; fi; "
        );
    }
    s += "echo $rc > exit.tmp && mv exit.tmp exit; }";
    s
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
            // Resolve `rm` first: the sourced env may replace PATH.
            s += &format!(
                "__hopbox_rm=$(command -v rm) && . ./{f} && \"$__hopbox_rm\" -f ./{f} && "
            );
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

/// Remove an env file that was written but never used.
pub fn env_remove_script(ws: &RemotePath, name: &str) -> String {
    format!("rm -f {}/{}", ws.render(), shell_quote(name))
}

/// Create the workspace and report whether `systemd-run --user --scope` works: prints `caps=1` or `caps=0`.
pub fn preflight_script(ws: &RemotePath) -> String {
    format!(
        "mkdir -p {} && if systemd-run --user --scope --quiet true >/dev/null 2>&1; \
         then echo caps=1; else echo caps=0; fi",
        ws.render()
    )
}

/// Print what `hopbox doctor` checks as `key=value` lines. Creates nothing on the remote.
pub fn doctor_script(root: &RemotePath, shell: &[String]) -> String {
    let shell_cmd: Vec<String> = shell.iter().map(|w| shell_quote(w).into_owned()).collect();
    format!(
        concat!(
            "echo \"arch=$(uname -m)\"; ",
            "echo \"rsync=$(rsync --version 2>/dev/null | head -n 1)\"; ",
            "echo \"bash=$(command -v bash)\"; ",
            "echo \"flock=$(flock --version 2>/dev/null | head -n 1)\"; ",
            "if systemd-run --user --scope --quiet true >/dev/null 2>&1; ",
            "then echo caps=1; else echo caps=0; fi; ",
            "echo \"mem=$(awk '/^MemAvailable:/{{a=$2}} /^MemTotal:/{{t=$2}} END{{print a, t}}' ",
            "/proc/meminfo 2>/dev/null)\"; ",
            // The workspace root may not exist yet: measure its nearest existing ancestor.
            "d={root}; while [ ! -d \"$d\" ]; do d=$(dirname \"$d\"); done; ",
            "echo \"disk=$(df -Pk \"$d\" | awk 'NR==2{{print $4}}')\"; ",
            "echo \"path=$({shell} 'printf %s \"$PATH\"' </dev/null 2>/dev/null)\"",
        ),
        root = root.render(),
        shell = shell_cmd.join(" "),
    )
}

/// List workspaces: first line `now=<epoch>`, then `<epoch> <name>` per workspace, using the
/// mtime of `.hopbox-last-used` (else the directory).
pub fn gc_list_script(root: &RemotePath) -> String {
    format!(
        concat!(
            r#"echo "now=$(date +%s)"; cd {ws} 2>/dev/null || exit 0; "#,
            r#"for d in */ .[!.]*/ ..?*/; do [ -d "$d" ] || continue; d=${{d%/}}; "#,
            r#"t=$(stat -c %Y -- "$d/.hopbox-last-used" 2>/dev/null || stat -c %Y -- "$d"); "#,
            r#"echo "$t $d"; done"#,
        ),
        ws = root.join("ws").render()
    )
}

/// Delete each named workspace while holding its lock, unless it was used after `cutoff`
/// (remote epoch seconds). Prints one line per name: `removed`, `busy`, `fresh` or
/// `failed <name> <rc>`. Lock files are never deleted.
pub fn gc_delete_script(root: &RemotePath, names: &[String], cutoff: u64) -> String {
    let quoted: Vec<String> = names.iter().map(|n| shell_quote(n).into_owned()).collect();
    let locks = root.join("locks").render();
    format!(
        concat!(
            r#"cd {ws} || exit 0; mkdir -p {locks} || exit 1; "#,
            r#"for n in {names}; do ( exec 8>{locks}/"$n.lock" && "#,
            r#"{{ flock -n 8 || exit 75; }} && "#,
            r#"t=$(stat -c %Y -- "$n/.hopbox-last-used" 2>/dev/null || stat -c %Y -- "$n") && "#,
            r#"{{ [ "$t" -le {cutoff} ] || exit 76; }} && rm -rf -- "$n" ); rc=$?; "#,
            r#"case $rc in 0) echo "removed $n";; 75) echo "busy $n";; 76) echo "fresh $n";; "#,
            r#"*) echo "failed $n $rc";; esac; done"#,
        ),
        ws = root.join("ws").render(),
        locks = locks,
        names = quoted.join(" "),
        cutoff = cutoff,
    )
}

/// Exit code of the job scripts below when `<root>/jobs/<id>` does not exist.
pub const NO_JOB_EXIT: i32 = 66;

/// Shell function `alive DIR`: true only when the job's recorded pid is still the same
/// process (same boot, same start time), so a reused pid is never mistaken for the job.
const ALIVE_FN: &str = concat!(
    r#"alive() { s=$(awk '{print $22}' "/proc/$(cat "$1/pid" 2>/dev/null)/stat" 2>/dev/null); "#,
    r#"[ -n "$s" ] && [ "$(cat "$1/ident" 2>/dev/null)" = "$(cat /proc/sys/kernel/random/boot_id) $s" ]; }; "#,
);

fn job_dir_cd(root: &RemotePath, id: &str) -> String {
    format!(
        "cd {} 2>/dev/null || exit {NO_JOB_EXIT}; ",
        root.join("jobs").join(id).render()
    )
}

/// One line `now=<epoch>`, then per job `<id>\t<state>\t<exit or ->\t<start>\t<cmd>`, where
/// state is `done` (exit file), `running` (pid alive) or `lost`.
pub fn jobs_list_script(root: &RemotePath) -> String {
    format!(
        concat!(
            "{alive}",
            r#"echo "now=$(date +%s)"; cd {jobs} 2>/dev/null || exit 0; "#,
            r#"for d in */; do id=${{d%/}}; [ -f "$id/meta" ] || continue; "#,
            r#"if [ -f "$id/exit" ]; then st=done; ex=$(cat "$id/exit"); "#,
            r#"elif alive "$id"; then st=running; ex=-; "#,
            r#"else st=lost; ex=-; fi; "#,
            r#"printf '%s\t%s\t%s\t%s\t%s\n' "$id" "$st" "$ex" "$(sed -n 's/^start=//p' "$id/meta")" "#,
            r#""$(sed -n 's/^cmd=//p' "$id/meta")"; done"#,
        ),
        jobs = root.join("jobs").render(),
        alive = ALIVE_FN,
    )
}

/// Print the job's log; with `follow`, keep printing until the job ends.
pub fn job_logs_script(root: &RemotePath, id: &str, follow: bool) -> String {
    let mut s = job_dir_cd(root, id);
    if follow {
        // Follow while the job's own process (checked by identity, not bare pid) is alive,
        // then give tail a second to print the last lines. `--retry`: right after
        // `run --bg` the wrapper may not have created `log` yet.
        s += ALIVE_FN;
        s += concat!(
            r#"if [ -f exit ] || ! alive .; then cat log 2>/dev/null; exit 0; fi; "#,
            r#"tail --retry -n +1 -f log 2>/dev/null & t=$!; "#,
            r#"while [ ! -e exit ] && alive .; do sleep 1; done; sleep 1; "#,
            r#"kill $t 2>/dev/null; wait $t 2>/dev/null; exit 0"#,
        );
    } else {
        s += "cat log";
    }
    s
}

/// Wait for the job to end. Prints its exit code (plus a line `oom` when the memory cap
/// killed it), or `lost` when the wrapper died without writing one.
pub fn job_wait_script(root: &RemotePath, id: &str) -> String {
    job_dir_cd(root, id)
        + ALIVE_FN
        + concat!(
            r#"while [ ! -e exit ]; do alive . || break; sleep 1; done; "#,
            r#"if [ -e exit ]; then cat exit; if [ -e oom ]; then echo oom; fi; else echo lost; fi"#,
        )
}

/// Stop the job: its scope when capped, and its whole process group (no `--`: dash's kill
/// rejects it). Prints `done` when the job had already finished and `lost` when its process
/// is gone; a reused pid is never signalled.
pub fn job_kill_script(root: &RemotePath, id: &str) -> String {
    let scope = shell_quote(&format!("hopbox-{id}.scope")).into_owned();
    job_dir_cd(root, id)
        + ALIVE_FN
        + &format!(
            concat!(
                r#"if [ -e exit ]; then echo done; exit 0; fi; "#,
                r#"if ! alive .; then echo lost; exit 0; fi; "#,
                r#"systemctl --user stop {scope} 2>/dev/null; "#,
                r#"kill -TERM -"$(cat pid)" 2>/dev/null; true"#,
            ),
            scope = scope
        )
}

/// Remove finished job dirs whose exit file is older than `cutoff` (remote epoch seconds).
/// Prints `job <id>` per match (`failed job <id>` and exit 1 when rm fails); deletes only
/// when `delete` is true.
pub fn gc_jobs_script(root: &RemotePath, cutoff: u64, delete: bool) -> String {
    format!(
        concat!(
            r#"cd {jobs} 2>/dev/null || exit 0; rc=0; for d in */; do id=${{d%/}}; "#,
            r#"[ -f "$id/exit" ] || continue; t=$(stat -c %Y -- "$id/exit") || continue; "#,
            r#"[ "$t" -le {cutoff} ] || continue; {act}; done; exit $rc"#,
        ),
        jobs = root.join("jobs").render(),
        cutoff = cutoff,
        act = if delete {
            r#"if rm -rf -- "$id"; then echo "job $id"; else echo "failed job $id"; rc=1; fi"#
        } else {
            r#"echo "job $id""#
        },
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
            s.contains(concat!(
                r#"cd /w && __hopbox_rm=$(command -v rm) && . ./.hopbox-env-deadbeef && "#,
                r#""$__hopbox_rm" -f ./.hopbox-env-deadbeef && cd ./sub && exec "#
            )),
            "{s}"
        );
        assert_eq!(
            env_write_script(&ws, ".hopbox-env-deadbeef"),
            "cd /w && umask 077 && cat > .hopbox-env-deadbeef"
        );
    }

    #[test]
    fn doctor_script_runs_locally() {
        let dir = tempfile::tempdir().unwrap();
        let root = RemotePath::Abs(dir.path().join("not/yet").to_str().unwrap().to_owned());
        let out = run_local(&doctor_script(&root, &words(&["sh", "-c"])));
        let text = String::from_utf8(out.stdout).unwrap();
        for key in [
            "arch=", "rsync=", "bash=", "flock=", "caps=", "mem=", "disk=", "path=",
        ] {
            assert!(
                text.lines().any(|l| l.starts_with(key)),
                "missing {key} in {text}"
            );
        }
        let disk = text.lines().find_map(|l| l.strip_prefix("disk=")).unwrap();
        assert!(disk.parse::<u64>().is_ok(), "disk={disk}");
        assert!(
            !dir.path().join("not").exists(),
            "doctor must not create the root"
        );
    }

    #[test]
    fn gc_scripts_golden() {
        let root = RemotePath::parse("~/.cache/hopbox");
        assert_eq!(
            gc_list_script(&root),
            concat!(
                r#"echo "now=$(date +%s)"; cd "$HOME"/.cache/hopbox/ws 2>/dev/null || exit 0; "#,
                r#"for d in */ .[!.]*/ ..?*/; do [ -d "$d" ] || continue; d=${d%/}; "#,
                r#"t=$(stat -c %Y -- "$d/.hopbox-last-used" 2>/dev/null || stat -c %Y -- "$d"); "#,
                r#"echo "$t $d"; done"#,
            )
        );
        assert_eq!(
            gc_delete_script(&root, &["old-1".into(), "my repo-2".into()], 1000),
            concat!(
                r#"cd "$HOME"/.cache/hopbox/ws || exit 0; mkdir -p "$HOME"/.cache/hopbox/locks || exit 1; "#,
                r#"for n in old-1 'my repo-2'; do ( exec 8>"$HOME"/.cache/hopbox/locks/"$n.lock" && "#,
                r#"{ flock -n 8 || exit 75; } && "#,
                r#"t=$(stat -c %Y -- "$n/.hopbox-last-used" 2>/dev/null || stat -c %Y -- "$n") && "#,
                r#"{ [ "$t" -le 1000 ] || exit 76; } && rm -rf -- "$n" ); rc=$?; "#,
                r#"case $rc in 0) echo "removed $n";; 75) echo "busy $n";; 76) echo "fresh $n";; "#,
                r#"*) echo "failed $n $rc";; esac; done"#,
            )
        );
    }

    #[test]
    fn gc_list_sees_every_name() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a", ".b", "..c", "-d"] {
            std::fs::create_dir_all(dir.path().join("ws").join(n)).unwrap();
        }
        let out = run_local(&gc_list_script(&gc_root(dir.path())));
        let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .map(|l| l.split_once(' ').unwrap().1.to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["-d", "..c", ".b", "a"]);
    }

    #[test]
    fn gc_delete_handles_dash_names() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws/-d")).unwrap();
        let out = run_local(&gc_delete_script(
            &gc_root(dir.path()),
            &["-d".into()],
            4_000_000_000,
        ));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "removed -d\n");
    }

    fn gc_root(dir: &Path) -> RemotePath {
        RemotePath::Abs(dir.to_str().unwrap().to_owned())
    }

    #[test]
    fn gc_delete_keeps_workspace_used_since_listing() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws/a")).unwrap();
        std::fs::write(dir.path().join("ws/a/.hopbox-last-used"), "").unwrap();
        // Cutoff far in the past: the workspace was used after it.
        let out = run_local(&gc_delete_script(&gc_root(dir.path()), &["a".into()], 1));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "fresh a\n");
        assert!(dir.path().join("ws/a").exists());
    }

    #[test]
    fn gc_delete_reports_busy_and_keeps_lock_file() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ws/a")).unwrap();
        std::fs::create_dir_all(dir.path().join("locks")).unwrap();
        let holder = hold_lock(&dir.path().join("locks/a.lock"));
        let out = run_local(&gc_delete_script(
            &gc_root(dir.path()),
            &["a".into()],
            4_000_000_000,
        ));
        drop(holder);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "busy a\n");
        assert!(dir.path().join("ws/a").exists());
        let out = run_local(&gc_delete_script(
            &gc_root(dir.path()),
            &["a".into()],
            4_000_000_000,
        ));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "removed a\n");
        assert!(!dir.path().join("ws/a").exists());
        assert!(
            dir.path().join("locks/a.lock").exists(),
            "lock file must stay"
        );
    }

    #[test]
    fn gc_delete_reports_rm_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("ws/a/sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("f"), "").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::remove_file(sub.join("f")).is_ok() || !have_flock() {
            return; // running as root: permissions do not stop rm
        }
        let out = run_local(&gc_delete_script(
            &gc_root(dir.path()),
            &["a".into()],
            4_000_000_000,
        ));
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with("failed a "), "{stdout}");
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
                locks: &RemotePath::parse("/l"),
                name: "my repo-1",
                wait_secs: 30,
                bg: None,
            }
            .render(),
            concat!(
                "mkdir -p /l && exec 9>'/l/my repo-1.lock' && ",
                "{ flock -n 9 || { echo 'hopbox: waiting for lock…' >&2; flock -w 30 9 || ",
                "{ echo 'hopbox: lock wait timed out' >&2; exit 124; }; }; } && ",
                "mkdir -p /w && cd /w && touch .hopbox-last-used && ",
                "echo locked && exec cat >/dev/null",
            )
        );
    }

    /// `<dir>/ws/w` workspace and `<dir>/locks/w.lock`, created as a run would.
    fn lock_layout(dir: &Path) -> (RemotePath, RemotePath, std::path::PathBuf) {
        std::fs::create_dir_all(dir.join("locks")).unwrap();
        let root = RemotePath::Abs(dir.to_str().unwrap().to_owned());
        (
            root.join("ws").join("w"),
            root.join("locks"),
            dir.join("locks/w.lock"),
        )
    }

    #[test]
    fn lock_script_bg_tail() {
        let ws = RemotePath::parse("/w");
        let jobs = RemotePath::parse("/r/jobs");
        let meta = ["cmd=echo hi".to_owned()];
        let s = LockScript {
            workspace: &ws,
            locks: &RemotePath::parse("/l"),
            name: "n",
            wait_secs: 1,
            bg: Some(BgStart {
                jobs: &jobs,
                id: "0a1b2c3d",
                meta: &meta,
                wrapper: "echo 'job'",
            }),
        }
        .render();
        assert!(
            s.ends_with(concat!(
                r#"echo locked && IFS= read -r go && [ "$go" = go ] && mkdir -p /r/jobs && "#,
                r#"mkdir /r/jobs/0a1b2c3d && { printf '%s\n' 'cmd=echo hi'; echo "start=$(date +%s)"; } "#,
                r#"> /r/jobs/0a1b2c3d/meta && { setsid nohup sh -c 'echo '\''job'\''' "#,
                r#"</dev/null >/dev/null 2>&1 & echo $! > /r/jobs/0a1b2c3d/pid && "#,
                r#"echo "$(cat /proc/sys/kernel/random/boot_id) $(awk '{print $22}' /proc/$!/stat)" "#,
                r#"> /r/jobs/0a1b2c3d/ident && echo started; }"#,
            )),
            "{s}"
        );
    }

    /// Run the bg tail in real sh, feeding `go`, with a lock-free layout under `dir`.
    fn run_bg_tail(dir: &Path, id: &str, wrapper: &str) -> std::process::Output {
        use std::io::Write;
        let root = RemotePath::Abs(dir.to_str().unwrap().to_owned());
        let (ws, locks, jobs) = (root.join("ws/w"), root.join("locks"), root.join("jobs"));
        let meta = ["cmd=x".to_owned()];
        let script = LockScript {
            workspace: &ws,
            locks: &locks,
            name: "w",
            wait_secs: 1,
            bg: Some(BgStart {
                jobs: &jobs,
                id,
                meta: &meta,
                wrapper,
            }),
        }
        .render();
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(&script))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn bg_start_reserves_job_dir_once() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = run_bg_tail(dir.path(), "0a1b2c3d", "true");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "locked\nstarted\n");
        let jd = dir.path().join("jobs/0a1b2c3d");
        assert!(jd.join("pid").exists() && jd.join("meta").exists());
        // Same id again: the existing dir is never reused, and no `started`.
        let out = run_bg_tail(dir.path(), "0a1b2c3d", "true");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "locked\n");
    }

    #[test]
    fn bg_wrapper_golden() {
        let jd = RemotePath::parse("/r/jobs/0a1b2c3d");
        let unit = UnitName::random();
        let scope = unit.scope();
        assert_eq!(
            bg_wrapper_script(&jd, "exec x", None),
            concat!(
                "trap true TERM; cd /r/jobs/0a1b2c3d && ",
                "{ ( exec x ) 9>&- >log 2>&1; rc=$?; echo $rc > exit.tmp && mv exit.tmp exit; }",
            )
        );
        let capped = bg_wrapper_script(&jd, "exec x", Some(&unit));
        assert!(capped.contains(&format!(
            "r=$(systemctl --user show -p Result --value {scope} 2>/dev/null); "
        )));
        assert!(capped.contains(r#"if [ "$r" = oom-kill ]; then rc=137; touch oom; fi; "#));
    }

    #[test]
    fn bg_wrapper_runs_in_real_sh() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("j")).unwrap();
        let jd = RemotePath::Abs(dir.path().join("j").to_str().unwrap().to_owned());
        let out = run_local(&bg_wrapper_script(
            &jd,
            "echo out; echo err >&2; exit 5",
            None,
        ));
        assert!(out.status.success());
        let read = |f: &str| std::fs::read_to_string(dir.path().join("j").join(f)).unwrap();
        assert_eq!(read("exit"), "5\n");
        assert_eq!(read("log"), "out\nerr\n");
    }

    #[test]
    fn gc_jobs_reports_rm_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("jobs/0a1b2c3d/sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("jobs/0a1b2c3d/exit"), "0\n").unwrap();
        std::fs::write(sub.join("f"), "").unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::remove_file(sub.join("f")).is_ok() {
            return; // root: permissions do not stop rm
        }
        let out = run_local(&gc_jobs_script(&gc_root(dir.path()), 4_000_000_000, true));
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "failed job 0a1b2c3d\n"
        );
        assert!(!out.status.success());
    }

    /// A job dir whose pid points at a live process that is not the job.
    fn reused_pid_job(dir: &Path) -> (KillOnDrop, std::path::PathBuf) {
        let other = KillOnDrop(Command::new("sleep").arg("30").spawn().unwrap());
        let jd = dir.join("jobs/0a1b2c3d");
        std::fs::create_dir_all(&jd).unwrap();
        std::fs::write(jd.join("pid"), format!("{}\n", other.0.id())).unwrap();
        std::fs::write(jd.join("ident"), "some-other-boot 12345\n").unwrap();
        std::fs::write(jd.join("meta"), "cmd=x\nstart=1\n").unwrap();
        (other, jd)
    }

    #[test]
    fn kill_never_signals_a_reused_pid() {
        let dir = tempfile::tempdir().unwrap();
        let (mut other, _) = reused_pid_job(dir.path());
        let out = run_local(&job_kill_script(&gc_root(dir.path()), "0a1b2c3d"));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "lost\n");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            other.0.try_wait().unwrap().is_none(),
            "unrelated process was signalled"
        );
    }

    #[test]
    fn reused_pid_is_lost_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let (_other, _) = reused_pid_job(dir.path());
        let out = run_local(&jobs_list_script(&gc_root(dir.path())));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("0a1b2c3d\tlost\t-"), "{text}");
        let out = run_local(&job_wait_script(&gc_root(dir.path()), "0a1b2c3d"));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "lost\n");
    }

    #[test]
    fn live_job_is_running_and_killable() {
        if !have_flock() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wrapper = bg_wrapper_script(
            &gc_root(dir.path()).join("jobs").join("0a1b2c3d"),
            "exec sleep 30",
            None,
        );
        let out = run_bg_tail(dir.path(), "0a1b2c3d", &wrapper);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "locked\nstarted\n");
        let list = run_local(&jobs_list_script(&gc_root(dir.path())));
        let text = String::from_utf8_lossy(&list.stdout);
        assert!(text.contains("0a1b2c3d\trunning\t-"), "{text}");
        let out = run_local(&job_kill_script(&gc_root(dir.path()), "0a1b2c3d"));
        assert!(out.status.success());
        let wait = run_local(&job_wait_script(&gc_root(dir.path()), "0a1b2c3d"));
        assert_eq!(String::from_utf8_lossy(&wait.stdout), "143\n");
    }

    #[test]
    fn logs_follow_ends_for_lost_job_with_reused_pid() {
        let dir = tempfile::tempdir().unwrap();
        let (_other, jd) = reused_pid_job(dir.path());
        std::fs::write(jd.join("log"), "partial\n").unwrap();
        let t = std::time::Instant::now();
        let out = run_local(&job_logs_script(&gc_root(dir.path()), "0a1b2c3d", true));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "must not follow a reused pid"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "partial\n");
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
        let (ws, locks, lock) = lock_layout(dir.path());
        let holder = hold_lock(&lock);
        let out = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(
                &LockScript {
                    workspace: &ws,
                    locks: &locks,
                    name: "w",
                    wait_secs: 0,
                    bg: None,
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
        let (ws, locks, lock) = lock_layout(dir.path());
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(wrap_for_ssh(
                &LockScript {
                    workspace: &ws,
                    locks: &locks,
                    name: "w",
                    wait_secs: 5,
                    bg: None,
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
        assert!(dir.path().join("ws/w/.hopbox-last-used").exists());
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
