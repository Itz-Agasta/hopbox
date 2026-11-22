//! Copy the local worktree to its remote workspace with rsync.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Resolved;
use crate::remote_script::RemotePath;
use crate::transport::Transport;
use crate::workspace::LocalTree;

/// Never synced, whatever `.gitignore` or `--include` say.
pub const SECRET_EXCLUDES: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    ".aws/",
    ".ssh/",
    ".npmrc",
    ".netrc",
    "*.tfstate*",
    ".hopbox-secrets",
];

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("refusing to sync into {0}: not a hopbox workspace under <workspace_root>/ws/")]
    UnsafeDest(String),
    #[error("invalid --include path {0:?}: use a path relative to the repo root without '..'")]
    BadInclude(String),
    #[error("rsync failed with exit code {0}")]
    Rsync(i32),
    #[error("cannot run rsync: {0}")]
    Spawn(#[from] io::Error),
}

/// Everything that decides the rsync command line.
#[derive(Debug)]
pub struct RsyncSpec<'a> {
    pub src_root: &'a Path,
    pub rsh: &'a str,
    pub dest: &'a str,
    pub ws: &'a RemotePath,
    pub workspace_root: &'a str,
    pub excludes: &'a [String],
    pub includes: &'a [String],
    /// Ancestors of include paths that `.gitignore` ignores; their other contents stay excluded.
    pub ignored_ancestors: &'a [String],
}

impl RsyncSpec<'_> {
    /// The rsync argv (without the program name). rsync applies the first matching rule, so
    /// secret excludes come first, then includes, then repo excludes, then `.gitignore`.
    pub fn args(&self) -> Result<Vec<OsString>, SyncError> {
        check_dest(self.ws, self.workspace_root)?;
        let includes = normalize_includes(self.includes)?;
        let mut a: Vec<OsString> = ["-a", "--delete", "--compress", "-s", "-e"]
            .iter()
            .map(OsString::from)
            .collect();
        a.push(self.rsh.into());
        // `.git` without a slash also covers a worktree's `.git` file.
        a.push("--exclude=.git".into());
        for secret in SECRET_EXCLUDES {
            a.push(format!("--exclude={secret}").into());
        }
        for inc in &includes {
            for anc in ancestors(inc) {
                a.push(format!("--include=/{anc}/").into());
            }
            a.push(format!("--include=/{inc}").into());
            a.push(format!("--include=/{inc}/***").into());
        }
        for anc in self.ignored_ancestors {
            a.push(format!("--exclude=/{anc}/*").into());
        }
        for exc in self.excludes {
            a.push(format!("--exclude={exc}").into());
        }
        a.push("--filter=:- .gitignore".into());
        let mut src = self.src_root.as_os_str().to_owned();
        src.push("/");
        a.push(src);
        a.push(format!("{}:{}/", self.dest, self.ws.rsync_arg()).into());
        Ok(a)
    }
}

/// `--delete` may only ever target `<workspace_root>/ws/<name>`.
fn check_dest(ws: &RemotePath, workspace_root: &str) -> Result<(), SyncError> {
    let base = RemotePath::parse(workspace_root).join("ws");
    let unsafe_dest = || SyncError::UnsafeDest(ws.render());
    let same_kind = matches!(
        (ws, &base),
        (RemotePath::Home(_), RemotePath::Home(_)) | (RemotePath::Abs(_), RemotePath::Abs(_))
    );
    if !same_kind {
        return Err(unsafe_dest());
    }
    let name = ws
        .raw()
        .strip_prefix(base.raw())
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(unsafe_dest)?;
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(unsafe_dest());
    }
    Ok(())
}

/// Strip `./` and trailing `/`; reject absolute paths, `..`, and empty paths.
fn normalize_includes(includes: &[String]) -> Result<Vec<String>, SyncError> {
    includes
        .iter()
        .map(|raw| {
            let parts: Vec<&str> = raw
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect();
            if raw.starts_with('/') || parts.is_empty() || parts.contains(&"..") {
                return Err(SyncError::BadInclude(raw.clone()));
            }
            Ok(parts.join("/"))
        })
        .collect()
}

/// `a/b/c` → `["a", "a/b"]`.
fn ancestors(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len()).map(|n| parts[..n].join("/")).collect()
}

