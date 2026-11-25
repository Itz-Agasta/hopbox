# Changelog

## 1.0.1

### Fixes

- A run that uses a terminal no longer ends with ssh's "Shared connection closed" line after the status line.

### Docs

- The README and the agents page now show how to add a hopbox rule to the global instructions of Claude Code, Codex, and OpenCode, and a Claude Code `SessionStart` hook.

## 1.0.0

hopbox 1.0 runs your heavy builds and tests on another machine over SSH and gives you the result as if they ran locally. It's built for coding agents and for the developer typing the same commands.

### Run jobs on any SSH host

- `hopbox run -- <command>` copies your working tree to the host with `rsync`, runs the command in the remote copy of your current directory, streams the output, and exits with the command's exit code.
- The last line of every run is a status line (or `--json` object) with `source=remote|hopbox`, so scripts and agents can tell a failed test from a failed connection.
- Each job runs in its own systemd scope with `MemoryMax`, no swap, and `CPUWeight` from your config, so a runaway build can't take down other services on a shared box. A job the cap kills exits with `137` and `oom=true`.
- Secret-looking files (`.env*`, keys, cloud credentials) are never synced, and environment variables are forwarded only when named with `--env`.
- One lock per worktree: a second run waits instead of changing files under a running job. Different worktrees run in parallel.
- `--tail N` keeps the full log on disk and prints only the end. `--fetch` copies results back, also when the job fails. `--task` runs commands defined in `.hopbox.toml`.
- Ctrl-C stops the whole remote job, not only the local side.

### Background jobs and port forwarding

- `hopbox run --bg` starts a job that keeps running when you close the terminal or your laptop sleeps, and prints its id.
- `hopbox jobs`, `logs [-f]`, `wait`, and `kill` manage background jobs. `wait` exits with the job's code.
- `hopbox run --port 3000` forwards a local port to the remote while the run lasts.

### Rent a GPU box

- `hopbox up vast --gpu RTX_4090 --ttl 2h` rents a vast.ai instance and makes it usable as `--host <name>`. It shows the price and asks first; without a terminal it needs `--yes-spend`.
- Every rental destroys itself at its deadline using the instance's own restricted key, so no credentials leave your laptop. `hopbox ls` shows cost and time left and cleans up anything past its deadline. `hopbox down` destroys an instance after checking its label.

### Look after hosts

- `hopbox doctor` reports what a host has and lacks: tools, resource caps, memory, disk, and the `PATH` your jobs see. It never installs anything.
- `hopbox gc` removes remote copies and finished background jobs nobody has used for a while.

### Install

Download a binary from the release page, use the shell installer, or build from source with Rust 1.85 or newer. hopbox runs on Linux and macOS and needs only `ssh`, `rsync`, `bash`, and `flock` on the remote.
