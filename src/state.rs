//! Rented instances hopbox created, kept in `$XDG_STATE_HOME/hopbox/instances.toml`.
//! Each one is usable as `--host <name>` like a configured host.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{GlobalConfig, HostConfig};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub name: String,
    pub provider: String,
    pub id: u64,
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    /// Unix seconds after which the instance must be gone.
    pub deadline: u64,
    /// Price in USD per hour when rented.
    pub dph: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    instance: Vec<Instance>,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("instances.toml")
}

/// Instances in the state file; a missing file is an empty list.
pub fn load(path: &Path) -> Result<Vec<Instance>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<File>(&text)
            .map(|f| f.instance)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write the state file atomically (temp file + rename).
pub fn save(path: &Path, instances: &[Instance]) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| err(&e))?;
    }
    let text = toml::to_string(&File {
        instance: instances.to_vec(),
    })
    .map_err(|e| err(&e))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| err(&e))?;
    std::fs::rename(&tmp, path).map_err(|e| err(&e))
}

/// Load the state file and add its instances to `global`; a broken file is a warning.
pub fn add_rented(global: &mut GlobalConfig, state_dir: &Path) {
    match load(&path(state_dir)) {
        Ok(instances) => merge_hosts(global, &instances, state_dir),
        Err(e) => eprintln!("hopbox: warning: ignoring rented hosts: {e}"),
    }
}

/// Add each rented instance as a host, unless `[hosts]` already has that name.
/// Rented hosts accept a new host key into hopbox's own `known_hosts`.
pub fn merge_hosts(global: &mut GlobalConfig, instances: &[Instance], state_dir: &Path) {
    for i in instances {
        global
            .hosts
            .entry(i.name.clone())
            .or_insert_with(|| HostConfig {
                ssh: Some(format!("{}@{}", i.ssh_user, i.ssh_host)),
                ssh_port: Some(i.ssh_port),
                ssh_options: vec![
                    "StrictHostKeyChecking=accept-new".into(),
                    crate::transport::ssh_path_option(
                        "UserKnownHostsFile",
                        &state_dir.join("known_hosts"),
                    ),
                ],
                cost: Some(format!("${:.2}/h", i.dph)),
                ..HostConfig::default()
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(name: &str) -> Instance {
        Instance {
            name: name.into(),
            provider: "vast".into(),
            id: 123,
            ssh_user: "root".into(),
            ssh_host: "1.2.3.4".into(),
            ssh_port: 40022,
            deadline: 1_700_000_000,
            dph: 0.45,
        }
    }

    #[test]
    fn state_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = path(&d.path().join("hopbox"));
        assert_eq!(load(&p).unwrap(), vec![]);
        save(&p, &[inst("a"), inst("b")]).unwrap();
        assert_eq!(load(&p).unwrap(), vec![inst("a"), inst("b")]);
        save(&p, &[]).unwrap();
        assert_eq!(load(&p).unwrap(), vec![]);
        std::fs::write(&p, "not toml [").unwrap();
        assert!(load(&p).is_err());
    }

    #[test]
    fn merge_keeps_configured_hosts() {
        let mut g = GlobalConfig::default();
        g.hosts.insert(
            "a".into(),
            HostConfig {
                ssh: Some("mine".into()),
                ..HostConfig::default()
            },
        );
        merge_hosts(&mut g, &[inst("a"), inst("gpu")], Path::new("/s"));
        assert_eq!(g.hosts["a"].ssh.as_deref(), Some("mine"));
        let gpu = &g.hosts["gpu"];
        assert_eq!(gpu.ssh.as_deref(), Some("root@1.2.3.4"));
        assert_eq!(gpu.ssh_port, Some(40022));
        assert_eq!(
            gpu.ssh_options,
            [
                "StrictHostKeyChecking=accept-new",
                "UserKnownHostsFile=\"/s/known_hosts\""
            ]
        );
        assert!(gpu.memory_max.is_none());
    }
}
