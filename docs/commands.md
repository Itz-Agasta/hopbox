# Commands

hopbox runs jobs on a remote host and manages what they leave behind:

- `run` executes a job, in the foreground or as a background job
- `jobs`, `logs`, `wait`, and `kill` manage background jobs
- `doctor` checks a host
- `gc` removes old remote copies and finished background jobs

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
| `--bg` | Start the job in the background, print its id, and return at once. See [Background jobs](#background-jobs). |
| `--port PORT` | Forward local `PORT` to the same port on the remote's `localhost` while the run lasts. Use `LOCAL:REMOTE` for different ports. Repeatable. |

### What gets synced

hopbox uses `rsync` over a shared SSH connection. It sends your working tree, including uncommitted changes, and skips:

- the `.git` folder
- everything git ignores, using git's own rules: nested `.gitignore` files, `!` negations, your global excludes, and ignores inside submodules
- secret-looking files, which are never sent even with `--include`: `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `.aws/`, `.ssh/`, `.npmrc`, `.netrc`, `*.tfstate*`, and `.hopbox-secrets`
- paths listed in `exclude` in `.hopbox.toml`

When you delete a file locally, hopbox deletes it from the remote copy. Ignored folders that exist only on the remote, such as a remote `target/` or `node_modules/`, are kept, so remote builds stay incremental.

Each worktree gets its own remote copy at `<workspace_root>/ws/<folder name>-<hash>`, so two worktrees can run jobs at the same time.

### Locking

Only one `run` at a time uses a given remote copy. A second run from the same worktree prints `hopbox: waiting for lock…` and waits up to 10 minutes, then exits with code 124. The lock covers the sync, the job, and `--fetch`, so a second run never changes files under a running job.

### Memory and CPU limits

When the host has a working `systemd-run --user`, each job runs in its own systemd scope with `MemoryMax` (and no swap) from `memory_max` or `--mem`, and `CPUWeight` from `cpu_weight`. When a job goes over its memory cap, the kernel stops it and the status line shows `exit=137` and `oom=true`.

Without `systemd-run --user`, hopbox prints `hopbox: warning: no resource caps on <host>` and runs the job without limits.

### Forwarded environment variables

hopbox sends `--env NAME` values over stdin to a file only your remote user can read. The remote side reads and deletes that file before your command starts, so values never appear in a process list.

A name passed with `--env` that isn't set locally is an error (exit 125). A name listed in `env` in `.hopbox.toml` that isn't set locally is skipped with a warning. Names starting with `__hopbox_` are reserved for hopbox and rejected with exit 125.

### Stopping a job

Press Ctrl-C (or send SIGTERM) to stop. On a host with resource caps, hopbox stops the job's whole scope on the remote. On a host without them, hopbox stops the local side, and the remote command may keep running until it finishes. Either way hopbox exits with code 130.

### Background jobs

`--bg` starts the job and returns as soon as it's running. hopbox prints the job id on stdout and adds `job=<id>` to the status line:

```sh
hopbox run --bg -- cargo build --release
# 3f9c2a1b
# hopbox: exit=0 source=hopbox host=hermes dur=2.3s job=3f9c2a1b
```

The job keeps running when you close the terminal, lose the connection, or put your laptop to sleep. It holds the remote copy's lock until it ends, so a later `run` from the same worktree waits for it. Its output goes to a log on the remote: read it with `hopbox logs`, and get its exit code with `hopbox wait`.

`--bg` can't be combined with `--fetch`, `--tail`, or `--port`, and exits with code 125 if you try. Paths listed in `fetch` in `.hopbox.toml` aren't copied back for background jobs.

### Port forwarding

`--port 3000` makes `localhost:3000` on your laptop reach `localhost:3000` on the remote while the run lasts, so you can open a dev server the job starts:

```sh
hopbox run --port 3000 -- npm run dev
```

Each forward uses its own SSH connection and closes when the run ends. If the local port is already in use, including by another run's forward, hopbox exits with code 125 before starting the job.

### Processes a job leaves running

A daemon that a job starts, such as an `sccache` server, can keep running after the job ends. On a host with resource caps, it stays inside the job's systemd scope, under the same memory and CPU limits.

### Where `--tail` logs go

With `--tail`, logs go to `$XDG_STATE_HOME/hopbox/logs/`, which defaults to `~/.local/state/hopbox/logs/`.

## `hopbox jobs`

```sh
hopbox jobs [--host NAME] [--json]
```

`jobs` lists background jobs on a host (default: `default_host`):

```text
ID        STATE    EXIT   AGE  COMMAND
3f9c2a1b  done        0   12m  cargo build --release
8d0e4f67  running     -   40s  cargo nextest run
```

| State | Meaning |
|---|---|
| `running` | The job is still running. |
| `done` | The job finished; `EXIT` is its exit code. |
| `lost` | The job's process is gone without an exit code, for example after the remote rebooted. |

`--json` prints a JSON array of objects with `id`, `state`, `exit`, `start` (Unix seconds), `cmd`, and `host`.

## `hopbox logs`

```sh
hopbox logs <id> [-f] [--host NAME]
```

`logs` prints a background job's output. `-f` keeps printing new output until the job ends.

## `hopbox wait`

```sh
hopbox wait <id> [--host NAME] [--json]
```

`wait` blocks until a background job ends, then exits with the job's exit code and prints a status line with `source=remote` and `job=<id>`, like a foreground `run`. A job the memory cap killed exits with 137 and `oom=true`. A `lost` job exits with 125.

## `hopbox kill`

```sh
hopbox kill <id> [--host NAME]
```

`kill` stops a background job and every process it started. The job records exit code 143. hopbox checks that the recorded process is still the job before it sends a signal, so it never stops an unrelated process that reused the job's process id.

`logs`, `wait`, and `kill` find the host on their own: hopbox remembers which host each job id started on. Pass `--host` to override. An id that isn't 8 lowercase hex characters, or that doesn't exist on the host, exits with code 125.

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

`gc` skips a copy whose lock is held by a running job and prints `skipped <name>: in use`. It also skips a copy that a run used after `gc` listed it and prints `skipped <name>: used since listing`. If `gc` can't remove a copy, it prints the error and exits with code 1.

`gc` also removes finished background jobs whose exit is older than `AGE`, printing `removed job <id>`. Remote copies are disposable: the next `run` from that worktree recreates its copy with a full sync.
