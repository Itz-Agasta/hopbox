# Getting started

hopbox runs a command from your repo on another machine and gives you its output and exit code. This page takes you from install to your first remote run.

## Requirements

You need:

- A remote machine you can reach with `ssh <host>` without a password prompt, for example over Tailscale or with an SSH key
- `ssh` and `rsync` on your laptop
- `rsync`, `bash`, and `flock` (from util-linux) on the remote
- Linux on the remote. Your laptop can run Linux or macOS.

hopbox installs nothing on the remote and opens no ports.

## Install

Build from source with Rust 1.85 or newer:

```sh
cargo install --git https://github.com/Itz-Agasta/hopbox
```

## Add a host

Create `~/.config/hopbox/config.toml`:

```toml
default_host = "hermes"

[hosts.hermes]
ssh = "hermes"
memory_max = "6G"
```

`ssh` is the destination hopbox passes to `ssh`, so anything in `~/.ssh/config` (user, port, jump hosts, keys) applies. `memory_max` caps each job's memory. See [Configuration](config.md) for every key.

## Check the host

```sh
hopbox doctor
```

```text
hopbox doctor: hermes (ssh hermes)
ok    ssh       connected
ok    rsync     rsync  version 3.2.7  protocol version 31
ok    bash      /usr/bin/bash
ok    flock     flock from util-linux 2.39.3
ok    caps      systemd-run --user works (memory_max 6G)
info  arch      aarch64
info  memory    8.0 GiB available of 11.6 GiB
info  disk      40.2 GiB free in the workspace root
info  path      /home/ubuntu/.local/bin:/home/ubuntu/.cargo/bin:/usr/bin
```

Fix any `fail` row before you continue. A `warn` on `caps` means jobs on this host run without memory or CPU limits.

## Run a job

From anywhere inside your repo:

```sh
hopbox run -- cargo nextest run
```

```text
… test output, streamed live …
hopbox: exit=0 source=remote host=hermes dur=41.8s
```

hopbox copies your working tree to the remote, runs the command in the same subdirectory you ran it from, streams the output, and exits with the command's exit code. The last line on stderr is always the status line. See [Exit codes](exit-codes.md) for how to read it.

## Next steps

Read on:

- [Commands](commands.md): every command and flag
- [Configuration](config.md): host settings and the per-repo `.hopbox.toml`
- [Using hopbox from coding agents](agents.md)
