//! `hopbox gc`: delete remote workspaces that have not been used for a while.

use std::time::Duration;

use crate::config::{self, RunFlags};
use crate::exit;
use crate::remote_script::{RemotePath, gc_delete_script, gc_jobs_script, gc_list_script};
use crate::run::Env;
use crate::transport::Transport;

/// `14d`, `12h`, `2w`, `30m` → a duration.
pub fn parse_age(s: &str) -> Result<Duration, String> {
    let bad = || format!("invalid age {s:?}: use a number with m, h, d or w (for example 14d)");
    let unit = s.chars().last().ok_or_else(bad)?;
    let secs_per = match unit {
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 7 * 86_400,
        _ => return Err(bad()),
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(secs_per)
        .map(Duration::from_secs)
        .ok_or_else(bad)
}

/// Workspace names hopbox creates are one path segment; anything else is never touched.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

pub fn execute(host: Option<&str>, older_than: &str, dry_run: bool, env: &Env) -> i32 {
    let fail = |code: u8, msg: String| {
        eprintln!("hopbox: {msg}");
        i32::from(code)
    };
    let age = match parse_age(older_than) {
        Ok(a) => a,
        Err(e) => return fail(exit::USAGE, e),
    };
    let global = match &env.global_config {
        Some(p) => match config::load_global(p) {
            Ok(g) => g,
            Err(e) => return fail(exit::USAGE, e.to_string()),
        },
        None => config::GlobalConfig::default(),
    };
    let flags = RunFlags {
        host: host.map(str::to_owned),
        ..RunFlags::default()
    };
    let r = match config::resolve(&global, &config::RepoConfig::default(), &flags) {
        Ok(r) => r,
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    let t = match Transport::new(&r.ssh_dest) {
        Ok(t) => t,
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    let root = RemotePath::parse(&r.workspace_root);
    let reachable = |out: &std::process::Output| out.status.code() != Some(255);
    let list = match t.capture(&gc_list_script(&root)) {
        Ok(o) if reachable(&o) => o,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            return fail(
                exit::CONNECT,
                format!("cannot reach {}: {}", r.host_name, stderr.trim()),
            );
        }
        Err(e) => return fail(exit::USAGE, e.to_string()),
    };
    let text = String::from_utf8_lossy(&list.stdout);
    let mut lines = text.lines();
    let Some(now) = lines
        .next()
        .and_then(|l| l.strip_prefix("now="))
        .and_then(|n| n.trim().parse::<u64>().ok())
    else {
        return fail(
            exit::USAGE,
            format!("unexpected output from {}", r.host_name),
        );
    };
    let mut old: Vec<(String, u64)> = Vec::new();
    for line in lines {
        let Some((t_str, name)) = line.split_once(' ') else {
            continue;
        };
        let Ok(mtime) = t_str.parse::<u64>() else {
            continue;
        };
        if !safe_name(name) {
            eprintln!("hopbox: warning: skipping unexpected workspace name {name:?}");
            continue;
        }
        let unused = now.saturating_sub(mtime);
        if unused > age.as_secs() {
            old.push((name.to_owned(), unused));
        }
    }
    // Finished background jobs older than the window go too (their dirs hold logs only).
    let cutoff = now.saturating_sub(age.as_secs());
    let mut removed_jobs = 0;
    match t.capture(&gc_jobs_script(&root, cutoff, !dry_run)) {
        Ok(o) if reachable(&o) => {
            for id in String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.strip_prefix("job "))
            {
                removed_jobs += 1;
                if dry_run {
                    println!("would remove job {id}");
                } else {
                    println!("removed job {id}");
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            return fail(
                exit::CONNECT,
                format!("cannot reach {}: {}", r.host_name, stderr.trim()),
            );
        }
        Err(e) => return fail(exit::USAGE, e.to_string()),
    }
    if old.is_empty() {
        if removed_jobs == 0 {
            println!("nothing to remove");
        }
        return 0;
    }
    if dry_run {
        for (name, unused) in &old {
            println!("would remove {name} (unused {}d)", unused / 86_400);
        }
        return 0;
    }
    let names: Vec<String> = old.into_iter().map(|(n, _)| n).collect();
    match t.capture(&gc_delete_script(&root, &names, cutoff)) {
        Ok(o) if reachable(&o) => {
            let mut code = 0;
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some(n) = line.strip_prefix("removed ") {
                    println!("removed {n}");
                } else if let Some(n) = line.strip_prefix("busy ") {
                    println!("skipped {n}: in use");
                } else if let Some(n) = line.strip_prefix("fresh ") {
                    println!("skipped {n}: used since listing");
                } else if let Some(rest) = line.strip_prefix("failed ") {
                    let (n, rc) = rest.rsplit_once(' ').unwrap_or((rest, "?"));
                    eprintln!("hopbox: failed to remove {n} (exit {rc})");
                    code = 1;
                }
            }
            if !o.status.success() {
                eprintln!(
                    "hopbox: gc failed on {}: {}",
                    r.host_name,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                code = 1;
            }
            code
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            fail(
                exit::CONNECT,
                format!("cannot reach {}: {}", r.host_name, stderr.trim()),
            )
        }
        Err(e) => fail(exit::USAGE, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_age_forms() {
        assert_eq!(parse_age("14d"), Ok(Duration::from_secs(14 * 86_400)));
        assert_eq!(parse_age("12h"), Ok(Duration::from_secs(12 * 3_600)));
        assert_eq!(parse_age("2w"), Ok(Duration::from_secs(14 * 86_400)));
        assert_eq!(parse_age("30m"), Ok(Duration::from_secs(1_800)));
        assert_eq!(parse_age("0d"), Ok(Duration::ZERO));
        for bad in ["", "d", "14", "-1d", "1.5d", "14 d", "soon"] {
            assert!(parse_age(bad).is_err(), "{bad}");
        }
    }
}
