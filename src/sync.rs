//! Copy the local worktree to its remote workspace with rsync.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
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
    /// Paths git reports as ignored (dirs end in `/`). When set, they are fed to rsync on stdin
    /// as exact excludes, which keeps git's full semantics (`!` negation, nested and global
    /// ignores). `None` outside a git repo: fall back to rsync's `.gitignore` filter.
    pub git_ignored: Option<&'a [Vec<u8>]>,
}

impl RsyncSpec<'_> {
    /// The rsync argv (without the program name). rsync applies the first matching rule, so
    /// secret excludes come first, then includes, then repo excludes, then git's ignores.
    pub fn args(&self) -> Result<Vec<OsString>, SyncError> {
        check_dest(self.ws, self.workspace_root)?;
        let includes = normalize_includes(self.includes)?;
        let mut a: Vec<OsString> = ["-a", "--delete", "--compress", "-s", "-e"]
            .iter()
            .map(OsString::from)
            .collect();
        a.push(self.rsh.into());
        // hopbox's own remote-only files must survive --delete (a running job holds the lock).
        a.push("--filter=P /.hopbox.lock".into());
        a.push("--filter=P /.hopbox-last-used".into());
        // `.git` without a slash also covers a worktree's `.git` file.
        a.push("--exclude=.git".into());
        for secret in SECRET_EXCLUDES {
            a.push(format!("--exclude={secret}").into());
        }
        for inc in &includes {
            for anc in ancestors(inc) {
                a.push(format!("--include=/{}/", literal(&anc)).into());
            }
            a.push(format!("--include=/{}", literal(inc)).into());
            a.push(format!("--include=/{}/***", literal(inc)).into());
        }
        for anc in self.ignored_ancestors {
            a.push(format!("--exclude=/{}/*", literal(anc)).into());
        }
        for exc in self.excludes {
            a.push(format!("--exclude={exc}").into());
        }
        match self.git_ignored {
            Some(_) => {
                a.push("--from0".into());
                a.push("--exclude-from=-".into());
                // Receiver-side only: protects ignored remote-only paths (a remote target/ or
                // node_modules/) from --delete without changing what is sent.
                a.push("--filter=dir-merge,-r .gitignore".into());
            }
            None => a.push("--filter=:- .gitignore".into()),
        }
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

/// Escape a path so rsync matches it literally. rsync honors backslash escapes only in
/// patterns that contain a wildcard, so plain paths are left as they are.
fn literal(path: &str) -> String {
    String::from_utf8(literal_bytes(path.as_bytes())).expect("escaping keeps UTF-8")
}

fn literal_bytes(path: &[u8]) -> Vec<u8> {
    let wild = |b: &u8| matches!(b, b'*' | b'?' | b'[');
    if !path.iter().any(wild) {
        return path.to_vec();
    }
    let mut out = Vec::with_capacity(path.len() + 4);
    for &b in path {
        if wild(&b) || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// NUL-separated anchored, literal exclude patterns for `--from0 --exclude-from=-`.
pub fn exclude_list(git_ignored: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in git_ignored {
        out.push(b'/');
        out.extend_from_slice(&literal_bytes(p));
        out.push(0);
    }
    out
}

/// Untracked paths git ignores, as raw bytes with ignored dirs collapsed (`dir/`), including
/// inside initialized submodules. `None` outside git.
pub fn git_ignored(root: &Path) -> Option<Vec<Vec<u8>>> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let split = |out: Vec<u8>| -> Vec<Vec<u8>> {
        out.split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let mut paths = split(git(&[
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ])?);
    // Submodules are gitlinks (mode 160000): "<mode> <sha> <stage>\t<path>".
    for entry in split(git(&["ls-files", "-z", "--stage"]).unwrap_or_default()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        if !entry.starts_with(b"160000 ") {
            continue;
        }
        let sub = &entry[tab + 1..];
        let sub_root = root.join(std::ffi::OsStr::from_bytes(sub));
        if !sub_root.join(".git").exists() {
            continue;
        }
        for p in git_ignored(&sub_root).unwrap_or_default() {
            let mut full = sub.to_vec();
            full.push(b'/');
            full.extend_from_slice(&p);
            paths.push(full);
        }
    }
    Some(paths)
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

/// Sync `tree` into `ws` on the transport's host. `on_spawn` gets rsync's pid, so a signal
/// handler can stop it.
pub fn sync(
    t: &Transport,
    tree: &LocalTree,
    ws: &RemotePath,
    r: &Resolved,
    on_spawn: &dyn Fn(u32),
) -> Result<(), SyncError> {
    let rsh = t.rsync_rsh();
    let ignored = ignored_ancestors(&tree.root, &r.include)?;
    let git_list = git_ignored(&tree.root);
    let spec = RsyncSpec {
        src_root: &tree.root,
        rsh: &rsh,
        dest: t.dest(),
        ws,
        workspace_root: &r.workspace_root,
        excludes: &r.exclude,
        includes: &r.include,
        ignored_ancestors: &ignored,
        git_ignored: git_list.as_deref(),
    };
    let mut child = Command::new("rsync")
        .args(spec.args()?)
        .stdin(if git_list.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()?;
    on_spawn(child.id());
    if let (Some(list), Some(mut stdin)) = (&git_list, child.stdin.take()) {
        use std::io::Write;
        // rsync reads the whole list before transferring; a write error surfaces as its exit code.
        let _ = stdin.write_all(&exclude_list(list));
    }
    let status = child.wait()?;
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
            git_ignored: None,
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
    fn literal_escapes_glob_chars_only_when_present() {
        assert_eq!(literal("src/a.rs"), "src/a.rs");
        assert_eq!(literal("a\\b"), "a\\b");
        assert_eq!(literal("data/[ab].txt"), "data/\\[ab].txt");
        assert_eq!(literal("x*y?\\z"), "x\\*y\\?\\\\z");
    }

    #[test]
    fn git_ignored_list_replaces_gitignore_filter() {
        let ws = RemotePath::parse("~/.cache/hopbox/ws/r-1");
        let ignored = vec![b"target/".to_vec()];
        let sp = RsyncSpec {
            git_ignored: Some(&ignored),
            ..spec(&ws, &[], &[], &[])
        };
        let a = args_of(&sp);
        assert!(
            a.contains(&"--exclude-from=-".to_string()) && a.contains(&"--from0".to_string()),
            "{a:?}"
        );
        assert!(!a.iter().any(|x| x == "--filter=:- .gitignore"), "{a:?}");
        assert!(
            a.contains(&"--filter=dir-merge,-r .gitignore".to_string()),
            "{a:?}"
        );
        assert_eq!(exclude_list(&ignored), b"/target/\0");
    }

    #[test]
    fn rsync_arg_forms() {
        assert_eq!(RemotePath::parse("~").rsync_arg(), ".");
        assert_eq!(RemotePath::parse("~/a b").rsync_arg(), "a b");
        assert_eq!(RemotePath::parse("/srv/x").rsync_arg(), "/srv/x");
    }
}
