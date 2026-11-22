//! `hopbox run`: preflight, lock, sync, exec, stream, report.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::cli::{RunArgs, command_string};
use crate::config::{self, RunFlags};
use crate::exit;
use crate::remote_script::{
    Caps, LOCKED_LINE, LockScript, RunScript, UnitName, oom_check_script, stop_script,
};
use crate::status::{Source, Status};
use crate::sync::{SyncError, sync};
use crate::transport::{Transport, TransportError};
use crate::workspace::{LocalTree, local_hostname, remote_workspace, workspace_name};

/// Internal hook: lock wait in seconds. Used by the integration tests.
pub const LOCK_WAIT_ENV: &str = "HOPBOX_LOCK_WAIT_SECS";
/// Default time to wait for another run holding the workspace lock.
pub const DEFAULT_LOCK_WAIT_SECS: u64 = 600;
/// How long an interrupted run waits for the remote scope to stop.
const STOP_GRACE: Duration = Duration::from_secs(5);

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

fn interrupted() -> Failure {
    (exit::INTERRUPTED.into(), "interrupted".to_owned())
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

    let unit = UnitName::random();
    let caps = pre.caps.then(|| Caps {
        unit: unit.clone(),
        memory_max: r.memory_max.clone(),
        cpu_weight: r.cpu_weight,
    });
    let interrupt = install_interrupt(&t, caps.as_ref().map(|c| c.unit.clone()));

    // One lock for sync, job and fetch, so a second run can't change files under this one.
    let holder = acquire_lock(&t, &ws, env.lock_wait_secs, &interrupt)?;

    if !args.no_sync {
        let synced = sync(&t, &tree, &ws, &r, &|pid| track_pid(pid, &interrupt));
        CHILD_PID.store(0, Ordering::SeqCst);
        if interrupt.load(Ordering::SeqCst) {
            return Err(interrupted());
        }
        synced.map_err(|e| match e {
            SyncError::Rsync(code @ (12 | 255)) => (
                exit::CONNECT.into(),
                format!("sync failed: rsync exit {code}"),
            ),
            SyncError::Rsync(code) => (code, format!("sync failed: rsync exit {code}")),
            other => usage(other),
        })?;
    }
    if caps.is_none() {
        eprintln!("hopbox: warning: no resource caps on {}", r.host_name);
    }
    let script = RunScript {
        workspace: &ws,
        subdir: &tree.subdir,
        caps: caps.as_ref(),
        shell: &r.shell,
        cmd: &cmd,
    }
    .render();

    let log = match args.tail {
        Some(_) => Some(open_log(&env.log_dir, &unit)?),
        None => None,
    };

    let mut child = t.spawn(&script, env.stdout_tty).map_err(usage)?;
    track_child(&child, &interrupt);
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
        // joining the forwarders could wait on an orphaned job. Give the scope stop a moment
        // to finish, then report and leave.
        wait_for_stop();
        let mut st = Status::hopbox_error(
            exit::INTERRUPTED.into(),
            &r.host_name,
            start.elapsed(),
            "interrupted",
        );
        st.log = log.as_ref().map(|l| l.path.clone());
        return Ok(st);
    }
    // A forwarder can block on a stalled stdout reader; keep Ctrl-C able to end the run.
    while !(out.is_finished() && err.is_finished()) {
        if interrupt.load(Ordering::SeqCst) {
            wait_for_stop();
            return Err(interrupted());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let write_errors: Vec<String> = [out, err]
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect();
    CHILD_PID.store(0, Ordering::SeqCst);

    let mut result = Status {
        exit: exit_code(status),
        source: Source::Remote,
        host: r.host_name.clone(),
        dur: start.elapsed(),
        log: log.as_ref().map(|l| l.path.clone()),
        oom: false,
        error: write_errors.first().map(|e| format!("log incomplete: {e}")),
    };
    if let (Some(n), Some(l)) = (args.tail, &log) {
        match tail_lines(&l.path, n) {
            Ok(lines) => {
                let mut stdout = io::stdout().lock();
                for line in lines {
                    let _ = writeln!(stdout, "{line}");
                }
            }
            Err(e) => result.error = Some(format!("cannot read log: {e}")),
        }
    }
    if let Some(c) = &caps {
        if let Ok(o) = t.capture(&oom_check_script(&c.unit)) {
            if String::from_utf8_lossy(&o.stdout).trim() == "oom-kill" {
                result.exit = exit::OOM.into();
                result.oom = true;
            }
        }
    }
    holder.release();
    Ok(result)
}

/// The lock-holder ssh session. Closing its stdin releases the lock.
struct LockHolder {
    child: Child,
}

impl LockHolder {
    fn release(mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        // Early returns: closing stdin is enough for the remote side to let go.
        drop(self.child.stdin.take());
    }
}

/// Start the lock holder and wait until it reports `locked` (or fails).
fn acquire_lock(
    t: &Transport,
    ws: &crate::remote_script::RemotePath,
    wait_secs: u64,
    interrupt: &AtomicBool,
) -> Result<LockHolder, Failure> {
    let script = LockScript {
        workspace: ws,
        wait_secs,
    }
    .render();
    let mut child = t.spawn_holder(&script).map_err(usage)?;
    track_child(&child, interrupt);
    let mut line = String::new();
    let stdout = child.stdout.take().expect("piped");
    let _ = BufReader::new(stdout).read_line(&mut line);
    CHILD_PID.store(0, Ordering::SeqCst);
    if line.trim_end() == LOCKED_LINE {
        return Ok(LockHolder { child });
    }
    let status = child.wait().map_err(|e| usage(format!("lock: {e}")))?;
    if interrupt.load(Ordering::SeqCst) {
        return Err(interrupted());
    }
    Err(match status.code() {
        Some(124) => (exit::TIMEOUT.into(), "lock wait timed out".to_owned()),
        Some(255) => (
            exit::CONNECT.into(),
            format!("lost connection to {}", t.dest()),
        ),
        code => usage(format!("cannot lock the workspace (exit {code:?})")),
    })
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

fn open_log(dir: &Path, unit: &UnitName) -> Result<Log, Failure> {
    std::fs::create_dir_all(dir)
        .map_err(|e| usage(format!("cannot create {}: {e}", dir.display())))?;
    let path = dir.join(format!("{}.log", unit.as_str()));
    let file =
        File::create(&path).map_err(|e| usage(format!("cannot create {}: {e}", path.display())))?;
    Ok(Log {
        path,
        file: Mutex::new(file),
    })
}

/// The last `n` lines of a file, read as a stream (bounded memory), decoded lossily.
fn tail_lines(path: &Path, n: usize) -> io::Result<Vec<String>> {
    let mut keep: VecDeque<Vec<u8>> = VecDeque::with_capacity(n.min(1024));
    if n == 0 {
        return Ok(Vec::new());
    }
    for line in BufReader::new(File::open(path)?).split(b'\n') {
        if keep.len() == n {
            keep.pop_front();
        }
        keep.push_back(line?);
    }
    Ok(keep
        .into_iter()
        .map(|l| String::from_utf8_lossy(&l).into_owned())
        .collect())
}

#[derive(Clone, Copy)]
enum Sink {
    Stdout,
    Stderr,
}

/// Copy a child stream to our stdout/stderr (or the log) as bytes arrive.
/// Returns the first log write error, if any.
fn forward<R: Read + Send + 'static>(
    mut src: R,
    sink: Sink,
    log: Option<&Log>,
) -> JoinHandle<Option<String>> {
    let log = log.map(|l| {
        // Share the file handle across both forwarding threads.
        Arc::new(l.file.lock().expect("log lock").try_clone())
    });
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut write_error: Option<String> = None;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &buf[..n];
            match &log {
                Some(f) => {
                    let res = match &**f {
                        Ok(file) => (&*file).write_all(chunk),
                        Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
                    };
                    if let (Err(e), None) = (res, &write_error) {
                        write_error = Some(e.to_string());
                    }
                }
                None => {
                    let _ = match sink {
                        Sink::Stdout => {
                            let mut out = io::stdout().lock();
                            out.write_all(chunk).and_then(|_| out.flush())
                        }
                        Sink::Stderr => io::stderr().lock().write_all(chunk),
                    };
                }
            }
        }
        write_error
    })
}

