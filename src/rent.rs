//! `hopbox up` / `ls` / `down`: rent, list and destroy provider instances.
//!
//! Money rules (VISION principle 4): `up` shows the price and needs a human yes (a `y` on a
//! tty, or `--yes-spend`), every rental gets a deadline the instance enforces on itself, and
//! only instances labelled `hopbox-<name>` are ever touched.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::exit;
use crate::gc::parse_age;
use crate::provider::Cli;
use crate::provider::vast::{CreateSpec, InstanceInfo, LABEL_PREFIX, Query, Vast};
use crate::state::{self, Instance};

pub type Failure = (u8, String);

fn usage(msg: impl Into<String>) -> Failure {
    (exit::USAGE, msg.into())
}

fn failed(msg: impl Into<String>) -> Failure {
    (1, msg.into())
}

/// Names become `--host` values and vast labels: lowercase letters, digits and `-`.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Debug, Clone)]
pub struct UpArgs {
    pub gpu: String,
    pub ttl: String,
    pub max_price: Option<f64>,
    pub name: Option<String>,
    pub disk: u32,
    pub image: String,
    pub yes_spend: bool,
    /// Host names from `[hosts]`; a rented name must not shadow one.
    pub taken: Vec<String>,
}

/// Side effects `up` needs, injectable for tests.
pub struct Deps<'a> {
    pub now: &'a dyn Fn() -> u64,
    pub sleep: &'a dyn Fn(Duration),
    /// Ask the human; `None` when there is no terminal to ask on.
    pub confirm: &'a dyn Fn(&str) -> Option<bool>,
    /// Wait until ssh to the instance works; false after giving up.
    pub ssh_ready: &'a dyn Fn(&Instance) -> bool,
    pub poll: Duration,
    /// How many status polls before giving up on boot.
    pub max_polls: u32,
}

fn random_name() -> String {
    let mut b = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    format!("vast-{:02x}{:02x}", b[0], b[1])
}

fn load(state_dir: &Path) -> Result<Vec<Instance>, Failure> {
    state::load(&state::path(state_dir)).map_err(failed)
}

/// Longest TTL `up` accepts.
pub const MAX_TTL: Duration = Duration::from_secs(30 * 86_400);
/// A state lock without an owner pid older than this is from a hopbox that crashed while
/// taking it, and is broken.
const STALE_LOCK: Duration = Duration::from_secs(60);

/// Whether a lock dir belongs to a dead process. Uses the owner pid when recorded
/// (`kill -0`, which works on Linux and macOS), else the lock's age.
fn lock_is_stale(lock: &Path) -> bool {
    match std::fs::read_to_string(lock.join("pid")) {
        Ok(pid) if !pid.trim().is_empty() => !std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        _ => std::fs::metadata(lock)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() > STALE_LOCK),
    }
}

