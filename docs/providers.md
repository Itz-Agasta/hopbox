# Rent a GPU box

`hopbox up vast` rents a GPU instance on [vast.ai](https://vast.ai), waits until SSH works, and makes it usable as `--host <name>`. Every rental has a deadline: the instance destroys itself when its time-to-live (TTL) runs out, even if your laptop is off.

## Before you start

You need:

- the `vastai` CLI, logged in: `pip install vastai`, then `vastai set api-key <key>`
- your SSH public key registered with vast: `vastai create ssh-key ~/.ssh/id_ed25519.pub`

hopbox runs the `vastai` CLI for you and never stores or sends your vast credentials.

## Rent an instance

```sh
hopbox up vast --gpu RTX_4090 --ttl 2h --max-price 0.60 --name gpu1
```

hopbox searches for the best-value offer that matches, shows it, and asks before renting:

```text
Rent RTX 4090 x1 · $0.45/h · South Korea · reliability 0.99 · up to $0.91 for 2h? [y/N]
```

After you answer `y`, hopbox creates the instance, waits for it to start (up to 10 minutes), waits for SSH, and prints the name to use:

```text
hopbox: gpu1 is ready (destroys itself in 1h59m): hopbox run --host gpu1 -- <command>
```

| Option | What it does |
|---|---|
| `--gpu NAME` | GPU model as vast names it, such as `RTX_4090` or `H100_SXM`. Required. |
| `--ttl AGE` | Destroy the instance after `AGE`, such as `30m`, `2h`, or `1d`. Required, at most `30d`. |
| `--max-price USD` | Highest price to accept, in US dollars per hour. |
| `--name NAME` | Name for `--host`: 1 to 32 lowercase letters, digits, or `-`. Default: `vast-` plus 4 random hex characters. Can't match a host in your config. |
| `--disk GB` | Disk size. Default: `30`. |
| `--image IMAGE` | Container image. Default: `vastai/pytorch:@vastai-automatic-tag`. |
| `--yes-spend` | Rent without asking. Use it only after a human has approved the price. |

hopbox only considers verified machines with direct ports and reliability above 0.98. It prices each offer with the disk size you asked for and rechecks the price against `--max-price` before renting.

Without a terminal to ask on, `up` exits with code 125 unless you pass `--yes-spend`. If the instance fails to start, never reaches SSH, or doesn't start within 10 minutes, hopbox destroys it and exits with code 1.

## Run jobs on it

A rented instance works like any configured host:

```sh
hopbox run --host gpu1 --tail 60 -- python train.py
```

hopbox connects as the instance's user on the port vast assigns, and keeps the instance's host key in `~/.local/state/hopbox/known_hosts`, not in `~/.ssh/known_hosts`. Rented instances have no resource caps, so `run` prints `hopbox: warning: no resource caps on gpu1`.

## Check instances and cost

```sh
hopbox ls
```

```text
NAME                 ID  STATUS    GPU              $/H      AGE     COST  LEFT
gpu1           31337000  running   RTX 4090        0.45      42m    $0.32  1h17m
```

`COST` is an estimate: the hourly price times the time since the instance started. `LEFT` is the time until the instance destroys itself. An instance labelled by hopbox that this laptop has no record of shows `untracked`. `--json` prints the same rows as a JSON array, with `age` and `left` in seconds.

`ls` also cleans up: it destroys any instance past its deadline, in case the instance couldn't destroy itself, and forgets instances that no longer exist on vast.

## Destroy an instance

```sh
hopbox down gpu1
```

`down` checks that the instance still carries its `hopbox-gpu1` label, destroys it, and forgets it. Destroying stops all billing, including storage. If the instance no longer exists on vast, `down` prints `gpu1 was already gone` and forgets it.

## How the deadline works

When hopbox creates an instance, it sets an onstart command that waits until the deadline and then deletes the instance through vast's API with the instance's own key. vast gives every instance a key that can only start, stop, or destroy that one instance, so no credentials leave your laptop. The deadline is a fixed time, so restarting the container doesn't extend it. The command uses `curl`, which the default image includes; if you pass `--image`, use one that has `curl`.

If you stop hopbox while `up` is waiting, the instance keeps running until its deadline. Run `hopbox ls` to see it and `hopbox down <name>` to destroy it.

## What hopbox touches

hopbox labels every instance it creates `hopbox-<name>` and only ever lists, checks, or destroys instances with that label. Your other vast instances are never touched.