static CHILD_PID: AtomicU32 = AtomicU32::new(0);
static STOP_DONE: Mutex<Option<Receiver<()>>> = Mutex::new(None);

/// Publish the child's pid for the signal handler; kill it at once if a signal already came.
fn track_child(child: &Child, interrupt: &AtomicBool) {
    track_pid(child.id(), interrupt);
}

fn track_pid(pid: u32, interrupt: &AtomicBool) {
    CHILD_PID.store(pid, Ordering::SeqCst);
    if interrupt.load(Ordering::SeqCst) {
        kill_pid(pid);
    }
}

fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

/// Wait (bounded) for the remote scope stop started by the signal handler.
fn wait_for_stop() {
    let rx = STOP_DONE.lock().ok().and_then(|mut g| g.take());
    if let Some(rx) = rx {
        let _ = rx.recv_timeout(STOP_GRACE);
    }
}

/// On Ctrl-C or SIGTERM: stop the remote scope (capped) in the background and terminate the
/// local ssh right away.
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
            if let Some((t, Some(unit))) = target {
                let (tx, rx) = mpsc::channel();
                if let Ok(mut g) = STOP_DONE.lock() {
                    *g = Some(rx);
                }
                std::thread::spawn(move || {
                    let _ = t.capture(&stop_script(&unit));
                    let _ = tx.send(());
                });
            }
            let pid = CHILD_PID.load(Ordering::SeqCst);
            if pid != 0 {
                kill_pid(pid);
            }
        });
        flag
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_lines_keeps_only_the_end() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        let body: String = (1..=1000).map(|i| format!("{i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        assert_eq!(tail_lines(&p, 2).unwrap(), ["999", "1000"]);
        assert!(tail_lines(&p, 0).unwrap().is_empty());
        assert_eq!(tail_lines(&p, 5000).unwrap().len(), 1000);
    }

    #[test]
    fn tail_lines_survives_invalid_utf8() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(&p, [b"bad \xff line\n".as_slice(), b"last\n"].concat()).unwrap();
        let lines = tail_lines(&p, 2).unwrap();
        assert_eq!(lines[1], "last");
        assert!(lines[0].starts_with("bad "));
    }
}
