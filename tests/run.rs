mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    sub: PathBuf,
    cfg_home: PathBuf,
    state: PathBuf,
    runtime: PathBuf,
}

fn setup(s: &common::Sshd) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("my repo");
    let sub = root.join("crates/a b");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(root.join("README"), "hi\n").unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let cfg_home = tmp.path().join("cfg");
    std::fs::create_dir_all(cfg_home.join("hopbox")).unwrap();
    std::fs::write(
        cfg_home.join("hopbox/config.toml"),
        format!(
            "default_host = \"test\"\n[hosts.test]\nssh = \"{}\"\n",
            s.host
        ),
    )
    .unwrap();
    let state = tmp.path().join("state");
    let runtime = tmp.path().join("run");
    Env {
        root,
        sub,
        cfg_home,
        state,
        runtime,
        _tmp: tmp,
    }
}

fn hopbox(s: &common::Sshd, e: &Env, cwd: &Path, args: &[&str]) -> Command {
    let mut c = Command::cargo_bin("hopbox").unwrap();
    c.current_dir(cwd)
        .args(args)
        .env("XDG_CONFIG_HOME", &e.cfg_home)
        .env("XDG_STATE_HOME", &e.state)
        .env("XDG_RUNTIME_DIR", &e.runtime)
        .env("HOPBOX_SSH_CONFIG", &s.ssh_config)
        .stdin(Stdio::null());
    c
}

fn run(s: &common::Sshd, e: &Env, args: &[&str]) -> Output {
    hopbox(s, e, &e.sub, args).output().unwrap()
}

fn last_stderr_line(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn run_passes_exit_code_and_streams() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(
        &s,
        &e,
        &["run", "--", "sh", "-c", "'echo out; echo err >&2; exit 3'"],
    );
    assert_eq!(
        o.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&o.stdout), "out\n");
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.lines().any(|l| l == "err"), "{stderr}");
    assert!(
        last_stderr_line(&o).starts_with("hopbox: exit=3 source=remote host=test dur="),
        "{stderr}"
    );
}

#[test]
fn run_executes_in_subdir_with_synced_files() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--", "pwd; cat ../../README"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.lines().next().unwrap().ends_with("/crates/a b"),
        "{out}"
    );
    assert_eq!(out.lines().nth(1), Some("hi"));
}

#[test]
fn remote_255_is_remote() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--", "exit", "255"]);
    assert_eq!(o.status.code(), Some(255));
    assert!(
        last_stderr_line(&o).contains("source=remote"),
        "{}",
        last_stderr_line(&o)
    );
}

#[test]
fn unknown_host_is_125() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--host", "nope", "--", "true"]);
    assert_eq!(o.status.code(), Some(125));
    let last = last_stderr_line(&o);
    assert!(
        last.contains("exit=125 source=hopbox host=-") && last.contains("nope"),
        "{last}"
    );
}

#[test]
fn usage_error_is_125() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--bogus"]);
    assert_eq!(o.status.code(), Some(125));
    let o = run(&s, &e, &["run"]);
    assert_eq!(
        o.status.code(),
        Some(125),
        "missing command is a usage error"
    );
}

#[test]
fn unreachable_is_255() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let cfg = e.cfg_home.join("bad_ssh_config");
    std::fs::write(&cfg, "Host nowhere\n  HostName 127.0.0.1\n  Port 1\n").unwrap();
    std::fs::write(
        e.cfg_home.join("hopbox/config.toml"),
        "default_host = \"gone\"\n[hosts.gone]\nssh = \"nowhere\"\n",
    )
    .unwrap();
    let o = hopbox(&s, &e, &e.sub, &["run", "--", "true"])
        .env("HOPBOX_SSH_CONFIG", &cfg)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(255));
    let last = last_stderr_line(&o);
    assert!(
        last.contains("source=hopbox host=gone") && last.contains("cannot reach"),
        "{last}"
    );
}

#[test]
fn json_status() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--json", "--", "exit", "4"]);
    assert_eq!(o.status.code(), Some(4));
    let v: serde_json::Value = serde_json::from_str(&last_stderr_line(&o)).unwrap();
    assert_eq!(v["exit"], 4);
    assert_eq!(v["source"], "remote");
}

