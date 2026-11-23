//! Merge global config, repo config, and command-line flags into one run configuration.

use std::collections::BTreeMap;

use super::{ConfigError, GlobalConfig, MemSize, RepoConfig};

pub const DEFAULT_SHELL: &str = "bash -lc";
pub const DEFAULT_WORKSPACE_ROOT: &str = "~/.cache/hopbox";

/// `run` flags that feed into config resolution. The CLI layer fills this in.
#[derive(Debug, Default)]
pub struct RunFlags {
    pub host: Option<String>,
    pub mem: Option<String>,
    pub include: Vec<String>,
    pub fetch: Vec<String>,
    pub env: Vec<String>,
}

/// Everything a run needs from config, with defaults applied.
#[derive(Debug)]
pub struct Resolved {
    pub host_name: String,
    pub ssh_dest: String,
    /// `ssh -p`, for rented hosts.
    pub ssh_port: Option<u16>,
    /// Extra `ssh -o` options, for rented hosts.
    pub ssh_opts: Vec<std::ffi::OsString>,
    pub memory_max: Option<MemSize>,
    pub cpu_weight: Option<u16>,
    pub shell: Vec<String>,
    pub workspace_root: String,
    /// Label only; shown by doctor.
    pub cost: Option<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub fetch: Vec<String>,
    pub env: Vec<String>,
    pub tasks: BTreeMap<String, String>,
}

