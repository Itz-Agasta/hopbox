//! Command-line interface.

use clap::{Args, Parser, Subcommand};

/// Run heavy builds and tests on another machine over SSH.
#[derive(Debug, Parser)]
#[command(name = "hopbox", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sync the repo, run a command on the remote, and stream the result.
    Run(RunArgs),
    /// Check that a host has what hopbox needs. Never installs anything.
    Doctor {
        /// Host from the global config (default: `default_host`).
        host: Option<String>,
    },
}

#[derive(Debug, Default, Args)]
pub struct RunArgs {
    /// Host from the global config (default: `default_host`).
    #[arg(long, value_name = "NAME")]
    pub host: Option<String>,
    /// Keep the full output in a log file and print only its last N lines.
    #[arg(long, value_name = "N")]
    pub tail: Option<usize>,
    /// Copy PATH back from the remote copy after the run (repeatable).
    #[arg(long, value_name = "PATH")]
    pub fetch: Vec<String>,
    /// Forward the local environment variable NAME (repeatable).
    #[arg(long, value_name = "NAME")]
    pub env: Vec<String>,
    /// Sync PATH even if .gitignore excludes it (repeatable; never secrets).
    #[arg(long, value_name = "PATH")]
    pub include: Vec<String>,
    /// Memory cap for this run, overriding the host's memory_max (for example 2G).
    #[arg(long, value_name = "SIZE")]
    pub mem: Option<String>,
    /// Print the final status as one JSON object instead of a text line.
    #[arg(long)]
    pub json: bool,
    /// Skip syncing and rerun on the existing remote copy.
    #[arg(long)]
    pub no_sync: bool,
    /// Run a task from .hopbox.toml; words after `--` are appended.
    #[arg(long, value_name = "NAME")]
    pub task: Option<String>,
    /// The command to run, after `--`.
    #[arg(last = true, value_name = "CMD")]
    pub cmd: Vec<String>,
}

/// Parse arguments. Usage errors map to exit 125 in `main`.
pub fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    Cli::try_parse_from(args)
}

/// The command words joined with spaces, like ssh does: the remote shell parses them.
pub fn command_string(words: &[String]) -> Option<String> {
    let joined = words.join(" ");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_parses_flags_and_trailing_command() {
        let cli = parse([
            "hopbox",
            "run",
            "--host",
            "h",
            "--mem",
            "2G",
            "--include",
            "a",
            "--include",
            "b",
            "--json",
            "--",
            "cargo",
            "test",
            "--",
            "--nocapture",
        ])
        .unwrap();
        let Command::Run(r) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(r.host.as_deref(), Some("h"));
        assert_eq!(r.mem.as_deref(), Some("2G"));
        assert_eq!(r.include, ["a", "b"]);
        assert!(r.json);
        assert_eq!(r.cmd, ["cargo", "test", "--", "--nocapture"]);
    }

    #[test]
    fn cli_usage_error_is_an_error() {
        assert!(parse(["hopbox", "run", "--bogus"]).is_err());
        assert!(
            parse(["hopbox", "run", "cargo"]).is_err(),
            "command must follow --"
        );
    }

    #[test]
    fn cmd_words_join() {
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            command_string(&w(&["cargo", "nextest", "run"])).as_deref(),
            Some("cargo nextest run")
        );
        assert_eq!(
            command_string(&w(&["echo hi && false"])).as_deref(),
            Some("echo hi && false")
        );
        assert_eq!(command_string(&[]), None);
        assert_eq!(command_string(&w(&["  "])), None);
    }
}
