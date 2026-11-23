---
name: hopbox
description: Offload heavy commands to a remote machine with hopbox. Use when a job is a full test suite, release or first build, Playwright or e2e run, Docker or Kubernetes work, needs more than ~3 GB of RAM, or the user says the laptop is struggling.
---

# hopbox

`hopbox run` copies the current repo to a remote host, runs one command there, streams the output back, and exits with the command's exit code. Your edits stay local; each run re-syncs only what changed.

## Before the first run

hopbox must be installed and have a host in `~/.config/hopbox/config.toml`. If `hopbox` is missing or `hopbox doctor` fails, tell the user what's missing instead of running the job locally.

## Choose where to run

**Offload**: full test suites, release builds, first builds of large projects, Playwright and e2e runs, `docker` and `kubectl` work, anything expected to need more than ~3 GB of RAM, and anything the user asks to run remotely.

**Keep local**: `cargo check`, a single focused test, linters, formatters, and git. The sync round trip costs more than these jobs.

## Run a job

```sh
hopbox run --tail 60 -- cargo nextest run
```

- Use `--tail N` for any command with long output. You see the last `N` lines; the final status line gives the path of the full log to grep.
- Use `--fetch PATH` to bring results back (reports, binaries, coverage).
- Use `--env NAME` to forward an environment variable. Nothing is forwarded otherwise.
- Use `--include PATH` for a git-ignored file the job needs. Secret-looking files are never sent.
- Use `--host NAME` to pick a host other than the default.
- Use `--task NAME` to run a task defined in the repo's `.hopbox.toml`.

## Run a long job in the background

For a job that takes several minutes, start it with `--bg` and keep working:

```sh
id=$(hopbox run --bg -- cargo build --release)
hopbox wait "$id"        # later: exits with the job's code, same status line
```

`hopbox logs "$id"` shows output so far; `hopbox jobs` lists jobs; `hopbox kill "$id"` stops one. `--bg` can't be combined with `--tail`, `--fetch`, or `--port`.

Use `--port 3000` on a foreground run to open a remote dev server at `localhost:3000`.

## Read the result

The last line is always the status line:

```text
hopbox: exit=1 source=remote host=hermes dur=42.1s log=/home/me/.local/state/hopbox/logs/hopbox-1a2b3c4d.log
```

- `source=remote`: the command ran and `exit` is its own exit code. Treat it exactly like a local failure: read the output, fix, rerun.
- `exit=137 oom=true`: the job hit the host's memory cap. Tell the user; a rerun with `--mem` above the cap needs their go-ahead.
- `source=hopbox`: hopbox could not run the command (unreachable host, missing `rsync`, bad config, or `exit=124` when another run held the workspace for 10 minutes). Report the `error` to the user. Never rerun the command locally as a substitute; the user offloaded it because the laptop can't handle it.
- Add `--json` if you would rather parse one JSON object with the same fields.

## Rent a GPU (costs money)

Only when the job needs CUDA and the user agrees. Run `hopbox up vast --gpu <GPU> --ttl <time> --max-price <usd>` without `--yes-spend` first: it prints the offer and price, then exits 125 because there is no terminal. Show that price to the user. Only after they approve that price in the conversation, rerun with `--max-price` set to the approved hourly price and `--yes-spend`: the rerun searches again, and the lower `--max-price` keeps it from renting anything more expensive than what they approved. Then `hopbox run --host <name> -- …`, and `hopbox down <name>` when done. `hopbox ls` shows cost so far.

## Check a host

Run `hopbox doctor [host]` when a host is new or a run fails with `source=hopbox`. It reports reachability, required tools, free RAM and disk, and CPU architecture. Report missing toolchains to the user; installing software on their remote is their decision.
