# Using hopbox from coding agents

hopbox lets a coding agent such as Claude Code offload heavy jobs and read the result without guessing. The repo ships a skill at `skills/hopbox/SKILL.md` that teaches an agent when and how to use it.

## Install the skill

Install it with the `skills` CLI, which supports Claude Code, Codex, Cursor, and other agents. `-g` makes it available in every project:

```sh
npx skills add Itz-Agasta/hopbox --skill hopbox -g
```

In Claude Code, you can install it as a plugin instead:

```sh
claude plugin marketplace add Itz-Agasta/hopbox
claude plugin install hopbox@hopbox
```

To install it for specific agents, pass `-a`:

```sh
npx skills add Itz-Agasta/hopbox --skill hopbox -g -a codex -a opencode
```

To install it by hand, copy `skills/hopbox` into your agent's skills directory, such as `~/.claude/skills/`.

## Make the agent offload on its own

A skill loads when the agent decides it applies. To nudge that decision, add one line to your agent's global instructions file, so it applies in every repo:

```md
For full test suites, release builds and e2e runs, use hopbox (default host hermes) via the `hopbox` skill.
```

| Agent | Global file | Notes |
|---|---|---|
| Claude Code | `~/.claude/CLAUDE.md` | |
| Codex | `~/.codex/AGENTS.md` | Codex reads `~/.codex/AGENTS.override.md` instead when it exists. |
| OpenCode | `~/.config/opencode/AGENTS.md` | Without this file, OpenCode falls back to `~/.claude/CLAUDE.md`. |

Replace `hermes` with the name of your default host. Keep the line short: the skill holds the details.

### Claude Code hook

If you want a reminder injected at the start of every session instead of a standing rule, add a `SessionStart` hook to `~/.claude/settings.json`. The hook's stdout becomes context for the session:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo 'For full test suites, release builds and e2e runs, use hopbox via the hopbox skill.'"
          }
        ]
      }
    ]
  }
}
```

Merge the `hooks` key into your existing file instead of replacing it.

## What the agent runs

```sh
hopbox run --tail 60 -- cargo nextest run
```

`--tail 60` keeps the agent's context small: it sees the last 60 lines, and the status line gives the path of the full log, which it can search for the first failure.

## Long jobs in the background

For a job that takes minutes, the agent can start it in the background and keep working:

```sh
id=$(hopbox run --bg -- cargo build --release)
# … other work …
hopbox wait "$id"
```

`hopbox wait` exits with the job's code and prints the same status line as a foreground run. `hopbox logs "$id"` shows the output so far.

## How the agent reads the result

The last line of stderr is the status line:

```text
hopbox: exit=1 source=remote host=hermes dur=42.1s log=/home/me/.local/state/hopbox/logs/hopbox-1a2b3c4d.log
```

Check `source` first:

- `source=remote`: the command ran and `exit` is its code. A failure here is a real test or build failure: read the output, fix the code, and rerun.
- `source=hopbox`: hopbox couldn't run the command. The agent reports the `error` to you instead of running the job locally, because you offloaded it for a reason.

Pass `--json` to get the same fields as one JSON object. See [Exit codes](exit-codes.md) for every code.

## Keeping you in control

hopbox limits what an agent can do on your behalf:

- hopbox forwards no environment variables unless the agent names them with `--env`, and secret files never leave your laptop.
- Every job runs under the host's memory and CPU caps, so a runaway build can't take down other services on the remote.
- hopbox never falls back to running a job locally when the remote is unreachable.
- `hopbox up` never rents without a human yes. The agent shows you the price `up` prints and waits for your approval. It then reruns `up` with `--yes-spend` and `--max-price` set to the approved hourly price, so the rerun can't pick a more expensive offer.
