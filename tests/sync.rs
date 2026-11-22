mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use hopbox::config::{GlobalConfig, HostConfig, RepoConfig, RunFlags, resolve};
use hopbox::sync::sync;
use hopbox::transport::Transport;
use hopbox::workspace::{LocalTree, remote_workspace};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// A git repo in a dir with a space, with ignored dirs and secrets.
fn repo(base: &Path) -> PathBuf {
    let root = base.join("my repo");
    write(&root, "src/a.rs", "fn main() {}\n");
    write(&root, ".gitignore", "target/\ndata/\n");
    write(&root, "target/x", "build output\n");
    write(&root, "data/keep.bin", "keep\n");
    write(&root, "data/skip.bin", "skip\n");
    write(&root, ".env", "TOKEN=secret\n");
    write(&root, "id_ed25519", "key\n");
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    root
}

fn run_sync(s: &common::Sshd, root: &Path, include: &[&str]) {
    let global = GlobalConfig {
        default_host: None,
        hosts: [(s.host.to_string(), HostConfig::default())].into(),
    };
    let flags = RunFlags {
        host: Some(s.host.into()),
        include: include.iter().map(|x| x.to_string()).collect(),
        ..RunFlags::default()
    };
    let r = resolve(&global, &RepoConfig::default(), &flags).unwrap();
    let t = Transport::with_config(s.host, Some(s.ssh_config.clone()), s.dir.path().join("cm"))
        .unwrap();
    let ws = remote_workspace(&r.workspace_root, "my_repo-test");
    t.preflight(&ws).unwrap();
    let tree = LocalTree::discover(root).unwrap();
    sync(&t, &tree, &ws, &r).unwrap();
}

fn remote_files(s: &common::Sshd) -> Vec<String> {
    let out = s.ssh("cd ~/.cache/hopbox/ws/my_repo-test && find . -mindepth 1 | sort");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn sync_respects_gitignore_and_secrets() {
    let Some(s) = common::sshd() else { return };
    let d = tempfile::tempdir().unwrap();
    let root = repo(d.path());
    run_sync(&s, &root, &[]);
    let files = remote_files(&s);
    assert!(files.contains(&"./src/a.rs".to_string()), "{files:?}");
    assert!(files.contains(&"./.gitignore".to_string()), "{files:?}");
    for absent in ["./target", "./data", "./.env", "./id_ed25519", "./.git"] {
        assert!(
            !files
                .iter()
                .any(|f| f == absent || f.starts_with(&format!("{absent}/"))),
            "{absent} synced: {files:?}"
        );
    }
}

#[test]
fn include_overrides_gitignore_not_secrets() {
    let Some(s) = common::sshd() else { return };
    let d = tempfile::tempdir().unwrap();
    let root = repo(d.path());
    run_sync(&s, &root, &["data/keep.bin", ".env"]);
    let files = remote_files(&s);
    assert!(files.contains(&"./data/keep.bin".to_string()), "{files:?}");
    assert!(!files.contains(&"./data/skip.bin".to_string()), "{files:?}");
    assert!(!files.contains(&"./.env".to_string()), "{files:?}");
}

#[test]
fn resync_deletes_removed_files() {
    let Some(s) = common::sshd() else { return };
    let d = tempfile::tempdir().unwrap();
    let root = repo(d.path());
    run_sync(&s, &root, &[]);
    std::fs::remove_file(root.join("src/a.rs")).unwrap();
    write(&root, "src/b.rs", "\n");
    run_sync(&s, &root, &[]);
    let files = remote_files(&s);
    assert!(!files.contains(&"./src/a.rs".to_string()), "{files:?}");
    assert!(files.contains(&"./src/b.rs".to_string()), "{files:?}");
}
