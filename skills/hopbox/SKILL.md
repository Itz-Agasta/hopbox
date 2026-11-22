---
name: hopbox
description: Offload heavy commands to a remote machine with hopbox. Use when a job is a full test suite, release or first build, Playwright or e2e run, Docker or Kubernetes work, needs more than ~3 GB of RAM or a GPU, or the user says the laptop is struggling.
---

# hopbox

`hopbox run` copies the current repo to a remote host, runs one command there, streams the output back, and exits with the command's exit code. Your edits stay local; each run re-syncs only what changed.

## Choose where to run

**Offload**: full test suites, release builds, first builds of large projects, Playwright and e2e runs, `docker` and `kubectl` work, anything expected to need more than ~3 GB of RAM, and anything the user asks to run remotely.

**Keep local**: `cargo check`, a single focused test, linters, formatters, and git. The sync round trip costs more than these jobs.

**GPU**: run on a GPU host only when the task needs CUDA. Renting one costs money, so ask the user first (see "Rent a GPU").

## Run a job

```sh
hopbox run --tail 60 -- cargo nextest run
```

- Use `--tail N` for any command with long output. You see the last `N` lines; the final status line gives the path of the full log to grep.
- Use `--fetch PATH` to bring results back (reports, binaries, coverage).
- Use `--env NAME` to forward an environment variable. Nothing is forwarded otherwise.
- Use `--include PATH` for a git-ignored file the job needs. Secret-looking files are never sent.
- Use `--host NAME` to pick a host other than the default.

## Read the result

The last line is always the status line:

```text
hopbox: exit=1 source=remote host=hermes dur=42.1s log=/home/me/.cache/hopbox/logs/…
```

- `source=remote`: the command ran and `exit` is its own exit code. Treat it exactly like a local failure: read the output, fix, rerun.
- `source=hopbox`: hopbox could not run the command (unreachable host, missing `rsync`, bad config). Report the message to the user. Never rerun the command locally as a substitute; the user offloaded it because the laptop can't handle it.

## Check a host

Run `hopbox doctor [host]` when a host is new or a run fails with `source=hopbox`. It reports reachability, required tools, free RAM and disk, and CPU architecture. Report missing toolchains to the user; installing software on their remote is their decision.

## Rent a GPU

1. Tell the user the GPU, the price per hour, and the TTL you plan to use, and wait for an explicit yes in this conversation.
2. Run `hopbox up vast --gpu <GPU> --max-price <USD/h> --ttl <duration>`. Pass `--yes-spend` only after that yes.
3. Run jobs with `--host` set to the name `up` printed.
4. Run `hopbox down <name>` as soon as the work is done, then confirm with `hopbox ls` that nothing is still billing.
