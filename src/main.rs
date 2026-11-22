use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use hopbox::cli::{self, Command};
use hopbox::config::global_config_path;
use hopbox::doctor;
use hopbox::exit;
use hopbox::run::{self, DEFAULT_LOCK_WAIT_SECS, LOCK_WAIT_ENV};
use hopbox::status::Status;

fn main() -> ExitCode {
    let cli = match cli::parse(std::env::args_os()) {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { exit::USAGE } else { 0 };
            let _ = e.print();
            if code != 0 {
                eprintln!(
                    "{}",
                    Status::hopbox_error(code.into(), "-", Duration::ZERO, "usage error").line()
                );
            }
            return ExitCode::from(code);
        }
    };
    match cli.command {
        Command::Run(args) => {
            let status = match env() {
                Ok(env) => run::execute(&args, &env),
                Err(e) => {
                    Status::hopbox_error(exit::USAGE.into(), "-", Duration::ZERO, format!("{e:#}"))
                }
            };
            print_status(if args.json {
                status.json()
            } else {
                status.line()
            });
            ExitCode::from(u8::try_from(status.exit).unwrap_or(1))
        }
        Command::Doctor { host } => match env() {
            Ok(env) => {
                ExitCode::from(u8::try_from(doctor::execute(host.as_deref(), &env)).unwrap_or(1))
            }
            Err(e) => {
                eprintln!("hopbox: {e:#}");
                ExitCode::from(exit::USAGE)
            }
        },
    }
}

/// Print the final status without letting a stalled stderr reader keep hopbox alive: after an
/// interrupt, a detached output forwarder may still be blocked holding the stderr lock.
fn print_status(line: String) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        eprintln!("{line}");
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(Duration::from_secs(2));
}

fn env() -> anyhow::Result<run::Env> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let home = var("HOME");
    let lock_wait_secs = match var(LOCK_WAIT_ENV) {
        Some(v) => v.to_string_lossy().parse()?,
        None => DEFAULT_LOCK_WAIT_SECS,
    };
    let state = var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    Ok(run::Env {
        cwd: std::env::current_dir()?,
        global_config: global_config_path(var("XDG_CONFIG_HOME").as_deref(), home.as_deref()),
        stdout_tty: std::io::stdout().is_terminal(),
        lock_wait_secs,
        log_dir: state.join("hopbox/logs"),
    })
}
