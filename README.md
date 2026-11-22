# hopbox

Run heavy builds and tests on another machine over SSH, from your laptop or your coding agent.

> **Status: pre-release.** Nothing below works yet. This README describes the v0.1 target. Track progress in the pull requests.

You keep working on your laptop. When a job is too heavy for it, `hopbox` copies the repo to a remote machine, runs the command there, streams the output back, and exits with the command's exit code:

```sh
hopbox run -- cargo nextest run
```

```text
...test output, streamed live...
hopbox: exit=0 source=remote host=hermes dur=41.8s
```

hopbox is built for coding agents such as Claude Code as much as for people. A bundled skill (`skills/hopbox/SKILL.md`) tells an agent when to offload a job and how to read the result.

## How it works

1. `rsync` copies your working tree to a per-worktree folder on the remote. Files in `.gitignore`, the `.git` folder, and anything that looks like a secret stay on your laptop. File timestamps are kept, so incremental builds stay incremental.
2. `ssh` runs your command under memory and CPU limits, so a runaway build can't take down other services on the machine.
3. Output streams back live. With `--tail N`, the full log is saved and only the last `N` lines are printed.
4. `--fetch PATH` copies results such as reports or binaries back to your laptop.

The remote needs `ssh`, `rsync`, and `bash`. hopbox installs nothing on it and opens no ports.

## Requirements

- A remote you can reach with `ssh <host>`, for example over Tailscale
- `ssh` and `rsync` on both machines
- Linux or macOS

## Why hopbox

See [VISION.md](VISION.md) for what hopbox is, what it refuses to become, and why.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
