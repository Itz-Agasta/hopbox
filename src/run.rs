//! `hopbox run`: preflight, sync, lock, exec, stream, report.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::cli::{RunArgs, command_string};
use crate::config::{self, RunFlags};
use crate::exit;
use crate::remote_script::{
    Caps, LOCK_TIMEOUT_LINE, RunScript, UnitName, oom_check_script, stop_script,
};
use crate::status::{Source, Status};
use crate::sync::{SyncError, sync};
use crate::transport::{Transport, TransportError};
use crate::workspace::{LocalTree, local_hostname, remote_workspace, workspace_name};

/// Internal hook: lock wait in seconds. Used by the integration tests.
pub const LOCK_WAIT_ENV: &str = "HOPBOX_LOCK_WAIT_SECS";
/// Default time to wait for another run holding the workspace lock.
pub const DEFAULT_LOCK_WAIT_SECS: u64 = 600;

/// Process context the run needs, gathered once in `main`.
#[derive(Debug, Clone)]
pub struct Env {
    pub cwd: PathBuf,
    pub global_config: Option<PathBuf>,
    pub stdout_tty: bool,
    pub lock_wait_secs: u64,
    pub log_dir: PathBuf,
}

pub fn execute(args: &RunArgs, env: &Env) -> Status {
    let start = Instant::now();
    let mut host = "-".to_owned();
    match run(args, env, start, &mut host) {
        Ok(status) => status,
        Err((code, msg)) => Status::hopbox_error(code, &host, start.elapsed(), msg),
    }
}

type Failure = (i32, String);

fn usage(msg: impl std::fmt::Display) -> Failure {
    (exit::USAGE.into(), msg.to_string())
}

fn run(args: &RunArgs, env: &Env, start: Instant, host: &mut String) -> Result<Status, Failure> {
    for (flag, used) in [
        ("--fetch", !args.fetch.is_empty()),
        ("--env", !args.env.is_empty()),
        ("--task", args.task.is_some()),
    ] {
        if used {
            return Err(usage(format!("{flag} is not implemented yet")));
        }
    }
    let cmd = command_string(&args.cmd).ok_or_else(|| usage("no command: pass it after --"))?;

    let tree = LocalTree::discover(&env.cwd).map_err(usage)?;
    let global = match &env.global_config {
        Some(p) => config::load_global(p).map_err(usage)?,
        None => config::GlobalConfig::default(),
    };
    let repo = config::load_repo(&tree.root).map_err(usage)?;
    let flags = RunFlags {
        host: args.host.clone(),
        mem: args.mem.clone(),
        include: args.include.clone(),
        fetch: args.fetch.clone(),
        env: args.env.clone(),
    };
    let r = config::resolve(&global, &repo, &flags).map_err(usage)?;
    *host = r.host_name.clone();

    let t = Transport::new(&r.ssh_dest).map_err(usage)?;
    let ws = remote_workspace(
        &r.workspace_root,
        &workspace_name(&tree.root, &local_hostname()),
    );
    let pre = t.preflight(&ws).map_err(|e| match e {
        TransportError::Unreachable { .. } => (exit::CONNECT.into(), e.to_string()),
        _ => usage(e),
    })?;

    if !args.no_sync {
        sync(&t, &tree, &ws, &r).map_err(|e| match e {
            SyncError::Rsync(code @ (12 | 255)) => (
                exit::CONNECT.into(),
                format!("sync failed: rsync exit {code}"),
            ),
            SyncError::Rsync(code) => (code, format!("sync failed: rsync exit {code}")),
            other => usage(other),
        })?;
    }

    let caps = pre.caps.then(|| Caps {
        unit: UnitName::random(),
        memory_max: r.memory_max.clone(),
        cpu_weight: r.cpu_weight,
    });
    if caps.is_none() {
        eprintln!("hopbox: warning: no resource caps on {}", r.host_name);
    }
    let script = RunScript {
        workspace: &ws,
        subdir: &tree.subdir,
        lock_wait_secs: env.lock_wait_secs,
        caps: caps.as_ref(),
        shell: &r.shell,
        cmd: &cmd,
    }
    .render();

    let log = match args.tail {
        Some(_) => Some(open_log(&env.log_dir, caps.as_ref())?),
        None => None,
    };
    let interrupt = install_interrupt(&t, caps.as_ref().map(|c| c.unit.clone()));

    let mut child = t.spawn(&script, env.stdout_tty).map_err(usage)?;
    CHILD_PID.store(child.id(), Ordering::SeqCst);
    let out = forward(
        child.stdout.take().expect("piped"),
        Sink::Stdout,
        log.as_ref(),
    );
    let err = forward(
        child.stderr.take().expect("piped"),
        Sink::Stderr,
        log.as_ref(),
    );
    let status = child
        .wait()
        .map_err(|e| usage(format!("waiting for ssh: {e}")))?;
    if interrupt.load(Ordering::SeqCst) {
        // Under ControlMaster the master holds our pipes until the remote command ends, so
        // joining the forwarders could wait on an orphaned job. Report and leave now.
        let mut st = Status::hopbox_error(
            exit::INTERRUPTED.into(),
            &r.host_name,
            start.elapsed(),
            "interrupted",
        );
        st.log = log.as_ref().map(|l| l.path.clone());
        return Ok(st);
    }
    let last_out = out.join().unwrap_or_default();
    let last_err = err.join().unwrap_or_default();
    CHILD_PID.store(0, Ordering::SeqCst);

    let mut result = Status {
        exit: exit_code(status),
        source: Source::Remote,
        host: r.host_name.clone(),
        dur: start.elapsed(),
        log: log.as_ref().map(|l| l.path.clone()),
        oom: false,
        error: None,
    };
    if let (Some(n), Some(l)) = (args.tail, &log) {
        print_tail(&l.path, n);
    }

    if result.exit == i32::from(exit::TIMEOUT)
        && [last_out.as_str(), last_err.as_str()].contains(&LOCK_TIMEOUT_LINE)
    {
        result.source = Source::Hopbox;
        result.error = Some("lock wait timed out".into());
        return Ok(result);
    }
    if let Some(c) = &caps {
        if let Ok(o) = t.capture(&oom_check_script(&c.unit)) {
            if String::from_utf8_lossy(&o.stdout).trim() == "oom-kill" {
                result.exit = exit::OOM.into();
                result.oom = true;
            }
        }
    }
    Ok(result)
}

fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

struct Log {
    path: PathBuf,
    file: Mutex<File>,
}

fn open_log(dir: &Path, caps: Option<&Caps>) -> Result<Log, Failure> {
    std::fs::create_dir_all(dir)
        .map_err(|e| usage(format!("cannot create {}: {e}", dir.display())))?;
    let id = caps.map_or_else(UnitName::random, |c| c.unit.clone());
    let path = dir.join(format!("{}.log", id.as_str()));
    let file =
        File::create(&path).map_err(|e| usage(format!("cannot create {}: {e}", path.display())))?;
    Ok(Log {
        path,
        file: Mutex::new(file),
    })
}

fn print_tail(path: &Path, n: usize) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = body.lines().collect();
    let mut stdout = io::stdout().lock();
    for line in &lines[lines.len().saturating_sub(n)..] {
        let _ = writeln!(stdout, "{line}");
    }
}

#[derive(Clone, Copy)]
enum Sink {
    Stdout,
    Stderr,
}

/// Copy a child stream to our stdout/stderr (or the log) as bytes arrive.
/// Returns the last complete line, without its line ending.
fn forward<R: Read + Send + 'static>(
    mut src: R,
    sink: Sink,
    log: Option<&Log>,
) -> JoinHandle<String> {
    let log = log.map(|l| {
        // Share the file handle across both forwarding threads.
        Arc::new(
            l.file
                .lock()
                .expect("log lock")
                .try_clone()
                .expect("clone log"),
        )
    });
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut current: Vec<u8> = Vec::new();
        let mut last = String::new();
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];
            match &log {
                Some(f) => {
                    let _ = (&**f).write_all(chunk);
                }
                None => {
                    let _ = match sink {
                        Sink::Stdout => io::stdout()
                            .lock()
                            .write_all(chunk)
                            .and_then(|_| io::stdout().flush()),
                        Sink::Stderr => io::stderr().lock().write_all(chunk),
                    };
                }
            }
            for &b in chunk {
                if b == b'\n' {
                    last = String::from_utf8_lossy(&current)
                        .trim_end_matches('\r')
                        .to_owned();
                    current.clear();
                } else {
                    current.push(b);
                }
            }
        }
        if !current.is_empty() {
            last = String::from_utf8_lossy(&current)
                .trim_end_matches('\r')
                .to_owned();
        }
        last
    })
}

static CHILD_PID: AtomicU32 = AtomicU32::new(0);

/// On Ctrl-C or SIGTERM: stop the remote scope (capped) or the local ssh (uncapped).
fn install_interrupt(t: &Transport, unit: Option<UnitName>) -> Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    static TARGET: Mutex<Option<(Transport, Option<UnitName>)>> = Mutex::new(None);
    *TARGET.lock().expect("interrupt target") = Some((t.clone(), unit));
    FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let seen = flag.clone();
        let _ = ctrlc::set_handler(move || {
            seen.store(true, Ordering::SeqCst);
            let target = TARGET.lock().ok().and_then(|g| g.clone());
            if let Some((t, Some(unit))) = &target {
                let _ = t.capture(&stop_script(unit));
            }
            let pid = CHILD_PID.load(Ordering::SeqCst);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
            }
        });
        flag
    })
    .clone()
}
