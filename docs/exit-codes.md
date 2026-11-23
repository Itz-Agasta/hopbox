# Exit codes

`hopbox run` exits with your command's own exit code, so scripts and agents can treat a remote run like a local one. The status line on the last line of stderr tells you whether a code came from your command or from hopbox.

## Status line

```text
hopbox: exit=1 source=remote host=hermes dur=42.1s
```

| Field | Meaning |
|---|---|
| `exit` | The exit code hopbox returns. |
| `source` | `remote` when your command ran and `exit` is its code. `hopbox` when hopbox itself failed or stopped the run. |
| `host` | The host name from your config, or `-` when no host was selected. |
| `dur` | Wall time in seconds. |
| `log` | Path of the full log, with `--tail`. |
| `oom` | `true` when the memory cap killed the job. |
| `error` | What went wrong, when `source=hopbox`. |

With `--json`, hopbox prints one JSON object with the same fields instead. `dur` is a number of seconds, and `log` and `error` are `null` when absent:

```json
{"dur":42.1,"error":null,"exit":1,"host":"hermes","log":null,"oom":false,"source":"remote"}
```

Always check `source`. A remote command can exit with any code, including the ones hopbox uses below.

## Codes

| Code | Source | Meaning |
|---|---|---|
| your command's code | `remote` | The command ran. A command killed by a signal exits with 128 plus the signal number. |
| 125 | `hopbox` | Usage or config error: bad flag, unknown host, unknown task, invalid config. |
| 255 | `hopbox` | hopbox couldn't reach the host, or lost the connection while syncing. |
| 124 | `hopbox` | Another run held the remote copy's lock for longer than 10 minutes. |
| 137 | `remote` | The memory cap killed the job. The status line adds `oom=true`. |
| 130 | `hopbox` | You stopped the run with Ctrl-C or SIGTERM. |

When a sync fails for another reason, hopbox exits with rsync's exit code and `source=hopbox`.
