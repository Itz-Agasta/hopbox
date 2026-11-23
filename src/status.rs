//! The machine-readable final line of every run.

use std::path::PathBuf;
use std::time::Duration;

/// Who produced the exit code: the remote command, or hopbox itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Remote,
    Hopbox,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Remote => "remote",
            Source::Hopbox => "hopbox",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub exit: i32,
    pub source: Source,
    /// Host name from config, or `-` when no host was selected.
    pub host: String,
    pub dur: Duration,
    pub log: Option<PathBuf>,
    pub oom: bool,
    pub error: Option<String>,
    /// Background job id for `run --bg`.
    pub job: Option<String>,
}

impl Status {
    /// A hopbox-side failure.
    pub fn hopbox_error(exit: i32, host: &str, dur: Duration, error: impl Into<String>) -> Status {
        Status {
            exit,
            source: Source::Hopbox,
            host: host.to_owned(),
            dur,
            log: None,
            oom: false,
            error: Some(error.into()),
            job: None,
        }
    }

    /// `hopbox: exit=<n> source=<s> host=<h> dur=<secs>s [log=<path>] [oom=true] [error="<msg>"]`
    pub fn line(&self) -> String {
        let mut s = format!(
            "hopbox: exit={} source={} host={} dur={:.1}s",
            self.exit,
            self.source.as_str(),
            self.host,
            self.dur.as_secs_f64()
        );
        if let Some(log) = &self.log {
            s += &format!(" log={}", log.display());
        }
        if self.oom {
            s += " oom=true";
        }
        if let Some(job) = &self.job {
            s += &format!(" job={job}");
        }
        if let Some(err) = &self.error {
            let one_line = err.replace(['\n', '\r'], " ").replace('"', "\\\"");
            s += &format!(" error=\"{one_line}\"");
        }
        s
    }

    /// One JSON object with the same fields as [`Status::line`].
    pub fn json(&self) -> String {
        serde_json::json!({
            "exit": self.exit,
            "source": self.source.as_str(),
            "host": self.host,
            "dur": (self.dur.as_secs_f64() * 1000.0).round() / 1000.0,
            "log": self.log.as_ref().map(|p| p.display().to_string()),
            "oom": self.oom,
            "error": self.error,
            "job": self.job,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Status {
        Status {
            exit: 3,
            source: Source::Remote,
            host: "hermes".into(),
            dur: Duration::from_millis(12_345),
            log: None,
            oom: false,
            error: None,
            job: None,
        }
    }

    #[test]
    fn status_line_format() {
        assert_eq!(
            base().line(),
            "hopbox: exit=3 source=remote host=hermes dur=12.3s"
        );
        let full = Status {
            exit: 137,
            log: Some("/l/x.log".into()),
            oom: true,
            error: Some("say \"hi\"\nthen".into()),
            ..base()
        };
        assert_eq!(
            full.line(),
            r#"hopbox: exit=137 source=remote host=hermes dur=12.3s log=/l/x.log oom=true error="say \"hi\" then""#
        );
    }

    #[test]
    fn status_line_job() {
        let st = Status {
            exit: 0,
            source: Source::Hopbox,
            job: Some("0a1b2c3d".into()),
            ..base()
        };
        assert_eq!(
            st.line(),
            "hopbox: exit=0 source=hopbox host=hermes dur=12.3s job=0a1b2c3d"
        );
    }

    #[test]
    fn status_json_fields() {
        let v: serde_json::Value = serde_json::from_str(&base().json()).unwrap();
        assert_eq!(v["exit"], 3);
        assert_eq!(v["source"], "remote");
        assert_eq!(v["host"], "hermes");
        assert_eq!(v["dur"], 12.345);
        assert_eq!(v["oom"], false);
        assert!(v["log"].is_null() && v["error"].is_null() && v["job"].is_null());
        assert!(!base().json().contains('\n'));
    }
}
