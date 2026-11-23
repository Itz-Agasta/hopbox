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
use crate::config::{self, Resolved, RunFlags};
use crate::exit;
use crate::quote::shell_quote;
use crate::remote_script::{
    BG_STARTED_LINE, BgStart, Caps, LOCKED_LINE, LockScript, RemotePath, RunScript, UnitName,
    bg_wrapper_script, env_remove_script, env_write_script, oom_check_script, stop_script,
};
use crate::status::{Source, Status};
use crate::sync::{SyncError, relative_path, sync};
use crate::transport::{PortSpec, Transport, TransportError};
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
    /// `$XDG_STATE_HOME/hopbox`: holds the background job index.
    pub state_dir: PathBuf,
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
    let tree = LocalTree::discover(&env.cwd).map_err(usage)?;
    let mut global = match &env.global_config {
        Some(p) => config::load_global(p).map_err(usage)?,
        None => config::GlobalConfig::default(),
    };
    crate::state::add_rented(&mut global, &env.state_dir);
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
    let cmd = command_for(args, &r)?;
    if args.bg && (!args.fetch.is_empty() || args.tail.is_some() || !args.port.is_empty()) {
        return Err(usage(
            "--bg cannot be combined with --fetch, --tail or --port",
        ));
    }
    let ports = args
        .port
        .iter()
        .map(|p| PortSpec::parse(p).map_err(usage))
        .collect::<Result<Vec<_>, _>>()?;
    if args.bg && !r.fetch.is_empty() {
        eprintln!(
            "hopbox: warning: fetch paths from .hopbox.toml are not copied back for --bg jobs"
        );
    }
    let fetch = r
        .fetch
        .iter()
        .map(|p| {
            relative_path(p).ok_or_else(|| {
                usage(format!(
                    "invalid fetch path {p:?}: use a path relative to the current directory without '..'"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let env_file_body = env_file_contents(&args.env, &repo.env)?;

    let t = Transport::for_host(&r).map_err(usage)?;
    let ws_name = workspace_name(&tree.root, &local_hostname());
    let ws = remote_workspace(&r.workspace_root, &ws_name);
    let locks = RemotePath::parse(&r.workspace_root).join("locks");
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
    let env_name = (!env_file_body.is_empty()).then(|| format!(".hopbox-env-{}", unit.id()));
    let script = RunScript {
        workspace: &ws,
        subdir: &tree.subdir,
        caps: caps.as_ref(),
        shell: &r.shell,
        cmd: &cmd,
        env_file: env_name.as_deref(),
    }
    .render();
    let jobs = RemotePath::parse(&r.workspace_root).join("jobs");
    let meta = [
        format!("ws={ws_name}"),
        format!("cmd={}", cmd.replace(['\n', '\r'], " ")),
    ];
    let wrapper = args.bg.then(|| {
        bg_wrapper_script(
            &jobs.join(unit.id()),
            &script,
            caps.as_ref().map(|c| &c.unit),
        )
    });

    // One lock for sync, job and fetch, so a second run can't change files under this one.
    // A background job inherits it and holds it until the job ends.
    let holder = acquire_lock(
        &t,
        &LockScript {
            workspace: &ws,
            locks: &locks,
            name: &ws_name,
            wait_secs: env.lock_wait_secs,
            bg: wrapper.as_deref().map(|wrapper| BgStart {
                jobs: &jobs,
                id: unit.id(),
                meta: &meta,
                wrapper,
            }),
        },
        &interrupt,
    )?;

    // Forwards live only for this run; the guard stops them on every return path.
    let forwards = Forwards::open(&t, &ports, &interrupt)?;

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
    // Fallible local setup first, so nothing below can fail with secrets left on the remote.
    let log = match args.tail {
        Some(_) => Some(open_log(&env.log_dir, &unit)?),
        None => None,
    };
    let env_file = if let Some(env_file) = env_name {
        let out = t.capture_input(
            &env_write_script(&ws, &env_file),
            env_file_body.as_bytes(),
            &|pid| track_pid(pid, &interrupt),
        );
        CHILD_PID.store(0, Ordering::SeqCst);
        if interrupt.load(Ordering::SeqCst) {
            let _ = t.capture(&env_remove_script(&ws, &env_file));
            return Err(interrupted());
        }
        let out = out.map_err(usage)?;
        if !out.status.success() {
            return Err(usage(format!(
                "cannot write the env file on {}: {}",
                r.host_name,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Some(env_file)
    } else {
        None
    };
    if caps.is_none() {
        eprintln!("hopbox: warning: no resource caps on {}", r.host_name);
    }
    if args.bg {
        if let Err((code, e)) = holder.launch(&interrupt) {
            if let Some(f) = &env_file {
                let _ = t.capture(&env_remove_script(&ws, f));
            }
            if code == i32::from(exit::INTERRUPTED) {
                return Err((code, e));
            }
            return Err(usage(format!(
                "cannot start the job on {}: {e}",
                r.host_name
            )));
        }
        let id = unit.id().to_owned();
        if let Err(e) = crate::jobs::record(&env.state_dir.join("jobs.tsv"), &id, &r.host_name) {
            eprintln!("hopbox: warning: cannot record job {id} locally: {e}");
        }
        println!("{id}");
        return Ok(Status {
            exit: 0,
            source: Source::Hopbox,
            host: r.host_name.clone(),
            dur: start.elapsed(),
            log: None,
            oom: false,
            error: None,
            job: Some(id),
        });
    }

    let mut child = match t.spawn(&script, env.stdout_tty) {
        Ok(c) => c,
        Err(e) => {
            if let Some(f) = &env_file {
                let _ = t.capture(&env_remove_script(&ws, f));
            }
            return Err(usage(e));
        }
    };
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
        job: None,
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
    fetch_back(&t, &ws, &tree, &fetch, &interrupt);
    if interrupt.load(Ordering::SeqCst) {
        return Err(interrupted());
    }
    // Close our forwards before the lock goes, so a queued run can take the same ports.
    drop(forwards);
    holder.release();
    Ok(result)
}

/// `--port` forwards, one owned ssh process each, stopped when dropped.
struct Forwards {
    procs: Vec<Child>,
}

/// How long a new forward has to start accepting connections.
const FORWARD_READY: Duration = Duration::from_secs(15);

impl Forwards {
    fn open(
        t: &Transport,
        ports: &[PortSpec],
        interrupt: &AtomicBool,
    ) -> Result<Forwards, Failure> {
        let mut f = Forwards { procs: Vec::new() };
        for &p in ports {
            // A busy port (another run's forward or any other listener) is never shared.
            // On error `f` drops here and stops the forwards already open.
            drop(
                std::net::TcpListener::bind(("127.0.0.1", p.local))
                    .map_err(|e| usage(format!("cannot forward port {}: {e}", p.local)))?,
            );
            let mut child = t.spawn_forward(p).map_err(usage)?;
            let ready = wait_listening(&mut child, p.local, interrupt);
            f.procs.push(child);
            match ready {
                Listen::Ready => {}
                Listen::Interrupted => return Err(interrupted()),
                Listen::Failed => {
                    return Err(usage(format!(
                        "cannot forward port {}: ssh did not start listening",
                        p.local
                    )));
                }
            }
        }
        Ok(f)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Listen {
    Ready,
    Failed,
    Interrupted,
}

/// Wait until something accepts on `127.0.0.1:<port>` while `child` is still running.
/// Ctrl-C ends the wait at once (the caller's guard then stops the child).
fn wait_listening(child: &mut Child, port: u16, interrupt: &AtomicBool) -> Listen {
    let deadline = Instant::now() + FORWARD_READY;
    while Instant::now() < deadline {
        if interrupt.load(Ordering::SeqCst) {
            return Listen::Interrupted;
        }
        if !matches!(child.try_wait(), Ok(None)) {
            return Listen::Failed;
        }
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Listen::Ready;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Listen::Failed
}

impl Drop for Forwards {
    fn drop(&mut self) {
        for c in &mut self.procs {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// The lock-holder ssh session. Closing its stdin releases the lock.
struct LockHolder {
    child: Child,
    stdout: BufReader<std::process::ChildStdout>,
}

impl LockHolder {
    /// Tell a background lock holder to start its job, and wait for it to confirm.
    /// Ctrl-C while waiting kills the holder ssh, so a stalled start can't hang the run.
    fn launch(mut self, interrupt: &AtomicBool) -> Result<(), Failure> {
        let mut stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| usage("lock holder has no stdin"))?;
        track_child(&self.child, interrupt);
        let sent = stdin.write_all(b"go\n");
        drop(stdin);
        let mut line = String::new();
        if sent.is_ok() {
            let _ = self.stdout.read_line(&mut line);
        }
        let _ = self.child.wait();
        CHILD_PID.store(0, Ordering::SeqCst);
        if interrupt.load(Ordering::SeqCst) {
            return Err(interrupted());
        }
        if line.trim_end() == BG_STARTED_LINE {
            Ok(())
        } else {
            Err(usage(
                "the remote did not confirm the start (job dir exists or is not writable)",
            ))
        }
    }

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
    lock: &LockScript,
    interrupt: &AtomicBool,
) -> Result<LockHolder, Failure> {
    let mut child = t.spawn_holder(&lock.render()).map_err(usage)?;
    track_child(&child, interrupt);
    let mut line = String::new();
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let _ = stdout.read_line(&mut line);
    CHILD_PID.store(0, Ordering::SeqCst);
    if line.trim_end() == LOCKED_LINE {
        return Ok(LockHolder { child, stdout });
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

/// The command from `--task NAME` (plus extra words) or from the words after `--`.
fn command_for(args: &RunArgs, r: &Resolved) -> Result<String, Failure> {
    let extra = command_string(&args.cmd);
    let Some(name) = &args.task else {
        return extra.ok_or_else(|| usage("no command: pass it after -- or use --task NAME"));
    };
    let base = r.tasks.get(name).ok_or_else(|| {
        let known = if r.tasks.is_empty() {
            "none".to_owned()
        } else {
            r.tasks.keys().cloned().collect::<Vec<_>>().join(", ")
        };
        usage(format!("unknown task {name:?}; tasks: {known}"))
    })?;
    Ok(match extra {
        Some(extra) => format!("{base} {extra}"),
        None => base.clone(),
    })
}

/// `export NAME='value'` lines for the env file. `--env` names must be set locally;
/// names from the repo config that are unset are skipped with a warning.
fn env_file_contents(flag_names: &[String], repo_names: &[String]) -> Result<String, Failure> {
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    let all = flag_names
        .iter()
        .map(|n| (n, true))
        .chain(repo_names.iter().map(|n| (n, false)));
    for (name, from_flag) in all {
        if seen.contains(&name.as_str()) {
            continue;
        }
        seen.push(name);
        let valid = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(usage(format!("invalid environment variable name {name:?}")));
        }
        // The run script keeps its own state in `__hopbox_*`; a forwarded value could break cleanup.
        if name.starts_with("__hopbox_") {
            return Err(usage(format!(
                "environment variable name {name:?} uses the reserved __hopbox_ prefix"
            )));
        }
        match std::env::var(name) {
            Ok(value) => out += &format!("export {name}={}\n", shell_quote(&value)),
            Err(_) if from_flag => {
                return Err(usage(format!(
                    "--env {name}: not set locally (or not UTF-8)"
                )));
            }
            Err(_) => eprintln!("hopbox: warning: env {name} is not set locally; skipping"),
        }
    }
    Ok(out)
}

/// Copy each path back from the remote copy of the current subdirectory. Failures warn only.
fn fetch_back(
    t: &Transport,
    ws: &RemotePath,
    tree: &LocalTree,
    paths: &[String],
    interrupt: &AtomicBool,
) {
    let local_cwd = tree.root.join(&tree.subdir);
    for p in paths {
        if interrupt.load(Ordering::SeqCst) {
            return;
        }
        let rel_parent = Path::new(p).parent().unwrap_or(Path::new(""));
        if symlink_on_path(&local_cwd, rel_parent) {
            eprintln!("hopbox: warning: fetch {p} skipped: a parent directory is a symlink");
            continue;
        }
        let mut remote = ws.rsync_arg();
        for part in [tree.subdir.as_str(), p.as_str()] {
            if !part.is_empty() {
                remote.push('/');
                remote.push_str(part);
            }
        }
        let parent = local_cwd.join(Path::new(p).parent().unwrap_or(Path::new("")));
        let status = std::fs::create_dir_all(&parent).and_then(|_| {
            let mut dest = parent.into_os_string();
            dest.push("/");
            let mut child = std::process::Command::new("rsync")
                .args(["-a", "-s", "-e"])
                .arg(t.rsync_rsh())
                .arg(format!("{}:{remote}", t.dest()))
                .arg(dest)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            track_pid(child.id(), interrupt);
            let status = child.wait();
            CHILD_PID.store(0, Ordering::SeqCst);
            status
        });
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!(
                "hopbox: warning: fetch {p} failed (rsync exit {})",
                s.code().unwrap_or(-1)
            ),
            Err(e) => eprintln!("hopbox: warning: fetch {p} failed ({e})"),
        }
    }
}

/// Whether any existing directory between `base` and `base/rel` is a symlink, which could
/// redirect a fetch outside the checkout.
fn symlink_on_path(base: &Path, rel: &Path) -> bool {
    let mut cur = base.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
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
    fn forward_wait_stops_on_interrupt() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let interrupt = AtomicBool::new(true);
        let t = Instant::now();
        assert_eq!(
            wait_listening(&mut child, port, &interrupt),
            Listen::Interrupted
        );
        assert!(t.elapsed() < Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
    }

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

    #[test]
    fn env_rejects_reserved_hopbox_prefix() {
        let err = env_file_contents(&["__hopbox_rm".to_owned()], &[]).unwrap_err();
        assert_eq!(err.0, i32::from(exit::USAGE));
        assert!(err.1.contains("reserved"), "{}", err.1);
        let err = env_file_contents(&[], &["__hopbox_x".to_owned()]).unwrap_err();
        assert!(err.1.contains("reserved"), "{}", err.1);
    }
}
