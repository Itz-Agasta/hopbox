//! Background jobs. The remote `<workspace_root>/jobs/<id>/` dir is the source of truth; the
//! local index only remembers which host each job id was started on.

use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use crate::config::{self, RunFlags};
use crate::exit;
use crate::remote_script::{
    NO_JOB_EXIT, RemotePath, job_kill_script, job_logs_script, job_wait_script, jobs_list_script,
};
use crate::run::Env;
use crate::status::{Source, Status};
use crate::transport::Transport;

type Failure = (u8, String);

/// Job ids are the 8 lowercase hex digits hopbox prints; nothing else reaches a remote script.
pub fn valid_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct Conn {
    host: String,
    t: Transport,
    root: RemotePath,
}

/// Host: `--host`, else the one the job was started on, else `default_host`.
fn connect(host: Option<&str>, id: Option<&str>, env: &Env) -> Result<Conn, Failure> {
    let usage = |e: &dyn std::fmt::Display| (exit::USAGE, e.to_string());
    let global = match &env.global_config {
        Some(p) => config::load_global(p).map_err(|e| usage(&e))?,
        None => config::GlobalConfig::default(),
    };
    let recorded = id.and_then(|id| lookup(&env.state_dir.join("jobs.tsv"), id));
    let flags = RunFlags {
        host: host.map(str::to_owned).or(recorded),
        ..RunFlags::default()
    };
    let r =
        config::resolve(&global, &config::RepoConfig::default(), &flags).map_err(|e| usage(&e))?;
    let t = Transport::new(&r.ssh_dest).map_err(|e| usage(&e))?;
    Ok(Conn {
        root: RemotePath::parse(&r.workspace_root),
        host: r.host_name,
        t,
    })
}

impl Conn {
    /// Map a remote exit code: 255 is ssh's connection failure, [`NO_JOB_EXIT`] a missing job.
    fn check(&self, code: Option<i32>, id: &str, stderr: &[u8]) -> Result<(), Failure> {
        match code {
            Some(255) => Err((
                exit::CONNECT,
                format!(
                    "cannot reach {}: {}",
                    self.host,
                    String::from_utf8_lossy(stderr).trim()
                ),
            )),
            Some(NO_JOB_EXIT) => Err((exit::USAGE, format!("no job {id} on {}", self.host))),
            _ => Ok(()),
        }
    }