/// Pick the host (`--host`, then `default_host`) and merge config layers.
/// Flags add to repo config, and repo config adds to defaults.
pub fn resolve(
    global: &GlobalConfig,
    repo: &RepoConfig,
    flags: &RunFlags,
) -> Result<Resolved, ConfigError> {
    let host_name = flags
        .host
        .as_ref()
        .or(global.default_host.as_ref())
        .ok_or(ConfigError::NoHost)?;
    let host = global
        .hosts
        .get(host_name)
        .ok_or_else(|| ConfigError::UnknownHost {
            name: host_name.clone(),
            known: global.hosts.keys().cloned().collect(),
        })?;

    let memory_max = match &flags.mem {
        Some(m) => Some(MemSize::parse(m)?),
        None => host.memory_max.clone(),
    };
    let shell: Vec<String> = host
        .shell
        .as_deref()
        .unwrap_or(DEFAULT_SHELL)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if shell.is_empty() {
        return Err(ConfigError::EmptyShell);
    }
    let concat = |a: &[String], b: &[String]| [a, b].concat();

    Ok(Resolved {
        host_name: host_name.clone(),
        ssh_dest: host.ssh.clone().unwrap_or_else(|| host_name.clone()),
        ssh_port: host.ssh_port,
        ssh_opts: host.ssh_options.clone(),
        memory_max,
        cpu_weight: host.cpu_weight,
        shell,
        workspace_root: host
            .workspace_root
            .clone()
            .unwrap_or_else(|| DEFAULT_WORKSPACE_ROOT.to_owned()),
        cost: host.cost.clone(),
        exclude: repo.exclude.clone(),
        include: concat(&repo.include, &flags.include),
        fetch: concat(&repo.fetch, &flags.fetch),
        env: concat(&repo.env, &flags.env),
        tasks: repo.tasks.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::HostConfig;
    use super::*;

    fn hosts(names: &[&str]) -> GlobalConfig {
        GlobalConfig {
            default_host: None,
            hosts: names
                .iter()
                .map(|n| (n.to_string(), HostConfig::default()))
                .collect(),
        }
    }

    fn flags_host(h: &str) -> RunFlags {
        RunFlags {
            host: Some(h.into()),
            ..RunFlags::default()
        }
    }

    #[test]
    fn flag_host_beats_default_host() {
        let mut g = hosts(&["a", "b"]);
        g.default_host = Some("a".into());
        let r = resolve(&g, &RepoConfig::default(), &flags_host("b")).unwrap();
        assert_eq!(r.host_name, "b");
        let r = resolve(&g, &RepoConfig::default(), &RunFlags::default()).unwrap();
        assert_eq!(r.host_name, "a");
    }

    #[test]
    fn no_host_anywhere_errors() {
        let e = resolve(
            &GlobalConfig::default(),
            &RepoConfig::default(),
            &RunFlags::default(),
        )
        .unwrap_err();
        assert!(matches!(e, ConfigError::NoHost));
        let msg = e.to_string();
        assert!(
            msg.contains("--host") && msg.contains("default_host"),
            "{msg}"
        );
    }

    #[test]
    fn unknown_host_lists_known() {
        let e = resolve(
            &hosts(&["a", "b"]),
            &RepoConfig::default(),
            &flags_host("x"),
        )
        .unwrap_err();
        assert!(
            matches!(&e, ConfigError::UnknownHost { name, known } if name == "x" && known == &["a", "b"])
        );
        let msg = e.to_string();
        assert!(msg.contains("\"x\"") && msg.contains("a, b"), "{msg}");
    }

    #[test]
    fn default_host_not_in_hosts_is_unknown() {
        let mut g = hosts(&["a"]);
        g.default_host = Some("gone".into());
        let e = resolve(&g, &RepoConfig::default(), &RunFlags::default()).unwrap_err();
        assert!(matches!(e, ConfigError::UnknownHost { .. }));
    }

    #[test]
    fn defaults_fill_missing_host_fields() {
        let r = resolve(
            &hosts(&["hermes"]),
            &RepoConfig::default(),
            &flags_host("hermes"),
        )
        .unwrap();
        assert_eq!(r.ssh_dest, "hermes");
        assert_eq!(r.shell, vec!["bash", "-lc"]);
        assert_eq!(r.workspace_root, "~/.cache/hopbox");
        assert_eq!(r.memory_max, None);
        assert_eq!(r.cpu_weight, None);
    }

    #[test]
    fn host_fields_are_used() {
        let mut g = hosts(&[]);
        g.hosts.insert(
            "h".into(),
            HostConfig {
                ssh: Some("ubuntu@10.0.0.1".into()),
                memory_max: Some(MemSize::parse("6G").unwrap()),
                cpu_weight: Some(50),
                shell: Some("zsh -lc".into()),
                workspace_root: Some("/srv/hb".into()),
                cost: None,
                ssh_port: Some(2222),
                ssh_options: vec!["StrictHostKeyChecking=accept-new".into()],
            },
        );
        let r = resolve(&g, &RepoConfig::default(), &flags_host("h")).unwrap();
        assert_eq!(r.ssh_dest, "ubuntu@10.0.0.1");
        assert_eq!(r.memory_max.unwrap().as_str(), "6G");
        assert_eq!(r.cpu_weight, Some(50));
        assert_eq!(r.shell, vec!["zsh", "-lc"]);
        assert_eq!(r.workspace_root, "/srv/hb");
        assert_eq!(r.ssh_port, Some(2222));
        assert_eq!(r.ssh_opts, ["StrictHostKeyChecking=accept-new"]);
    }

    #[test]
    fn mem_flag_overrides_and_is_validated() {
        let mut g = hosts(&[]);
        g.hosts.insert(
            "h".into(),
            HostConfig {
                memory_max: Some(MemSize::parse("6G").unwrap()),
                ..HostConfig::default()
            },
        );
        let mut f = flags_host("h");
        f.mem = Some("2G".into());
        let r = resolve(&g, &RepoConfig::default(), &f).unwrap();
        assert_eq!(r.memory_max.unwrap().as_str(), "2G");

        f.mem = Some("lots".into());
        let e = resolve(&g, &RepoConfig::default(), &f).unwrap_err();
        assert!(matches!(e, ConfigError::InvalidMem(m) if m == "lots"));
    }

    #[test]
    fn lists_append_flags_after_repo() {
        let repo = RepoConfig {
            exclude: vec!["data/".into()],
            include: vec!["a".into()],
            fetch: vec!["f1".into()],
            env: vec!["E1".into()],
            tasks: BTreeMap::from([("test".into(), "cargo test".into())]),
        };
        let f = RunFlags {
            host: Some("h".into()),
            mem: None,
            include: vec!["b".into()],
            fetch: vec!["f2".into()],
            env: vec!["E2".into()],
        };
        let r = resolve(&hosts(&["h"]), &repo, &f).unwrap();
        assert_eq!(r.include, vec!["a", "b"]);
        assert_eq!(r.fetch, vec!["f1", "f2"]);
        assert_eq!(r.env, vec!["E1", "E2"]);
        assert_eq!(r.exclude, vec!["data/"]);
        assert_eq!(r.tasks["test"], "cargo test");
    }

    #[test]
    fn shell_is_split_on_whitespace() {
        let mut g = hosts(&[]);
        g.hosts.insert(
            "h".into(),
            HostConfig {
                shell: Some("zsh  -l -c".into()),
                ..HostConfig::default()
            },
        );
        let r = resolve(&g, &RepoConfig::default(), &flags_host("h")).unwrap();
        assert_eq!(r.shell, vec!["zsh", "-l", "-c"]);

        g.hosts.get_mut("h").unwrap().shell = Some("  ".into());
        let e = resolve(&g, &RepoConfig::default(), &flags_host("h")).unwrap_err();
        assert!(matches!(e, ConfigError::EmptyShell));
    }
}
