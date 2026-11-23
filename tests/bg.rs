mod common;

use std::time::{Duration, Instant};

use common::{finished, hopbox, job_id, repo};

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