    fn capture(&self, script: &str, id: &str) -> Result<String, Failure> {
        let o = self
            .t
            .capture(script)
            .map_err(|e| (exit::USAGE, e.to_string()))?;
        self.check(o.status.code(), id, &o.stderr)?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

fn report(r: Result<i32, Failure>) -> i32 {
    r.unwrap_or_else(|(code, msg)| {
        eprintln!("hopbox: {msg}");
        i32::from(code)
    })
}

fn checked_id(id: &str) -> Result<(), Failure> {
    if valid_id(id) {
        Ok(())
    } else {
        Err((
            exit::USAGE,
            format!("invalid job id {id:?}: expected 8 hex digits from `hopbox run --bg`"),
        ))
    }
}

/// One job as `jobs` reports it.
#[derive(Debug, PartialEq)]
pub struct Job {
    pub id: String,
    pub state: String,
    pub exit: Option<i32>,
    pub start: u64,
    pub cmd: String,
}

/// Parse [`jobs_list_script`] output into (remote now, jobs sorted by start).
pub fn parse_list(text: &str) -> Option<(u64, Vec<Job>)> {
    let mut lines = text.lines();
    let now = lines.next()?.strip_prefix("now=")?.trim().parse().ok()?;
    let mut jobs: Vec<Job> = lines
        .filter_map(|l| {
            let mut f = l.splitn(5, '\t');
            Some(Job {
                id: f.next()?.to_owned(),
                state: f.next()?.to_owned(),
                exit: f.next()?.parse().ok(),
                start: f.next()?.parse().unwrap_or(0),
                cmd: f.next().unwrap_or("").to_owned(),
            })
        })
        .collect();
    jobs.sort_by_key(|j| j.start);
    Some((now, jobs))
}

fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 7_200 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// `hopbox jobs`.
pub fn list(host: Option<&str>, json: bool, env: &Env) -> i32 {
    report((|| {
        let c = connect(host, None, env)?;
        let text = c.capture(&jobs_list_script(&c.root), "-")?;
        let (now, jobs) = parse_list(&text)
            .ok_or_else(|| (exit::USAGE, format!("unexpected output from {}", c.host)))?;
        let mut out = io::stdout().lock();
        if json {
            let v: Vec<_> = jobs
                .iter()
                .map(|j| {
                    serde_json::json!({
                        "id": j.id, "state": j.state, "exit": j.exit,
                        "start": j.start, "cmd": j.cmd, "host": c.host,
                    })
                })
                .collect();
            let _ = writeln!(out, "{}", serde_json::Value::from(v));
        } else {
            let _ = writeln!(
                out,
                "{:<8}  {:<7}  {:>4}  {:>4}  COMMAND",
                "ID", "STATE", "EXIT", "AGE"
            );
            for j in &jobs {
                let ex = j.exit.map_or("-".to_owned(), |e| e.to_string());
                let a = age(now.saturating_sub(j.start));
                let _ = writeln!(
                    out,
                    "{:<8}  {:<7}  {ex:>4}  {a:>4}  {}",
                    j.id, j.state, j.cmd
                );
            }
        }
        Ok(0)
    })())
}

/// `hopbox logs <id> [-f]`.
pub fn logs(id: &str, host: Option<&str>, follow: bool, env: &Env) -> i32 {
    report((|| {
        checked_id(id)?;
        let c = connect(host, Some(id), env)?;
        let st =
            c.t.stream(&job_logs_script(&c.root, id, follow))
                .map_err(|e| (exit::USAGE, e.to_string()))?;
        c.check(st.code(), id, b"")?;
        Ok(st.code().unwrap_or(1))
    })())
}

/// `hopbox wait <id>`: exits with the job's code and prints the status line.
pub fn wait(id: &str, host: Option<&str>, env: &Env) -> Status {
    let start = Instant::now();
    let mut host_name = "-".to_owned();
    let r = (|| {
        checked_id(id)?;
        let c = connect(host, Some(id), env)?;
        host_name = c.host.clone();
        let text = c.capture(&job_wait_script(&c.root, id), id)?;
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("").trim();
        let code: i32 = first
            .parse()
            .map_err(|_| (exit::USAGE, format!("job {id} ended without an exit code")))?;
        Ok(Status {
            exit: code,
            source: Source::Remote,
            host: c.host.clone(),
            dur: start.elapsed(),
            log: None,
            oom: lines.any(|l| l.trim() == "oom"),
            error: None,
            job: Some(id.to_owned()),
        })
    })();
    r.unwrap_or_else(|(code, msg)| {
        let mut st = Status::hopbox_error(code.into(), &host_name, start.elapsed(), msg);
        if valid_id(id) {
            st.job = Some(id.to_owned());
        }
        st
    })
}

/// `hopbox kill <id>`.
pub fn kill(id: &str, host: Option<&str>, env: &Env) -> i32 {
    report((|| {
        checked_id(id)?;
        let c = connect(host, Some(id), env)?;
        let text = c.capture(&job_kill_script(&c.root, id), id)?;
        if text.trim() == "done" {
            println!("job {id} had already finished");
        } else if text.trim() == "lost" {
            println!("job {id} is not running (it ended without an exit code)");
        } else {
            println!("stopped job {id}");
        }
        Ok(0)
    })())
}

/// Append `<id>\t<host>` to the index at `path`, creating it and its parent.
pub fn record(path: &Path, id: &str, host: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{id}\t{host}")
}

/// The host a job id was recorded with, if any (the latest entry wins).
pub fn lookup(path: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter_map(|l| l.split_once('\t'))
        .find(|(i, _)| *i == id)
        .map(|(_, h)| h.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_validation() {
        assert!(valid_id("0a1b2c3d"));
        for bad in [
            "",
            "0a1b2c3",
            "0a1b2c3d4",
            "ABCDEF12",
            "../../x",
            "0a1b 2c3",
            "nope",
        ] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn jobs_list_parse() {
        let text =
            "now=1000\nbbbbbbbb\trunning\t-\t990\tsleep 5\naaaaaaaa\tdone\t4\t900\techo a\tb\n";
        let (now, jobs) = parse_list(text).unwrap();
        assert_eq!(now, 1000);
        assert_eq!(jobs[0].id, "aaaaaaaa");
        assert_eq!(jobs[0].exit, Some(4));
        assert_eq!(jobs[0].cmd, "echo a\tb");
        assert_eq!(jobs[1].state, "running");
        assert_eq!(jobs[1].exit, None);
        assert!(parse_list("garbage").is_none());
    }

    #[test]
    fn index_record_lookup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state/hopbox/jobs.tsv");
        assert_eq!(lookup(&p, "0a1b2c3d"), None);
        record(&p, "0a1b2c3d", "hermes").unwrap();
        record(&p, "11111111", "box").unwrap();
        assert_eq!(lookup(&p, "0a1b2c3d").as_deref(), Some("hermes"));
        assert_eq!(lookup(&p, "11111111").as_deref(), Some("box"));
        assert_eq!(lookup(&p, "22222222"), None);
    }
}
