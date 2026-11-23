mod common;

use std::time::{Duration, Instant};

use common::{Repo, Sshd, finished, hopbox, job_id, repo};

fn start(s: &Sshd, r: &Repo, cmd: &str) -> String {
    let o = hopbox(s, r, &["run", "--bg", "--", cmd]).output().unwrap();
    job_id(&o)
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn wait_returns_job_code() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let id = start(&s, &r, "sleep 1; exit 7");
    let o = hopbox(&s, &r, &["wait", &id]).output().unwrap();
    assert_eq!(o.status.code(), Some(7));
    let stderr = String::from_utf8_lossy(&o.stderr);
    let last = stderr.lines().last().unwrap();
    assert!(
        last.starts_with("hopbox: exit=7 source=remote host=test"),
        "{last}"
    );
    assert!(last.contains(&format!("job={id}")), "{last}");
}

#[test]
fn logs_prints_output() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let id = start(&s, &r, "echo one; echo two >&2");
    finished(&s, &id);
    let o = hopbox(&s, &r, &["logs", &id]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out(&o), "one\ntwo\n");
}

#[test]
fn logs_follow_ends_with_job() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let id = start(&s, &r, "echo a; sleep 2; echo b");
    let t = Instant::now();
    let o = hopbox(&s, &r, &["logs", "-f", &id]).output().unwrap();
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(out(&o), "a\nb\n");
}

#[test]
fn jobs_lists_running_and_done() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let done = start(&s, &r, "exit 4");
    finished(&s, &done);
    // A second repo, so its job doesn't wait on the first repo's lock.
    let r2 = repo(&s);
    let running = start(&s, &r2, "sleep 5");
    let o = hopbox(&s, &r, &["jobs", "--json"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    let find = |id: &str| {
        v.as_array()
            .unwrap()
            .iter()
            .find(|j| j["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} missing in {v}"))
    };
    assert_eq!(find(&done)["state"], "done");
    assert_eq!(find(&done)["exit"], 4);
    assert_eq!(find(&done)["cmd"], "exit 4");
    assert_eq!(find(&running)["state"], "running");
    assert!(find(&running)["exit"].is_null());
    let o = hopbox(&s, &r, &["jobs"]).output().unwrap();
    let text = out(&o);
    assert!(text.starts_with("ID"), "{text}");
    assert!(text.contains(&done) && text.contains(&running), "{text}");
}

#[test]
fn kill_stops_children() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let id = start(&s, &r, "sleep 101 & sleep 102; wait");
    std::thread::sleep(Duration::from_millis(500));
    let o = hopbox(&s, &r, &["kill", &id]).output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let (code, _) = finished(&s, &id);
    assert_eq!(code, "143");
    let ps = s.ssh("ps -eo args");
    let ps = String::from_utf8_lossy(&ps.stdout);
    assert!(
        !ps.contains("sleep 101") && !ps.contains("sleep 102"),
        "{ps}"
    );
}

#[test]
fn bad_job_id_125() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    for args in [
        vec!["wait", "nope"],
        vec!["logs", "../../x"],
        vec!["kill", "ABCDEF12"],
        vec!["wait", "0a1b2c3d"],
    ] {
        let o = hopbox(&s, &r, &args).output().unwrap();
        assert_eq!(o.status.code(), Some(125), "{args:?}");
    }
}

#[test]
fn gc_removes_old_job_dirs() {
    let Some(s) = common::sshd() else { return };
    let r = repo(&s);
    let id = start(&s, &r, "true");
    finished(&s, &id);
    let o = s.ssh(&format!(
        "touch -d '30 days ago' ~/.cache/hopbox/jobs/{id}/exit"
    ));
    assert!(o.status.success());
    let o = hopbox(&s, &r, &["gc"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(
        out(&o).contains(&format!("removed job {id}")),
        "{}",
        out(&o)
    );
    let o = s.ssh(&format!("test -e ~/.cache/hopbox/jobs/{id}"));
    assert!(!o.status.success());
}
