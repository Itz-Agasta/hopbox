# Commands

hopbox has three commands: `run` executes a job on a remote host, `doctor` checks a host, and `gc` removes old remote copies.

## `hopbox run`

```sh
hopbox run [options] -- <command...>
hopbox run [options] --task <name> [-- <extra args...>]
```

`run` copies the repo to the host, runs the command there, streams its output, and exits with its exit code. Put the command after `--`. hopbox joins the words with spaces and the remote shell parses them, so `hopbox run -- 'make && make test'` works.

The command runs in the remote copy of your current directory: run it from `crates/foo` and it runs in `crates/foo` on the remote.

| Option | What it does |
|---|---|
| `--host NAME` | Run on host `NAME` from the global config. Default: `default_host`. |
| `--task NAME` | Run `[tasks].NAME` from `.hopbox.toml`. Words after `--` are appended. |
| `--tail N` | Save the full output to a log file and print only its last `N` lines. The status line gains `log=<path>`. |
| `--fetch PATH` | Copy `PATH` (relative to the current directory) back from the remote after the job, also when it fails. Repeatable. |
| `--env NAME` | Forward the local environment variable `NAME`. Repeatable. Nothing is forwarded otherwise. |
| `--include PATH` | Sync `PATH` (relative to the repo root) even though `.gitignore` excludes it. Repeatable. Secret files are never synced. |
| `--mem SIZE` | Memory cap for this run, such as `2G`. Overrides the host's `memory_max`. |
| `--no-sync` | Skip syncing and run on the copy already on the remote. |
| `--json` | Print the final status as one JSON object instead of a text line. |

### What gets synced

hopbox uses `rsync` over a shared SSH connection. It sends your working tree, including uncommitted changes, and skips:

- the `.git` folder
- everything git ignores, using git's own rules: nested `.gitignore` files, `!` negations, your global excludes, and ignores inside submodules
- secret-looking files, which are never sent even with `--include`: `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `.aws/`, `.ssh/`, `.npmrc`, `.netrc`, `*.tfstate*`, and `.hopbox-secrets`
- paths listed in `exclude` in `.hopbox.toml`

Files you delete locally are deleted from the remote copy. Ignored folders that exist only on the remote, such as a remote `target/` or `node_modules/`, are kept, so remote builds stay incremental.

Each worktree gets its own remote copy at `<workspace_root>/ws/<folder name>-<hash>`, so two worktrees can run jobs at the same time.

### Locking

Only one `run` at a time uses a given remote copy. A second run from the same worktree prints `hopbox: waiting for lock…` and waits up to 10 minutes, then exits with code 124. The lock covers the sync, the job, and `--fetch`, so a second run never changes files under a running job.

### Memory and CPU limits

When the host has a working `systemd-run --user`, each job runs in its own systemd scope with `MemoryMax` (and no swap) from `memory_max` or `--mem`, and `CPUWeight` from `cpu_weight`. A job that goes over its memory cap is killed, and the status line shows `exit=137` and `oom=true`.

Without `systemd-run --user`, hopbox prints `hopbox: warning: no resource caps on <host>` and runs the job without limits.

### Forwarded environment variables

`--env NAME` values travel to the remote on stdin and land in a file only your remote user can read. The file is read and deleted before your command starts, so values never appear in a process list. A name passed with `--env` that isn't set locally is an error (exit 125). A name listed in `env` in `.hopbox.toml` that isn't set locally is skipped with a warning.

### Stopping a job

Press Ctrl-C (or send SIGTERM) to stop. On a host with resource caps, hopbox stops the job's whole scope on the remote. On a host without them, hopbox stops the local side, and the remote command may keep running until it finishes. Either way hopbox exits with code 130.

### Logs

With `--tail`, logs go to `$XDG_STATE_HOME/hopbox/logs/`, which defaults to `~/.local/state/hopbox/logs/`.

## `hopbox doctor`

```sh
hopbox doctor [host]
```

`doctor` connects to the host (default: `default_host`) and reports what hopbox needs. It never installs or creates anything.

| Row | Meaning |
|---|---|
| `ssh` | hopbox could connect. |
| `rsync`, `bash`, `flock` | Required tools. A missing one is `fail` and makes `doctor` exit with code 1. |
| `caps` | Whether `systemd-run --user` works. `warn` means jobs run without limits. |
| `arch`, `memory`, `disk` | CPU architecture, available memory, and free disk space where remote copies live. |
| `path` | The `PATH` your configured `shell` sees. Use it to check that toolchains such as `cargo` are found. |
| `cost` | The host's `cost` label, when set. |

`doctor` exits with 0 when every required tool is present, 1 when one is missing, 125 for a config error, and 255 when the host is unreachable.

## `hopbox gc`

```sh
hopbox gc [host] [--older-than AGE] [--dry-run]
```

`gc` deletes remote copies that no run has used for longer than `AGE`. The default is `14d`. `AGE` is a number followed by `m`, `h`, `d`, or `w`, such as `12h` or `2w`.

| Option | What it does |
|---|---|
| `--older-than AGE` | Remove copies unused for longer than `AGE`. Default: `14d`. |
| `--dry-run` | Print what would be removed without deleting anything. |

`gc` skips a copy whose lock is held by a running job and prints `skipped <name>: in use`. Remote copies are disposable: the next `run` from that worktree recreates its copy with a full sync.
