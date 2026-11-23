mod common;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;

struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cfg: PathBuf,
    state: PathBuf,
    runtime: PathBuf,
}

fn repo(s: &common::Sshd) -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(cfg.join("hopbox")).unwrap();
    std::fs::write(
        cfg.join("hopbox/config.toml"),
        format!(
            "default_host = \"test\"\n[hosts.test]\nssh = \"{}\"\n",
            s.host
        ),
    )
    .unwrap();
    Repo {
        root,
        state: tmp.path().join("state"),
        runtime: tmp.path().join("run"),
        cfg,
        _tmp: tmp,
    }
}

fn hopbox(s: &common::Sshd, r: &Repo, args: &[&str]) -> Command {
    let mut c = Command::cargo_bin("hopbox").unwrap();
    c.current_dir(&r.root)
        .args(args)
        .env("XDG_CONFIG_HOME", &r.cfg)
        .env("XDG_STATE_HOME", &r.state)
        .env("XDG_RUNTIME_DIR", &r.runtime)
        .env("HOPBOX_SSH_CONFIG", &s.ssh_config)
        .stdin(Stdio::null());
    c
}

fn job_id(o: &Output) -> String {
    let id = String::from_utf8_lossy(&o.stdout).trim().to_owned();
    assert!(
        id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "stdout {id:?}, stderr {}",
        String::from_utf8_lossy(&o.stderr)
    );
    id
}

/// Wait for the job's exit file and return (exit, log).
fn finished(s: &common::Sshd, id: &str) -> (String, String) {
    for _ in 0..100 {
        let o = s.ssh(&format!(
            "cd ~/.cache/hopbox/jobs/{id} && cat exit && echo --- && cat log"
        ));
        if o.status.success() {
            let text = String::from_utf8(o.stdout).unwrap();
            let (code, log) = text.split_once("---\n").unwrap();
            return (code.trim().to_owned(), log.to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("job {id} never finished");
}

#[test]
fn run_bg_prints_id_and_outlives_the_run() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let t = Instant::now();
    let o = hopbox(&s, &r, &["run", "--bg", "--", "sleep 2; echo hi; exit 3"])
        .output()
        .unwrap();
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "run --bg must not wait"
    );
    assert_eq!(o.status.code(), Some(0));
    let id = job_id(&o);
    let stderr = String::from_utf8_lossy(&o.stderr);
    let last = stderr.lines().last().unwrap();
    assert!(last.contains("source=hopbox"), "{last}");
    assert!(last.contains(&format!("job={id}")), "{last}");
    assert_eq!(finished(&s, &id), ("3".to_owned(), "hi\n".to_owned()));
    let index = std::fs::read_to_string(r.state.join("hopbox/jobs.tsv")).unwrap();
    assert_eq!(index, format!("{id}\ttest\n"));
}

#[test]
fn bg_job_holds_lock() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let o = hopbox(&s, &r, &["run", "--bg", "--", "sleep 3"])
        .output()
        .unwrap();
    let id = job_id(&o);
    let o = hopbox(&s, &r, &["run", "--", "true"])
        .env("HOPBOX_LOCK_WAIT_SECS", "1")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(124));
    assert_eq!(finished(&s, &id).0, "0");
    let o = hopbox(&s, &r, &["run", "--", "true"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "lock must be free after the job");
}

#[test]
fn bg_forwards_env() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let o = hopbox(
        &s,
        &r,
        &["run", "--bg", "--env", "HB_X", "--", "printf %s \"$HB_X\""],
    )
    .env("HB_X", "a'b $c")
    .output()
    .unwrap();
    let id = job_id(&o);
    assert_eq!(finished(&s, &id).1, "a'b $c");
}

#[test]
fn bg_rejects_fetch_and_tail() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    for extra in [["--fetch", "x"], ["--tail", "5"]] {
        let mut args = vec!["run", "--bg"];
        args.extend(extra);
        args.extend(["--", "true"]);
        let o = hopbox(&s, &r, &args).output().unwrap();
        assert_eq!(o.status.code(), Some(125), "{extra:?}");
    }
}
