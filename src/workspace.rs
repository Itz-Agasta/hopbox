//! The local worktree and the name of its remote copy.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::remote_script::RemotePath;

/// Where the job runs from: the worktree root and the current subdirectory inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTree {
    pub root: PathBuf,
    /// `/`-separated path of the cwd relative to `root`; empty at the root.
    pub subdir: String,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("cannot read the current directory {}: {source}", path.display())]
    Cwd {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8(PathBuf),
}

impl LocalTree {
    /// The git worktree containing `cwd`, or `cwd` itself outside a git repo.
    pub fn discover(cwd: &Path) -> Result<LocalTree, WorkspaceError> {
        let cwd = cwd.canonicalize().map_err(|source| WorkspaceError::Cwd {
            path: cwd.to_owned(),
            source,
        })?;
        let git_root = Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args(["rev-parse", "--show-toplevel"])
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| PathBuf::from(s.trim_end_matches('\n')).canonicalize().ok());
        let root = git_root.unwrap_or_else(|| cwd.clone());
        let rel = cwd.strip_prefix(&root).unwrap_or(Path::new(""));
        let mut parts = Vec::new();
        for c in rel.components() {
            let part = c
                .as_os_str()
                .to_str()
                .ok_or_else(|| WorkspaceError::NonUtf8(cwd.clone()))?;
            parts.push(part);
        }
        Ok(LocalTree {
            subdir: parts.join("/"),
            root,
        })
    }
}

/// `<sanitized dirname>-<hash8>`, where hash8 is the first 8 hex of sha256(hostname + root).
/// Different machines and different worktrees get different remote copies.
pub fn workspace_name(root: &Path, hostname: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    let stem: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = match stem.as_str() {
        "" | "." | ".." => "repo".to_owned(),
        _ => stem,
    };
    let mut h = Sha256::new();
    h.update(hostname.as_bytes());
    h.update(root.as_os_str().as_bytes());
    let digest = h.finalize();
    let hash8: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("{stem}-{hash8}")
}

/// The laptop's hostname.
pub fn local_hostname() -> String {
    let from_proc = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    let from_cmd = || {
        Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
    };
    from_proc
        .or_else(from_cmd)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// `<workspace_root>/ws/<name>` on the remote.
pub fn remote_workspace(workspace_root: &str, name: &str) -> RemotePath {
    RemotePath::parse(workspace_root).join("ws").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn discover_finds_git_root_and_subdir() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("my repo");
        std::fs::create_dir_all(root.join("crates/a b")).unwrap();
        git(&root, &["init", "-q"]);
        let t = LocalTree::discover(&root.join("crates/a b")).unwrap();
        assert_eq!(t.root, root.canonicalize().unwrap());
        assert_eq!(t.subdir, "crates/a b");
        assert_eq!(LocalTree::discover(&root).unwrap().subdir, "");
    }

    #[test]
    fn discover_outside_git_uses_cwd() {
        let d = tempfile::tempdir().unwrap();
        let t = LocalTree::discover(d.path()).unwrap();
        assert_eq!(t.root, d.path().canonicalize().unwrap());
        assert_eq!(t.subdir, "");
    }

    #[test]
    fn workspace_name_is_stable_and_sanitized() {
        let a = workspace_name(Path::new("/home/u/my repo"), "laptop");
        assert_eq!(a, workspace_name(Path::new("/home/u/my repo"), "laptop"));
        let (stem, hash) = a.rsplit_once('-').unwrap();
        assert_eq!(stem, "my_repo");
        assert_eq!(hash.len(), 8);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, workspace_name(Path::new("/home/u/my repo"), "other"));
        assert_ne!(a, workspace_name(Path::new("/home/v/my repo"), "laptop"));
        assert!(workspace_name(Path::new("/"), "h").starts_with("repo-"));
        assert!(workspace_name(Path::new("/x/martin.rs"), "h").starts_with("martin.rs-"));
    }

    #[test]
    fn workspace_hash_is_sha256_of_hostname_then_root() {
        // Computed independently with `printf h/r | sha256sum`.
        let name = workspace_name(Path::new("/r"), "h");
        let expected = {
            let out = Command::new("sh")
                .args(["-c", "printf %s h/r | sha256sum"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()[..8].to_owned()
        };
        assert_eq!(name, format!("r-{expected}"));
    }

    #[test]
    fn local_hostname_is_not_empty() {
        assert!(!local_hostname().is_empty());
    }

    #[test]
    fn remote_workspace_joins_ws() {
        assert_eq!(
            remote_workspace("~/.cache/hopbox", "r-1234abcd").render(),
            r#""$HOME"/.cache/hopbox/ws/r-1234abcd"#
        );
    }
}