/// Load, change and save the state file under a lock (a directory: `mkdir` is atomic), so
/// two hopbox processes never drop each other's records.
pub fn with_state<T>(
    state_dir: &Path,
    f: impl FnOnce(&mut Vec<Instance>) -> Result<T, Failure>,
) -> Result<T, Failure> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| failed(format!("{}: {e}", state_dir.display())))?;
    let lock = state_dir.join("instances.lock");
    let mut tries = 0;
    loop {
        match std::fs::create_dir(&lock) {
            Ok(()) => {
                let _ = std::fs::write(lock.join("pid"), std::process::id().to_string());
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&lock) {
                    let _ = std::fs::remove_dir_all(&lock);
                    continue;
                }
                tries += 1;
                if tries > 200 {
                    return Err(failed(format!(
                        "{} is held by another hopbox; try again",
                        lock.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(failed(format!("{}: {e}", lock.display()))),
        }
    }
    let result = (|| {
        let mut list = load(state_dir)?;
        let out = f(&mut list)?;
        state::save(&state::path(state_dir), &list).map_err(failed)?;
        Ok(out)
    })();
    let _ = std::fs::remove_dir_all(&lock);
    result
}

/// Rent an instance, wait until ssh works, and record it as a host.
pub fn up<C: Cli>(
    v: &Vast<C>,
    a: &UpArgs,
    state_dir: &Path,
    d: &Deps,
) -> Result<Instance, Failure> {
    let name = a.name.clone().unwrap_or_else(random_name);
    if !valid_name(&name) {
        return Err(usage(format!(
            "invalid name {name:?}: use 1-32 lowercase letters, digits and '-'"
        )));
    }
    if a.taken.contains(&name) {
        return Err(usage(format!(
            "{name} is already a host in your config; pick another --name"
        )));
    }
    let ttl = parse_age(&a.ttl).map_err(usage)?;
    if ttl.is_zero() || ttl > MAX_TTL {
        return Err(usage("--ttl must be more than zero and at most 30d"));
    }
    if load(state_dir)?.iter().any(|i| i.name == name) {
        return Err(usage(format!("an instance named {name} already exists")));
    }
    let offers = v
        .search(&Query {
            gpu: a.gpu.clone(),
            max_price: a.max_price,
            disk: a.disk,
        })
        .map_err(|e| failed(e.to_string()))?;
    let offer = offers.first().ok_or_else(|| {
        failed(format!(
            "no vast offer matches {}{}",
            a.gpu,
            a.max_price
                .map(|m| format!(" at ${m:.2}/h or less"))
                .unwrap_or_default()
        ))
    })?;
    let hours = ttl.as_secs_f64() / 3600.0;
    let pitch = format!(
        "{} x{} · ${:.2}/h · {} · reliability {:.2} · up to ${:.2} for {}",
        offer.gpu_name,
        offer.num_gpus,
        offer.dph_total,
        offer.geolocation.as_deref().unwrap_or("unknown location"),
        offer.reliability,
        offer.dph_total * hours,
        a.ttl
    );
    if a.yes_spend {
        eprintln!("hopbox: renting {pitch}");
    } else {
        match (d.confirm)(&pitch) {
            None => {
                return Err(usage(format!(
                    "renting costs money ({pitch}): pass --yes-spend once a human has approved the price"
                )));
            }
            Some(false) => return Err(failed("not rented")),
            Some(true) => {}
        }
    }

    let deadline = (d.now)()
        .checked_add(ttl.as_secs())
        .ok_or_else(|| usage("--ttl is too large"))?;
    let id = v
        .create(
            offer.id,
            &CreateSpec {
                name: name.clone(),
                image: a.image.clone(),
                disk: a.disk,
                deadline,
            },
        )
        .map_err(|e| failed(e.to_string()))?;
    // Record it at once, so `ls` and `down` can find it even if the rest fails.
    let mut inst = Instance {
        name: name.clone(),
        provider: "vast".into(),
        id,
        ssh_user: String::new(),
        ssh_host: String::new(),
        ssh_port: 0,
        deadline,
        dph: offer.dph_total,
    };
    let recorded = with_state(state_dir, |list| {
        if list.iter().any(|i| i.name == name) {
            return Err(usage(format!(
                "another hopbox just rented an instance named {name}"
            )));
        }
        list.push(inst.clone());
        Ok(())
    });
    if let Err((code, msg)) = recorded {
        return Err(match v.destroy(id) {
            Ok(()) => (code, format!("{msg}; destroyed instance {id}")),
            Err(e) => (
                code,
                format!("{msg}; destroying instance {id} also failed: {e}"),
            ),
        });
    }

    let abandon = |why: String| -> Failure {
        if v.destroy(id).is_ok() {
            let _ = with_state(state_dir, |list| {
                list.retain(|i| i.id != id);
                Ok(())
            });
            failed(format!("{why}; destroyed instance {id}"))
        } else {
            failed(format!(
                "{why}; destroying instance {id} also failed, run `hopbox down {name}`"
            ))
        }
    };

    let mut polls = 0;
    loop {
        let info = v
            .show(id)
            .map_err(|e| abandon(e.to_string()))?
            .ok_or_else(|| abandon(format!("instance {id} disappeared")))?;
        match info.actual_status.as_deref() {
            Some("running") => break,
            Some(s @ ("exited" | "unknown" | "offline")) => {
                return Err(abandon(format!("instance {id} is {s}")));
            }
            _ => {}
        }
        polls += 1;
        if polls >= d.max_polls {
            return Err(abandon(format!("instance {id} did not start in time")));
        }
        (d.sleep)(d.poll);
    }
    let (user, host, port) = v.ssh_url(id).map_err(|e| abandon(e.to_string()))?;
    inst.ssh_user = user;
    inst.ssh_host = host;
    inst.ssh_port = port;
    with_state(state_dir, |list| {
        match list.iter_mut().find(|i| i.id == id) {
            Some(slot) => *slot = inst.clone(),
            None => list.push(inst.clone()),
        }
        Ok(())
    })?;
    if !(d.ssh_ready)(&inst) {
        return Err(abandon(format!("ssh to instance {id} never worked")));
    }
    Ok(inst)
}

/// Destroy `name`, after checking the instance still carries its `hopbox-<name>` label.
pub fn down<C: Cli>(v: &Vast<C>, name: &str, state_dir: &Path) -> Result<String, Failure> {
    let list = load(state_dir)?;
    let id = match list.iter().find(|i| i.name == name) {
        Some(i) => i.id,
        None => v
            .list()
            .map_err(|e| failed(e.to_string()))?
            .into_iter()
            .find(|i| i.hopbox_name() == Some(name))
            .map(|i| i.id)
            .ok_or_else(|| usage(format!("no instance named {name}")))?,
    };
    let forget = |id: u64| {
        with_state(state_dir, |list| {
            list.retain(|i| i.id != id);
            Ok(())
        })
    };
    let Some(info) = v.show(id).map_err(|e| failed(e.to_string()))? else {
        forget(id)?;
        return Ok(format!("{name} was already gone"));
    };
    let want = format!("{LABEL_PREFIX}{name}");
    if info.label.as_deref() != Some(want.as_str()) {
        return Err(failed(format!(
            "instance {id} is labelled {:?}, not {want:?}; refusing to destroy it",
            info.label.unwrap_or_default()
        )));
    }
    v.destroy(id).map_err(|e| failed(e.to_string()))?;
    forget(id)?;
    Ok(format!("destroyed {name}"))
}

/// One row of `ls`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Row {
    pub name: String,
    pub id: u64,
    pub status: String,
    pub gpu: String,
    pub dph: f64,
    /// Seconds since the instance started.
    pub age: u64,
    /// Estimated spend so far, USD.
    pub cost: f64,
    /// Seconds until the deadline; `None` for instances hopbox has no record of.
    pub left: Option<u64>,
}

/// Sweep expired and vanished instances, then list what's left. Returns (notes, rows).
pub fn ls<C: Cli>(
    v: &Vast<C>,
    state_dir: &Path,
    now: u64,
) -> Result<(Vec<String>, Vec<Row>), Failure> {
    // No provider calls happen under the state lock: decide from a snapshot, act, then
    // remove only the ids confirmed gone, so records added meanwhile are never touched.
    let mut live = v.list().map_err(|e| failed(e.to_string()))?;
    let known = load(state_dir)?;
    let mut notes = Vec::new();
    let mut gone: Vec<u64> = Vec::new();
    for i in &known {
        let on_vast = live
            .iter()
            .position(|l| l.id == i.id && l.hopbox_name() == Some(i.name.as_str()));
        match on_vast {
            Some(pos) if now >= i.deadline => match v.destroy(i.id) {
                Ok(()) => {
                    live.remove(pos);
                    gone.push(i.id);
                    notes.push(format!("destroyed {}: past its deadline", i.name));
                }
                Err(e) => notes.push(format!("cannot destroy expired {}: {e}", i.name)),
            },
            Some(_) => {}
            // Missing from the snapshot: confirm before forgetting (it may be brand new).
            None => match v.show(i.id) {
                Ok(Some(info)) if info.hopbox_name() == Some(i.name.as_str()) => {}
                Ok(_) => {
                    gone.push(i.id);
                    notes.push(format!("removed {}: no longer on vast", i.name));
                }
                Err(e) => notes.push(format!("cannot check {}: {e}", i.name)),
            },
        }
    }
    if !gone.is_empty() {
        with_state(state_dir, |list| {
            list.retain(|i| !gone.contains(&i.id));
            Ok(())
        })?;
    }
    let keep = known;
    let rows = live
        .iter()
        .map(|l: &InstanceInfo| {
            let age = l.start_date.map_or(0, |s| now.saturating_sub(s as u64));
            let rec = keep.iter().find(|k| k.id == l.id);
            Row {
                name: l.hopbox_name().unwrap_or_default().to_owned(),
                id: l.id,
                status: l.actual_status.clone().unwrap_or_else(|| "-".into()),
                gpu: l.gpu_name.clone().unwrap_or_else(|| "-".into()),
                dph: l.dph_total,
                age,
                cost: l.dph_total * age as f64 / 3600.0,
                left: rec.map(|k| k.deadline.saturating_sub(now)),
            }
        })
        .collect();
    Ok((notes, rows))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn human(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 7_200 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3_600, (s % 3_600) / 60),
    }
}

fn report(r: Result<i32, Failure>) -> i32 {
    r.unwrap_or_else(|(code, msg)| {
        eprintln!("hopbox: {msg}");
        i32::from(code)
    })
}

fn vast() -> Vast<crate::provider::SystemCli> {
    Vast {
        cli: crate::provider::SystemCli {
            program: "vastai".into(),
        },
    }
}

/// Ask on the terminal; `None` when stdin or stderr is not a terminal.
fn ask(pitch: &str) -> Option<bool> {
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    eprint!("Rent {pitch}? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    Some(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// `hopbox up vast …`.
pub fn execute_up(a: &UpArgs, state_dir: &Path) -> i32 {
    let ssh_ready = |i: &Instance| {
        let mut g = crate::config::GlobalConfig::default();
        state::merge_hosts(&mut g, std::slice::from_ref(i), state_dir);
        let flags = crate::config::RunFlags {
            host: Some(i.name.clone()),
            ..Default::default()
        };
        let Ok(r) = crate::config::resolve(&g, &Default::default(), &flags) else {
            return false;
        };
        let Ok(t) = crate::transport::Transport::for_host(&r) else {
            return false;
        };
        (0..24).any(|n| {
            if n > 0 {
                std::thread::sleep(Duration::from_secs(5));
            }
            t.capture("true").is_ok_and(|o| o.status.success())
        })
    };
    let deps = Deps {
        now: &now,
        sleep: &std::thread::sleep,
        confirm: &ask,
        ssh_ready: &ssh_ready,
        poll: Duration::from_secs(5),
        max_polls: 120,
    };
    report(up(&vast(), a, state_dir, &deps).map(|i| {
        println!(
            "hopbox: {} is ready (destroys itself in {}): hopbox run --host {} -- <command>",
            i.name,
            human(i.deadline.saturating_sub(now())),
            i.name
        );
        0
    }))
}

/// `hopbox ls`.
pub fn execute_ls(json: bool, state_dir: &Path) -> i32 {
    report(ls(&vast(), state_dir, now()).map(|(notes, rows)| {
        for n in notes {
            eprintln!("hopbox: {n}");
        }
        if json {
            println!("{}", serde_json::to_string(&rows).unwrap_or_default());
        } else if rows.is_empty() {
            println!("no hopbox instances");
        } else {
            println!(
                "{:<12}  {:>9}  {:<8}  {:<12}  {:>6}  {:>7}  {:>7}  LEFT",
                "NAME", "ID", "STATUS", "GPU", "$/H", "AGE", "COST"
            );
            for r in rows {
                println!(
                    "{:<12}  {:>9}  {:<8}  {:<12}  {:>6.2}  {:>7}  {:>7}  {}",
                    r.name,
                    r.id,
                    r.status,
                    r.gpu,
                    r.dph,
                    human(r.age),
                    format!("${:.2}", r.cost),
                    r.left.map_or("untracked".to_owned(), human)
                );
            }
        }
        0
    }))
}

/// `hopbox down <name>`.
pub fn execute_down(name: &str, state_dir: &Path) -> i32 {
    report(down(&vast(), name, state_dir).map(|msg| {
        println!("{msg}");
        0
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::vast::tests::{FakeCli, fixture};
    use std::cell::Cell;

    fn args() -> UpArgs {
        UpArgs {
            gpu: "RTX_4090".into(),
            ttl: "2h".into(),
            max_price: Some(0.6),
            name: Some("gpu1".into()),
            disk: 30,
            image: "img".into(),
            yes_spend: false,
            taken: vec![],
        }
    }

    fn deps<'a>(
        confirm: &'a dyn Fn(&str) -> Option<bool>,
        ssh: &'a dyn Fn(&Instance) -> bool,
    ) -> Deps<'a> {
        Deps {
            now: &|| 1_700_000_000,
            sleep: &|_| {},
            confirm,
            ssh_ready: ssh,
            poll: Duration::ZERO,
            max_polls: 3,
        }
    }

    #[test]
    fn name_validation() {
        assert!(valid_name("gpu1") && valid_name("a-b-9"));
        for bad in ["", "-x", "GPU", "a_b", "a b", "a/b", &"x".repeat(33)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn up_requires_yes_spend_without_tty() {
        let d = tempfile::tempdir().unwrap();
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("search-4090.json"))]),
        };
        let err = up(&v, &args(), d.path(), &deps(&|_| None, &|_| true)).unwrap_err();
        assert_eq!(err.0, exit::USAGE);
        assert!(
            err.1.contains("--yes-spend") && err.1.contains("$0.45/h"),
            "{}",
            err.1
        );
        assert_eq!(v.cli.calls.borrow().len(), 1, "nothing but the search ran");
    }

    #[test]
    fn up_declined_rents_nothing() {
        let d = tempfile::tempdir().unwrap();
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("search-4090.json"))]),
        };
        let asked = Cell::new(false);
        let confirm = |p: &str| {
            asked.set(p.contains("up to $0.91 for 2h"));
            Some(false)
        };
        assert_eq!(
            up(&v, &args(), d.path(), &deps(&confirm, &|_| true))
                .unwrap_err()
                .0,
            1
        );
        assert!(asked.get(), "pitch shows the TTL total");
        assert_eq!(v.cli.calls.borrow().len(), 1);
    }

    #[test]
    fn up_happy_path_records_host() {
        let d = tempfile::tempdir().unwrap();
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("search-4090.json")),
                (0, &fixture("create.json")),
                (0, &fixture("show-loading.json")),
                (0, &fixture("show-running.json")),
                (0, "ssh://root@1.2.3.4:40022\n"),
            ]),
        };
        let a = UpArgs {
            yes_spend: true,
            ..args()
        };
        let inst = up(
            &v,
            &a,
            d.path(),
            &deps(&|_| panic!("no prompt with --yes-spend"), &|_| true),
        )
        .unwrap();
        assert_eq!((inst.id, inst.ssh_port), (31337, 40022));
        assert_eq!(inst.deadline, 1_700_000_000 + 7_200);
        assert_eq!(state::load(&state::path(d.path())).unwrap(), vec![inst]);
    }

    #[test]
    fn up_destroys_failed_instance() {
        let d = tempfile::tempdir().unwrap();
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("search-4090.json")),
                (0, &fixture("create.json")),
                (0, &fixture("show-exited.json")),
                (0, "destroying instance 31337."),
            ]),
        };
        let err = up(&v, &args(), d.path(), &deps(&|_| Some(true), &|_| true)).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(
            err.1.contains("exited") && err.1.contains("destroyed instance 31337"),
            "{}",
            err.1
        );
        let calls = v.cli.calls.borrow();
        assert_eq!(
            calls.last().unwrap(),
            &["destroy", "instance", "31337", "-y"]
        );
        assert!(state::load(&state::path(d.path())).unwrap().is_empty());
    }

    #[test]
    fn up_times_out_and_destroys() {
        let d = tempfile::tempdir().unwrap();
        let loading = fixture("show-loading.json");
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("search-4090.json")),
                (0, &fixture("create.json")),
                (0, &loading),
                (0, &loading),
                (0, &loading),
                (0, "destroying instance 31337."),
            ]),
        };
        let err = up(&v, &args(), d.path(), &deps(&|_| Some(true), &|_| true)).unwrap_err();
        assert!(err.1.contains("did not start in time"), "{}", err.1);
        assert!(state::load(&state::path(d.path())).unwrap().is_empty());
    }

    fn recorded(d: &Path, name: &str, id: u64, deadline: u64) {
        let mut list = state::load(&state::path(d)).unwrap();
        list.push(Instance {
            name: name.into(),
            provider: "vast".into(),
            id,
            ssh_user: "root".into(),
            ssh_host: "1.2.3.4".into(),
            ssh_port: 40022,
            deadline,
            dph: 0.45,
        });
        state::save(&state::path(d), &list).unwrap();
    }

    #[test]
    fn up_rejects_huge_ttl_before_anything() {
        let d = tempfile::tempdir().unwrap();
        let v = Vast {
            cli: FakeCli::with(&[]),
        };
        let a = UpArgs {
            ttl: "100000000000w".into(),
            yes_spend: true,
            ..args()
        };
        assert_eq!(
            up(&v, &a, d.path(), &deps(&|_| Some(true), &|_| true))
                .unwrap_err()
                .0,
            exit::USAGE
        );
        let a = UpArgs {
            ttl: "31d".into(),
            yes_spend: true,
            ..args()
        };
        assert_eq!(
            up(&v, &a, d.path(), &deps(&|_| Some(true), &|_| true))
                .unwrap_err()
                .0,
            exit::USAGE
        );
        assert!(v.cli.calls.borrow().is_empty());
    }

    #[test]
    fn up_destroys_when_state_cannot_be_saved() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        state::save(&state::path(d.path()), &[]).unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(d.path().join("probe"), "").is_ok() {
            return; // root: permissions do not stop writes
        }
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("search-4090.json")),
                (0, &fixture("create.json")),
                (0, "destroying instance 31337."),
            ]),
        };
        let a = UpArgs {
            yes_spend: true,
            ..args()
        };
        let err = up(&v, &a, d.path(), &deps(&|_| Some(true), &|_| true)).unwrap_err();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(err.1.contains("destroyed instance 31337"), "{}", err.1);
        assert_eq!(
            v.cli.calls.borrow().last().unwrap(),
            &["destroy", "instance", "31337", "-y"]
        );
    }

    #[test]
    fn down_of_a_gone_instance_forgets_it() {
        let d = tempfile::tempdir().unwrap();
        recorded(d.path(), "gpu1", 31337, 4_000_000_000);
        let v = Vast {
            cli: FakeCli::with(&[(0, r#"{"instances": null}"#)]),
        };
        assert_eq!(down(&v, "gpu1", d.path()).unwrap(), "gpu1 was already gone");
        assert!(state::load(&state::path(d.path())).unwrap().is_empty());
    }

    #[test]
    fn stale_state_lock_is_broken() {
        let d = tempfile::tempdir().unwrap();
        let lock = d.path().join("instances.lock");
        std::fs::create_dir(&lock).unwrap();
        assert!(
            std::process::Command::new("touch")
                .args(["-d", "5 minutes ago"])
                .arg(&lock)
                .status()
                .unwrap()
                .success()
        );
        let n = with_state(d.path(), |list| Ok(list.len())).unwrap();
        assert_eq!(n, 0);
        assert!(!lock.exists(), "lock released");
    }

    #[test]
    fn ls_rechecks_before_forgetting() {
        // A record missing from the list snapshot (a concurrent `up`) is kept if show finds it.
        let d = tempfile::tempdir().unwrap();
        recorded(d.path(), "new", 777, 4_000_000_000);
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("instances.json")),
                (
                    0,
                    r#"{"id": 777, "actual_status": "loading", "label": "hopbox-new", "dph_total": 0.4}"#,
                ),
            ]),
        };
        let (notes, _) = ls(&v, d.path(), 1_700_000_200).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(state::load(&state::path(d.path())).unwrap().len(), 1);
    }

    #[test]
    fn lock_of_a_dead_process_is_broken_even_when_fresh() {
        let d = tempfile::tempdir().unwrap();
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        let pid = dead.id();
        dead.wait().unwrap();
        let lock = d.path().join("instances.lock");
        std::fs::create_dir(&lock).unwrap();
        std::fs::write(lock.join("pid"), pid.to_string()).unwrap();
        assert_eq!(with_state(d.path(), |l| Ok(l.len())).unwrap(), 0);
        assert!(!lock.exists());
    }

    #[test]
    fn down_checks_label() {
        let d = tempfile::tempdir().unwrap();
        recorded(d.path(), "gpu1", 424242, 4_000_000_000);
        let v = Vast {
            cli: FakeCli::with(&[(
                0,
                r#"{"id": 424242, "actual_status": "running", "label": "my-own-training", "dph_total": 1.2}"#,
            )]),
        };
        let err = down(&v, "gpu1", d.path()).unwrap_err();
        assert!(err.1.contains("refusing"), "{}", err.1);
        assert_eq!(v.cli.calls.borrow().len(), 1, "never destroyed");
        assert_eq!(state::load(&state::path(d.path())).unwrap().len(), 1);
    }

    #[test]
    fn down_destroys_and_forgets() {
        let d = tempfile::tempdir().unwrap();
        recorded(d.path(), "gpu1", 31337, 4_000_000_000);
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("show-running.json")),
                (0, "destroying instance 31337."),
            ]),
        };
        assert_eq!(down(&v, "gpu1", d.path()).unwrap(), "destroyed gpu1");
        assert_eq!(
            v.cli.calls.borrow()[1],
            ["destroy", "instance", "31337", "-y"]
        );
        assert!(state::load(&state::path(d.path())).unwrap().is_empty());
    }

    #[test]
    fn ls_sweeps_expired() {
        let d = tempfile::tempdir().unwrap();
        recorded(d.path(), "gpu1", 31337, 1_700_000_100); // expired at now=1_700_000_200
        recorded(d.path(), "gone", 999, 4_000_000_000); // not on vast anymore
        let v = Vast {
            cli: FakeCli::with(&[
                (0, &fixture("instances.json")),
                (0, "destroying instance 31337."),
                (0, r#"{"instances": null}"#),
            ]),
        };
        let (notes, rows) = ls(&v, d.path(), 1_700_000_200).unwrap();
        assert_eq!(
            notes,
            [
                "destroyed gpu1: past its deadline",
                "removed gone: no longer on vast"
            ]
        );
        assert_eq!(
            v.cli.calls.borrow()[1],
            ["destroy", "instance", "31337", "-y"]
        );
        // The other hopbox-labelled instance is shown as untracked; the user's own is not.
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), rows[0].left), ("stray", None));
        assert!(state::load(&state::path(d.path())).unwrap().is_empty());
    }
}