#[test]
fn tail_writes_log() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--tail", "2", "--", "seq", "10"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "9\n10\n");
    let last = last_stderr_line(&o);
    let log = last
        .split(" log=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap();
    let body = std::fs::read_to_string(log).unwrap();
    assert_eq!(body.lines().count(), 10, "{body}");
}

#[test]
fn uncapped_warns() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--", "true"]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(
        stderr.contains("hopbox: warning: no resource caps on test"),
        "{stderr}"
    );
}

#[test]
fn no_sync_reuses_remote_copy() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    assert!(run(&s, &e, &["run", "--", "true"]).status.success());
    std::fs::write(e.root.join("README"), "changed\n").unwrap();
    let o = run(&s, &e, &["run", "--no-sync", "--", "cat", "../../README"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout), "hi\n");
}

#[test]
fn lock_timeout_is_124() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    // First run creates the workspace; then hold its lock from inside the container.
    assert!(run(&s, &e, &["run", "--", "true"]).status.success());
    let held = s.ssh(
        "cd ~/.cache/hopbox && mkdir -p locks && for d in ws/*/; do n=${d#ws/}; nohup flock \"locks/${n%/}.lock\" sleep 60 </dev/null >/dev/null 2>&1 & done; sleep 0.5",
    );
    assert!(held.status.success());
    let o = hopbox(&s, &e, &e.sub, &["run", "--no-sync", "--", "echo", "ran"])
        .env("HOPBOX_LOCK_WAIT_SECS", "1")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.stdout.is_empty());
    let last = last_stderr_line(&o);
    assert!(
        last.contains("exit=124 source=hopbox") && last.contains("lock wait timed out"),
        "{last}"
    );
}

#[test]
fn interrupt_exits_130() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let child = hopbox(&s, &e, &e.sub, &["run", "--", "sleep", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until the remote sleep is running.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !s.ssh("pgrep -x sleep").status.success() {
        assert!(Instant::now() < deadline, "remote sleep never started");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let sent = Instant::now();
    let o = child.wait_with_output().unwrap();
    assert!(
        sent.elapsed() < Duration::from_secs(10),
        "hopbox took {:?} to exit after SIGINT",
        sent.elapsed()
    );
    assert_eq!(
        o.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        last_stderr_line(&o).contains("exit=130 source=hopbox"),
        "{}",
        last_stderr_line(&o)
    );
}

fn write_repo_config(e: &Env, body: &str) {
    std::fs::write(e.root.join(".hopbox.toml"), body).unwrap();
}

#[test]
fn task_runs_with_extra_args() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    write_repo_config(&e, "[tasks]\nhi = \"echo hi\"\n");
    let o = run(&s, &e, &["run", "--task", "hi", "--", "there"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "hi there\n");
}

#[test]
fn unknown_task_is_125() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    write_repo_config(&e, "[tasks]\nhi = \"echo hi\"\ntest = \"true\"\n");
    let o = run(&s, &e, &["run", "--task", "nope"]);
    assert_eq!(o.status.code(), Some(125));
    let last = last_stderr_line(&o);
    assert!(
        last.contains("unknown task") && last.contains("hi, test"),
        "{last}"
    );
}

#[test]
fn env_forwarded_exactly() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let value = "a'b \"$c\"\nd";
    let o = hopbox(
        &s,
        &e,
        &e.sub,
        &["run", "--env", "X", "--", "printf", "%s", "\"$X\""],
    )
    .env("X", value)
    .output()
    .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), value);
    let left = s.ssh("ls -A ~/.cache/hopbox/ws/*/ | grep -c hopbox-env || true");
    assert_eq!(String::from_utf8_lossy(&left.stdout).trim(), "0");
}

#[test]
fn env_unset_flag_is_125() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = hopbox(
        &s,
        &e,
        &e.sub,
        &["run", "--env", "HOPBOX_SURELY_UNSET", "--", "true"],
    )
    .env_remove("HOPBOX_SURELY_UNSET")
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(125));
    assert!(last_stderr_line(&o).contains("HOPBOX_SURELY_UNSET"));
}

