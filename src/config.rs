//! Global (`~/.config/hopbox/config.toml`) and per-repo (`.hopbox.toml`) configuration.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GlobalConfig {
    pub default_host: Option<String>,
    pub hosts: BTreeMap<String, HostConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HostConfig {
    pub ssh: Option<String>,
    pub memory_max: Option<MemSize>,
    pub cpu_weight: Option<u16>,
    pub shell: Option<String>,
    pub workspace_root: Option<String>,
    pub cost: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RepoConfig {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub fetch: Vec<String>,
    pub env: Vec<String>,
    pub tasks: BTreeMap<String, String>,
}

/// A systemd `MemoryMax` value: bytes with an optional K/M/G/T suffix, or a percentage.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct MemSize(String);

impl TryFrom<String> for MemSize {
    type Error = ConfigError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        MemSize::parse(&s)
    }
}

impl MemSize {
    pub fn parse(s: &str) -> Result<MemSize, ConfigError> {
        let all_digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
        let valid = if let Some(pct) = s.strip_suffix('%') {
            all_digits(pct) && pct.len() <= 3 && pct.parse::<u16>().is_ok_and(|n| n <= 100)
        } else {
            let digits = s.strip_suffix(['K', 'M', 'G', 'T']).unwrap_or(s);
            all_digits(digits)
        };
        if valid {
            Ok(MemSize(s.to_owned()))
        } else {
            Err(ConfigError::InvalidMem(s.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid config {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("invalid memory size {0:?}")]
    InvalidMem(String),
    #[error("cpu_weight {0} out of range 1..=10000")]
    InvalidCpuWeight(u16),
    #[error("unknown host {name:?}")]
    UnknownHost { name: String, known: Vec<String> },
    #[error("no host selected")]
    NoHost,
    #[error("shell is empty")]
    EmptyShell,
}

/// `$XDG_CONFIG_HOME/hopbox/config.toml`, else `$HOME/.config/hopbox/config.toml`.
/// An empty `XDG_CONFIG_HOME` counts as unset, per the XDG spec.
pub fn global_config_path(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|x| !x.is_empty()) {
        Some(xdg) => PathBuf::from(xdg),
        None => PathBuf::from(home.filter(|h| !h.is_empty())?).join(".config"),
    };
    Some(base.join("hopbox").join("config.toml"))
}

/// Load the global config. A missing file is an empty config.
pub fn load_global(path: &Path) -> Result<GlobalConfig, ConfigError> {
    match read_optional(path)? {
        Some(text) => parse_global(&text, path),
        None => Ok(GlobalConfig::default()),
    }
}

/// Load `<repo_root>/.hopbox.toml`. A missing file is an empty config.
pub fn load_repo(repo_root: &Path) -> Result<RepoConfig, ConfigError> {
    let path = repo_root.join(".hopbox.toml");
    match read_optional(&path)? {
        Some(text) => parse_repo(&text, &path),
        None => Ok(RepoConfig::default()),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn parse_toml<T: serde::de::DeserializeOwned>(text: &str, path: &Path) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

fn parse_global(text: &str, path: &Path) -> Result<GlobalConfig, ConfigError> {
    let global: GlobalConfig = parse_toml(text, path)?;
    let bad_weight = global
        .hosts
        .values()
        .filter_map(|h| h.cpu_weight)
        .find(|w| !(1..=10000).contains(w));
    if let Some(w) = bad_weight {
        return Err(ConfigError::InvalidCpuWeight(w));
    }
    Ok(global)
}

fn parse_repo(text: &str, path: &Path) -> Result<RepoConfig, ConfigError> {
    parse_toml(text, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(text: &str) -> Result<GlobalConfig, ConfigError> {
        parse_global(text, Path::new("config.toml"))
    }

    #[test]
    fn parses_design_example() {
        let g = global(
            r#"
default_host = "hermes"
[hosts.hermes]
ssh = "hermes"
memory_max = "6G"
cpu_weight = 50
shell = "bash -lc"
workspace_root = "~/.cache/hopbox"
cost = "free"
"#,
        )
        .unwrap();
        assert_eq!(g.default_host.as_deref(), Some("hermes"));
        let h = &g.hosts["hermes"];
        assert_eq!(h.ssh.as_deref(), Some("hermes"));
        assert_eq!(h.memory_max.as_ref().unwrap().as_str(), "6G");
        assert_eq!(h.cpu_weight, Some(50));
        assert_eq!(h.shell.as_deref(), Some("bash -lc"));
        assert_eq!(h.workspace_root.as_deref(), Some("~/.cache/hopbox"));
        assert_eq!(h.cost.as_deref(), Some("free"));
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = global("[hosts.h]\nmemmory_max = \"1G\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(err.to_string().contains("memmory_max"), "{err}");
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let err = global("defualt_host = \"h\"\n").unwrap_err();
        assert!(err.to_string().contains("defualt_host"), "{err}");
    }

    #[test]
    fn mem_size_accepts_systemd_forms() {
        for ok in ["512M", "6G", "1T", "64K", "1048576", "50%", "100%"] {
            assert_eq!(MemSize::parse(ok).unwrap().as_str(), ok);
        }
        for bad in [
            "", "6GB", "-1G", "6 G", "G", "1.5G", "101%", "%", "$(rm)", "6g",
        ] {
            assert!(MemSize::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn bad_mem_in_config_is_rejected() {
        let err = global("[hosts.h]\nmemory_max = \"lots\"\n").unwrap_err();
        assert!(err.to_string().contains("lots"), "{err}");
    }

    #[test]
    fn cpu_weight_range() {
        for ok in [1, 10000] {
            global(&format!("[hosts.h]\ncpu_weight = {ok}\n")).unwrap();
        }
        for bad in [0, 10001] {
            let err = global(&format!("[hosts.h]\ncpu_weight = {bad}\n")).unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidCpuWeight(w) if w == bad),
                "{err}"
            );
        }
    }

    #[test]
    fn repo_config_parses_and_defaults() {
        let r = parse_repo(
            "exclude = [\"data/raw/\"]\n[tasks]\ntest = \"cargo nextest run\"\n",
            Path::new(".hopbox.toml"),
        )
        .unwrap();
        assert_eq!(r.exclude, vec!["data/raw/"]);
        assert!(r.include.is_empty() && r.fetch.is_empty() && r.env.is_empty());
        assert_eq!(r.tasks["test"], "cargo nextest run");
    }

    #[test]
    fn repo_unknown_key_is_rejected() {
        let err = parse_repo("excludes = []\n", Path::new(".hopbox.toml")).unwrap_err();
        assert!(err.to_string().contains("excludes"), "{err}");
    }

    #[test]
    fn missing_files_are_empty() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            load_global(&d.path().join("nope.toml"))
                .unwrap()
                .hosts
                .is_empty()
        );
        assert!(load_repo(d.path()).unwrap().tasks.is_empty());
    }

    #[test]
    fn load_reads_files_and_names_path_on_error() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".hopbox.toml"), "env = [\"TOKEN\"]\n").unwrap();
        assert_eq!(load_repo(d.path()).unwrap().env, vec!["TOKEN"]);

        let cfg = d.path().join("config.toml");
        std::fs::write(&cfg, "default_host = 3\n").unwrap();
        let err = load_global(&cfg).unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");
    }

    #[test]
    fn unreadable_path_is_read_error() {
        let d = tempfile::tempdir().unwrap();
        // A directory where a file is expected: exists, but cannot be read as a file.
        let err = load_global(d.path()).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    #[test]
    fn global_path_prefers_xdg() {
        let p = |x: Option<&str>, h: Option<&str>| {
            global_config_path(x.map(OsStr::new), h.map(OsStr::new))
        };
        assert_eq!(
            p(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/hopbox/config.toml"))
        );
        assert_eq!(
            p(None, Some("/h")),
            Some(PathBuf::from("/h/.config/hopbox/config.toml"))
        );
        assert_eq!(
            p(Some(""), Some("/h")),
            Some(PathBuf::from("/h/.config/hopbox/config.toml"))
        );
        assert_eq!(p(None, None), None);
    }
}
