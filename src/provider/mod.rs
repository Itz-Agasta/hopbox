//! Rental providers. hopbox drives each one through its own CLI and never holds its credentials.

pub mod vast;

use std::io;
use std::process::{Command, Stdio};

/// What a provider CLI call returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a provider CLI. Tests swap in recorded fixtures.
pub trait Cli {
    fn run(&self, args: &[String]) -> io::Result<CliOutput>;
}

/// The real CLI: `<program> <args…>`, stdin closed.
pub struct SystemCli {
    pub program: String,
}

impl Cli for SystemCli {
    fn run(&self, args: &[String]) -> io::Result<CliOutput> {
        let o = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        Ok(CliOutput {
            code: o.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        })
    }
}
