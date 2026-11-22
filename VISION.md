# Vision

## Identity

hopbox lets the machine you work on borrow the machine you don't. You stay on your laptop, with your editor, your notes, and your coding agent. When a job is too heavy for the laptop (a full test suite, a release build, a Playwright run, a CUDA job), `hopbox run` copies the repo to a remote machine, runs the command there, streams the output back, and exits with the command's exit code.

The primary user is a coding agent acting for a developer. The second is the developer typing the same commands by hand. Every design choice is judged by whether an agent can use it correctly on the first try and whether the human stays in control of anything that costs money.

## Principles

1. **The laptop is home.** The source of truth is the local working tree. Remote copies are disposable caches that can be deleted at any time without losing work.
2. **Nothing to install on the remote.** A remote needs `ssh`, `rsync`, and `bash`. No daemon, no open port, no agent. If `ssh host` works, hopbox works.
3. **Borrow, don't take over.** A remote is often shared with other services. Every job runs under memory and CPU caps, so a runaway build is killed before it harms anything else on the machine.
4. **Money needs a human yes.** Anything that rents paid compute shows the price and waits for confirmation. Every rental has a deadline the instance enforces on itself, so a forgotten box cannot keep billing.
5. **Fail loudly.** When a remote is unreachable, hopbox stops and says why. It never quietly runs the job somewhere else.
6. **Output is a contract.** The remote exit code becomes hopbox's exit code. The last line of every run is machine-readable. Long logs are kept on disk and summarized, so an agent spends its context on failures, not on passing tests.
7. **Use what the system already has.** hopbox calls the system `ssh` and `rsync` so `~/.ssh/config`, jump hosts, keys, and Tailscale work unchanged.
8. **Secrets stay local.** Files that look like secrets are never copied, and environment variables are forwarded only when named.

## Non-goals

- **A remote development environment.** hopbox runs jobs that start, finish, and report. Hosting long-lived agent sessions on a remote is the job of a terminal multiplexer such as Herdr or tmux.
- **A cloud provider.** hopbox rents from providers through their own CLIs and never bills, proxies, or stores credentials for them.
- **A build system or cache.** hopbox moves files and runs commands. Incremental builds come from the project's own tools.
- **Function-style platforms.** Services such as Modal or Kaggle run functions and notebooks, not a copy of your repo. Agents use those CLIs directly.
- **Windows hosts** in v1.

## Accept or resist

| Proposal | Verdict | Why |
|---|---|---|
| Add another rental provider with SSH instances | Accept | Fits the rent, sync, run, destroy model behind the provider interface. |
| Forward a dev server port from the remote | Accept (v0.2) | Still runs your repo's command; the port is part of seeing its result. |
| Fall back to running locally when the remote is down | Resist by default | Breaks principle 5. Acceptable only as an explicit opt-in flag. |
| Install toolchains on the remote automatically | Resist | `hopbox doctor` reports what's missing; installing software on someone's machine is their call. |
| A server process on the remote for faster syncs | Resist | Breaks principle 2. Make rsync faster first. |
| Rent without a TTL "just this once" | Resist | Breaks principle 4. The TTL is required. |
| Destroy instances hopbox didn't create | Resist | hopbox only touches instances carrying its own label. |
| A web dashboard | Resist | The CLI and its machine-readable output are the interface. |