#[test]
fn repo_env_unset_warns_and_runs() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    write_repo_config(&e, "env = [\"HOPBOX_SURELY_UNSET\"]\n");
    let o = run(&s, &e, &["run", "--", "true"]);
    assert!(o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("env HOPBOX_SURELY_UNSET is not set locally")
    );
}

#[test]
fn fetch_copies_back() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(
        &s,
        &e,
        &[
            "run",
            "--fetch",
            "out",
            "--fetch",
            "single.txt",
            "--",
            "mkdir -p out && echo r > out/report.txt && echo s > single.txt",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(e.sub.join("out/report.txt")).unwrap(),
        "r\n"
    );
    assert_eq!(
        std::fs::read_to_string(e.sub.join("single.txt")).unwrap(),
        "s\n"
    );
}

#[test]
fn fetch_on_failure() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    write_repo_config(&e, "fetch = [\"crates/a b/log.txt\"]\n");
    // Repo fetch paths are relative to the current subdir as well.
    let o = hopbox(
        &s,
        &e,
        &e.root,
        &[
            "run",
            "--",
            "mkdir -p 'crates/a b' && echo failed > 'crates/a b/log.txt'; exit 2",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(e.sub.join("log.txt")).unwrap(),
        "failed\n"
    );
}

#[test]
fn fetch_missing_warns() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let o = run(&s, &e, &["run", "--fetch", "nothere", "--", "exit", "3"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&o.stderr).contains("hopbox: warning: fetch nothere failed"));
}

#[test]
fn fetch_rejects_escape() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    for bad in ["../x", "/etc/passwd"] {
        let o = run(&s, &e, &["run", "--fetch", bad, "--", "true"]);
        assert_eq!(o.status.code(), Some(125), "{bad}");
    }
}

#[test]
fn sync_waits_for_the_lock() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    assert!(run(&s, &e, &["run", "--", "true"]).status.success());
    std::fs::write(e.root.join("README"), "edited\n").unwrap();
    let held = s.ssh(
        "cd ~/.cache/hopbox && mkdir -p locks && for d in ws/*/; do n=${d#ws/}; nohup flock \"locks/${n%/}.lock\" sleep 60 </dev/null >/dev/null 2>&1 & done; sleep 0.5",
    );
    assert!(held.status.success());
    let o = hopbox(&s, &e, &e.sub, &["run", "--", "true"])
        .env("HOPBOX_LOCK_WAIT_SECS", "1")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    // The running job's files were not touched: sync waited for the lock and gave up.
    let readme = s.ssh("cat ~/.cache/hopbox/ws/*/README");
    assert_eq!(String::from_utf8_lossy(&readme.stdout), "hi\n");
}

#[test]
fn fetch_refuses_symlinked_parent() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), e.sub.join("out")).unwrap();
    let o = run(&s, &e, &["run", "--fetch", "out/r.txt", "--", "true"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        !outside.path().join("r.txt").exists(),
        "fetch wrote through a symlink"
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("hopbox: warning: fetch out/r.txt"));
}

#[test]
fn env_path_without_rm_still_cleans_up() {
    let Some(s) = common::sshd() else { return };
    let e = setup(&s);
    // A local PATH that exists only on the laptop: the remote finds no `rm` under it.
    let bin = tempfile::tempdir().unwrap();
    for tool in ["ssh", "rsync", "git", "kill", "hostname", "sh"] {
        let found = Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap();
        let path = String::from_utf8(found.stdout).unwrap();
        if !path.trim().is_empty() {
            std::os::unix::fs::symlink(path.trim(), bin.path().join(tool)).unwrap();
        }
    }
    let o = hopbox(
        &s,
        &e,
        &e.sub,
        &["run", "--env", "PATH", "--", "echo", "ran"],
    )
    .env("PATH", bin.path())
    .output()
    .unwrap();
    let left = s.ssh("ls -A ~/.cache/hopbox/ws/*/ | grep -c hopbox-env || true");
    assert_eq!(
        String::from_utf8_lossy(&left.stdout).trim(),
        "0",
        "env file left behind: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}
