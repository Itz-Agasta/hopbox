//! `hopbox doctor [host]`: check that a host has what hopbox needs. Never installs anything.

use std::collections::BTreeMap;

use crate::config::{self, RunFlags};
use crate::exit;
use crate::remote_script::{RemotePath, doctor_script};
use crate::run::Env;
use crate::transport::Transport;

/// What the doctor script reported, as `key=value` pairs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    fields: BTreeMap<String, String>,
}

/// Facts from config shown alongside the remote checks.
#[derive(Debug, Default)]
pub struct HostFacts<'a> {
    pub name: &'a str,
    pub ssh_dest: &'a str,
    pub memory_max: Option<&'a str>,
    pub cost: Option<&'a str>,
}

pub fn parse_report(out: &str) -> Report {
    let fields = out
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Report { fields }
}

/// Required tools and the package to suggest when one is missing.
const REQUIRED: [(&str, &str); 3] = [
    ("rsync", "rsync"),
    ("bash", "bash"),
    ("flock", "util-linux"),
];

fn row(status: &str, check: &str, detail: &str) -> String {
    format!("{status:<6}{check:<10}{detail}")
}

fn gib(kb: u64) -> String {
    format!("{:.1} GiB", kb as f64 / 1_048_576.0)
}

impl Report {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Rows `ok|warn|fail|info  <check>  <detail>`, the header first.
    pub fn lines(&self, facts: &HostFacts) -> Vec<String> {
        let mut out = vec![
            format!("hopbox doctor: {} (ssh {})", facts.name, facts.ssh_dest),
            row("ok", "ssh", "connected"),
        ];
        for (tool, package) in REQUIRED {
            out.push(match self.get(tool) {
                Some(v) => row("ok", tool, v),
                None => row("fail", tool, &format!("not found (install {package})")),
            });
        }
        out.push(if self.get("caps") == Some("1") {
            let cap = match facts.memory_max {
                Some(m) => format!("memory_max {m}"),
                None => "no memory_max set".to_owned(),
            };
            row("ok", "caps", &format!("systemd-run --user works ({cap})"))
        } else {
            row(
                "warn",
                "caps",
                "systemd-run --user unavailable: jobs run without memory or CPU caps",
            )
        });
        out.push(row("info", "arch", self.get("arch").unwrap_or("unknown")));
        let mem = self.get("mem").and_then(|m| {
            let (avail, total) = m.split_once(' ')?;
            Some(format!(
                "{} available of {}",
                gib(avail.trim().parse().ok()?),
                gib(total.trim().parse().ok()?)
            ))
        });
        out.push(row("info", "memory", mem.as_deref().unwrap_or("unknown")));
        let disk = self
            .get("disk")
            .and_then(|d| d.parse().ok())
            .map(|kb| format!("{} free in the workspace root", gib(kb)));
        out.push(row("info", "disk", disk.as_deref().unwrap_or("unknown")));
        out.push(row("info", "path", self.get("path").unwrap_or("unknown")));
        if let Some(cost) = facts.cost {
            out.push(row("info", "cost", cost));
        }
        out
    }

    /// 1 when a required tool (rsync, bash, flock) is missing, else 0.
    pub fn exit_code(&self) -> i32 {
        if REQUIRED.iter().all(|(tool, _)| self.get(tool).is_some()) {
            0
        } else {
            1
        }
    }
}

pub fn execute(host: Option<&str>, env: &Env) -> i32 {
    let fail = |code: u8, msg: String| {
        eprintln!("hopbox: {msg}");
        i32::from(code)
    };
    let mut global = match &env.global_config {
        Some(p) => match config::load_global(p) {
            Ok(g) => g,
            Err(e) => return fail(exit::USAGE, e.to_string()),
        },
        None => config::GlobalConfig::default(),
    };
    crate::state::add_rented(&mut global, &env.state_dir);
    let flags = RunFlags {
        host: host.map(str::to_owned),
        ..RunFlags::default()
    };
    let r = match config::resolve(&global, &config::RepoConfig::default(), &flags) {
        Ok(r) => r,
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    let t = match Transport::for_host(&r) {
        Ok(t) => t,
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    let root = RemotePath::parse(&r.workspace_root);
    let out = match t.capture(&doctor_script(&root, &r.shell)) {
        Ok(o) => o,
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    if out.status.code() == Some(255) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return fail(
            exit::CONNECT,
            format!("cannot reach {}: {}", r.host_name, stderr.trim()),
        );
    }
    let report = parse_report(&String::from_utf8_lossy(&out.stdout));
    let facts = HostFacts {
        name: &r.host_name,
        ssh_dest: &r.ssh_dest,
        memory_max: r.memory_max.as_ref().map(|m| m.as_str()),
        cost: r.cost.as_deref(),
    };
    for line in report.lines(&facts) {
        println!("{line}");
    }
    report.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "arch=aarch64\nrsync=rsync  version 3.2.7  protocol version 31\nbash=/usr/bin/bash\n\
        flock=flock from util-linux 2.39.3\ncaps=1\nmem=8388608 12582912\ndisk=41943040\npath=/home/u/.cargo/bin:/usr/bin\n";

    fn facts() -> HostFacts<'static> {
        HostFacts {
            name: "hermes",
            ssh_dest: "hermes",
            memory_max: Some("6G"),
            cost: Some("free"),
        }
    }

    #[test]
    fn parse_report_full() {
        let r = parse_report(FULL);
        let lines = r.lines(&facts());
        assert_eq!(lines[0], "hopbox doctor: hermes (ssh hermes)");
        let want = [
            "ok    ssh       connected",
            "ok    rsync     rsync  version 3.2.7  protocol version 31",
            "ok    bash      /usr/bin/bash",
            "ok    flock     flock from util-linux 2.39.3",
            "ok    caps      systemd-run --user works (memory_max 6G)",
            "info  arch      aarch64",
            "info  memory    8.0 GiB available of 12.0 GiB",
            "info  disk      40.0 GiB free in the workspace root",
            "info  path      /home/u/.cargo/bin:/usr/bin",
            "info  cost      free",
        ];
        assert_eq!(&lines[1..], want);
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn missing_flock_fails() {
        let r = parse_report(&FULL.replace("flock=flock from util-linux 2.39.3", "flock="));
        assert_eq!(r.exit_code(), 1);
        assert!(
            r.lines(&facts())
                .iter()
                .any(|l| l == "fail  flock     not found (install util-linux)")
        );
    }

    #[test]
    fn no_caps_warns() {
        let r = parse_report(&FULL.replace("caps=1", "caps=0"));
        assert_eq!(r.exit_code(), 0);
        assert!(
            r.lines(&facts())
                .iter()
                .any(|l| l.starts_with("warn  caps      systemd-run --user unavailable"))
        );
    }

    #[test]
    fn missing_fields_are_unknown_not_panics() {
        let r = parse_report("");
        assert_eq!(r.exit_code(), 1);
        assert!(
            r.lines(&HostFacts::default())
                .iter()
                .any(|l| l.starts_with("info  memory    unknown"))
        );
    }
}
