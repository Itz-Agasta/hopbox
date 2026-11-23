# Configuration

hopbox reads two files: a global config with your hosts, and an optional `.hopbox.toml` in each repo. Unknown keys in either file are an error, so a typo fails loudly instead of being ignored.

## Global config

hopbox reads `$XDG_CONFIG_HOME/hopbox/config.toml`, or `~/.config/hopbox/config.toml` when `XDG_CONFIG_HOME` is unset.

```toml
default_host = "hermes"

[hosts.hermes]
ssh = "hermes"
memory_max = "6G"
cpu_weight = 50
shell = "bash -lc"
workspace_root = "~/.cache/hopbox"
cost = "free"
```

| Key | Default | Meaning |
|---|---|---|
| `default_host` | none | Host used when you don't pass `--host` or a host argument. |

Each `[hosts.NAME]` table defines a host:

| Key | Default | Meaning |
|---|---|---|
| `ssh` | the table name | Destination passed to `ssh`. Settings in `~/.ssh/config` apply. |
| `memory_max` | no cap | Memory cap per job, in systemd `MemoryMax` form: a byte count with an optional `K`, `M`, `G`, or `T` suffix (`512M`, `6G`), or a percentage of RAM (`50%`). |
| `cpu_weight` | systemd default | CPU share per job relative to other processes, from 1 to 10000. |
| `shell` | `bash -lc` | Command that runs your job on the remote. The default login shell reads `~/.profile`, so toolchains added there are on `PATH`. |
| `workspace_root` | `~/.cache/hopbox` | Where remote copies live. `~` means the remote user's home. |
| `cost` | none | A label shown by `hopbox doctor`. |

If a tool such as `cargo` isn't found on the remote, run `hopbox doctor` and check the `path` row. Add the tool's `PATH` setup to `~/.profile` on the remote, or set `shell` to a shell that loads it.

## Per-repo config

Put `.hopbox.toml` at the root of the repo and commit it:

```toml
exclude = ["data/raw/"]
include = ["fixtures/large.bin"]
fetch = ["target/nextest/junit.xml"]
env = ["DATABASE_URL"]

[tasks]
test = "cargo nextest run"
e2e = "npx playwright test"
```

| Key | Meaning |
|---|---|
| `exclude` | Extra paths to keep off the remote, in rsync pattern form. |
| `include` | Git-ignored paths to sync anyway, relative to the repo root. Secret files stay local. |
| `fetch` | Paths to copy back after every run, relative to the directory you run from. |
| `env` | Environment variables to forward when they are set locally. |
| `tasks` | Named commands for `hopbox run --task NAME`. |

Command-line flags add to these lists: `--include`, `--fetch`, and `--env` values are appended to the repo's.