/// Ancestors of `includes` that git reports as ignored, as `/`-joined relative paths.
pub fn ignored_ancestors(root: &Path, includes: &[String]) -> Result<Vec<String>, SyncError> {
    let mut found: Vec<String> = Vec::new();
    for inc in normalize_includes(includes)? {
        for anc in ancestors(&inc) {
            if found.contains(&anc) {
                continue;
            }
            let ignored = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["check-ignore", "-q", "--"])
                .arg(format!("{anc}/"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?
                .success();
            if ignored {
                found.push(anc);
            }
        }
    }
    Ok(found)
}

/// Sync `tree` into `ws` on the transport's host.
pub fn sync(
    t: &Transport,
    tree: &LocalTree,
    ws: &RemotePath,
    r: &Resolved,
) -> Result<(), SyncError> {
    let rsh = t.rsync_rsh();
    let ignored = ignored_ancestors(&tree.root, &r.include)?;
    let spec = RsyncSpec {
        src_root: &tree.root,
        rsh: &rsh,
        dest: t.dest(),
        ws,
        workspace_root: &r.workspace_root,
        excludes: &r.exclude,
        includes: &r.include,
        ignored_ancestors: &ignored,
    };
    let status = Command::new("rsync")
        .args(spec.args()?)
        .stdin(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(SyncError::Rsync(status.code().unwrap_or(-1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn args_of(spec: &RsyncSpec) -> Vec<String> {
        spec.args()
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    fn spec<'a>(
        ws: &'a RemotePath,
        inc: &'a [String],
        anc: &'a [String],
        exc: &'a [String],
    ) -> RsyncSpec<'a> {
        RsyncSpec {
            src_root: Path::new("/src/my repo"),
            rsh: "ssh -o BatchMode=yes",
            dest: "hermes",
            ws,
            workspace_root: "~/.cache/hopbox",
            excludes: exc,
            includes: inc,
            ignored_ancestors: anc,
        }
    }

    #[test]
    fn rsync_args_order() {
        let ws = RemotePath::parse("~/.cache/hopbox/ws/r-1");
        let inc = s(&["data/keep.bin"]);
        let anc = s(&["data"]);
        let exc = s(&["big/"]);
        let a = args_of(&spec(&ws, &inc, &anc, &exc));
        assert_eq!(
            &a[..6],
            [
                "-a",
                "--delete",
                "--compress",
                "-s",
                "-e",
                "ssh -o BatchMode=yes"
            ]
        );
        let pos = |x: &str| {
            a.iter()
                .position(|y| y == x)
                .unwrap_or_else(|| panic!("missing {x} in {a:?}"))
        };
        assert!(pos("--exclude=.git") < pos("--exclude=.env"));
        assert!(pos("--exclude=.hopbox-secrets") < pos("--include=/data/"));
        assert!(pos("--include=/data/") < pos("--include=/data/keep.bin"));
        assert!(pos("--include=/data/keep.bin") < pos("--include=/data/keep.bin/***"));
        assert!(pos("--include=/data/keep.bin/***") < pos("--exclude=/data/*"));
        assert!(pos("--exclude=/data/*") < pos("--exclude=big/"));
        assert!(pos("--exclude=big/") < pos("--filter=:- .gitignore"));
        assert_eq!(
            &a[a.len() - 2..],
            ["/src/my repo/", "hermes:.cache/hopbox/ws/r-1/"]
        );
    }

    #[test]
    fn delete_guard_rejects_outside_ws() {
        for bad in [
            "~",
            "~/.cache/hopbox",
            "~/.cache/hopbox/ws",
            "~/.cache/hopbox/other",
            "~/.cache/hopbox/ws/a/b",
            "/tmp/x",
        ] {
            let ws = RemotePath::parse(bad);
            let e = spec(&ws, &[], &[], &[]).args().unwrap_err();
            assert!(matches!(e, SyncError::UnsafeDest(_)), "{bad}: {e:?}");
        }
        for bad in ["..", "."] {
            let ws = RemotePath::parse("~/.cache/hopbox").join("ws").join(bad);
            assert!(spec(&ws, &[], &[], &[]).args().is_err(), "{bad}");
        }
    }

    #[test]
    fn delete_guard_accepts_absolute_root() {
        let ws = RemotePath::parse("/srv/hb/ws/r-1");
        let sp = RsyncSpec {
            workspace_root: "/srv/hb/",
            ..spec(&ws, &[], &[], &[])
        };
        let a = args_of(&sp);
        assert_eq!(a.last().unwrap(), "hermes:/srv/hb/ws/r-1/");
    }

    #[test]
    fn bad_includes_are_rejected() {
        let ws = RemotePath::parse("~/.cache/hopbox/ws/r-1");
        for bad in ["/etc/passwd", "../x", "a/../../x", ""] {
            let inc = s(&[bad]);
            let e = spec(&ws, &inc, &[], &[]).args().unwrap_err();
            assert!(matches!(e, SyncError::BadInclude(_)), "{bad}: {e:?}");
        }
        let inc = s(&["./gen/"]);
        let a = args_of(&spec(&ws, &inc, &[], &[]));
        assert!(a.contains(&"--include=/gen".to_string()), "{a:?}");
    }

    #[test]
    fn rsync_arg_forms() {
        assert_eq!(RemotePath::parse("~").rsync_arg(), ".");
        assert_eq!(RemotePath::parse("~/a b").rsync_arg(), "a b");
        assert_eq!(RemotePath::parse("/srv/x").rsync_arg(), "/srv/x");
    }
}
