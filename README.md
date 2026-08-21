# pf — SSH Port Forward Manager

A Rust CLI + TUI for managing SSH tunnels as background daemons with auto-reconnect, named profiles, and a live dashboard.

Forwards to the same machine share **one multiplexed SSH connection**, so a host pays a single handshake no matter how many ports you forward through it, and adding or removing one is instant — no reconnect.

## Install

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/lgestin/pf/releases/latest/download/pf-installer.sh | sh
```

Or build from source:

```bash
cargo install --path .
```

## Quick Start

```bash
# Start a tunnel (ad-hoc)
pf start myserver 8080:80

# Start with a saved profile
pf config add dev myserver 8080:80
pf start dev

# List running forwards
pf list

# Stop a forward
pf stop dev

# Launch the TUI dashboard
pf tui
```

## CLI Reference

```
pf start <NAME_OR_HOST> [LOCAL:REMOTE]   Start a forward (profile or ad-hoc)
    --name <NAME>           Name for ad-hoc forwards (default: <host>-<port>)
    --no-reconnect          Disable auto-reconnect
    --max-retries <N>       Max reconnect attempts (0 = unlimited, default)
    --retry-delay <SECS>    Delay between retries (default: 5)

pf stop <NAME>              Stop a running forward
    --all                   Stop all
    --host <HOST>           Stop every forward on one machine

pf list                     Table of all forwards with status
    --json                  JSON output

pf restart <NAME>           Stop + start
    --all                   Restart all

pf logs <NAME>              View the session log for that forward's machine
    -f, --follow            Tail the log
    --host <HOST>           Show a machine's session log directly

pf config add <NAME> <HOST> <LOCAL:REMOTE>   Save a profile
pf config remove <NAME>
pf config list

pf hosts                    List SSH hosts from ~/.ssh/config
pf clean                    Forget sessions no live watcher owns
pf completions <SHELL>      Generate shell completions (bash/zsh/fish)
pf tui                      Launch interactive dashboard
```

When `start` gets a single arg matching a saved profile name, it uses that profile. Otherwise it treats it as `HOST LOCAL:REMOTE`.

## SSH Config Integration

`pf` leverages `~/.ssh/config` for everything — host aliases, ProxyCommand, keys, jump hosts all work automatically.

Each machine gets one multiplexing master, which carries **no** `-L` flags of its own:

```
ssh -M -S ~/.pf/run/{host}.sock -N \
    -o ServerAliveInterval=30 \
    -o ServerAliveCountMax=3 \
    -o ConnectTimeout=10 \
    {host}
```

Forwards then attach to and detach from that live connection:

```
ssh -S ~/.pf/run/{host}.sock -O forward -L {local}:{remote_host}:{remote} {host}
ssh -S ~/.pf/run/{host}.sock -O cancel  -L {local}:{remote_host}:{remote} {host}
```

Keeping the master free of `-L` is what stops one failing bind from tearing down its neighbours — which is also why `ExitOnForwardFailure` is deliberately absent. A forward that cannot bind fails alone and reports its own error.

## TUI Dashboard

Launch with `pf tui` for a tree of your machines, each unfolding to the forwards running on it:

```
 pf  ·  all hosts
╭ sessions · 3 ───────────────────────────────────────────────────────╮
│   machine                            state        uptime            │
│ ● ▾ gpu-01 ·2                        connected    2h14m         ↻1  │
│▌●   ├─ 8888 → localhost:8888         forwarded    2h14m             │
│ ●   └─ 6006 → localhost:6006         forwarded    12m               │
│ ● ▾ gpu-02 ·2                        connected    17m04s            │
│ ●   ├─ 3000 → localhost:3000         forwarded    17m04s            │
│ ✕   └─ 5432 → localhost:5432         failed       port in use       │
│ ◐ ▾ turing ·1                        reconnecting              ↻4   │
╰─────────────────────────────────────────────────────────────────────╯
╭ hosts · 2 ──────────────────────────────────────────────────────────╮
│ ○   bastion                                                         │
│ ○   nas                                                             │
╰─────────────────────────────────────────────────────────────────────╯
```

**sessions** holds the machines you have a live connection to, each unfolding to the forwards running on it. **hosts** holds the ones you could connect to but haven't — press `a` on any of them to start a forward and it moves up. Each box scrolls on its own and is sized to what it holds, and a list that is all one kind stays a single box. `·2` is the forward count, `↻4` the reconnect count — shown only once it stops being zero.

Lamps carry state by shape as well as colour, so the tree reads on a monochrome terminal: `●` connected, `◐` connecting or reconnecting (it turns while it works), `✕` failed, `○` idle. The local port carries the accent — it's the number you type into a browser.

### Keybindings

Press `?` for the full list without leaving the dashboard; the bar along the bottom carries the essentials.

| Key | Action |
|-----|--------|
| `j/k` or arrows | Navigate |
| `g` / `G`, `PgUp` / `PgDn`, `ctrl-d` / `ctrl-u` | Jump, page, half-page |
| `Tab` / `Enter` / `Space` | Fold or unfold a machine |
| `→` | Unfold |
| `←` | Collapse, or jump to the parent machine |
| `Z` | Collapse all |
| `a` | Add a forward to the selected machine |
| `A` | Connect to a machine that isn't listed |
| `x` / `d` | Stop — one forward, or all on a machine |
| `r` | Restart a forward, or reconnect a machine |
| `l` | Session log |
| `/` | Filter by machine or forward name — `↑`/`↓` browse the matches |
| `Esc` | Clear the filter |
| `m` | Cycle which machines are listed |
| `s` | Start from a saved profile |
| `o` | Open the forward in a browser |
| `y` | Copy the forward's URL |
| `?` | Every key, in one overlay |
| `q` | Quit |

Starting, stopping, and restarting run in the background — the dashboard says what's in flight and stays responsive while ssh takes its time. The session log follows its tail like `tail -f`, holding your place as soon as you scroll up.

`a` knows which machine you're on, so it only asks for ports. An empty remote port mirrors the local one, and an empty name is generated — the form shows both before you submit. `A` is the way in for a host that isn't in the list: an IP, a `user@host`, or anything hidden behind a wildcard in `~/.ssh/config`.

### Which machines are listed

`m` cycles the list, persisted as `machine_source` under `[tui]` in `config.toml`:

| Value | Shows |
|---|---|
| `all_hosts` | Every `~/.ssh/config` host, plus profile hosts and live sessions (default) |
| `configured` | Hosts with a live session or a saved profile |
| `live` | Only hosts with a live session |

## Auto-Reconnect

Each machine runs a watcher daemon owning its SSH master. If the connection drops, the watcher reconnects with exponential backoff and jitter, then re-attaches every forward the machine had. Kill the master manually (`kill <ssh_pid>`) to test — the watcher picks it right back up.

Retry settings are per **machine**, since the connection is shared: `--max-retries`, `--retry-delay`, and `--no-reconnect` on any `pf start` set them for that forward's host, and the last one wins.

Because the master carries no forwards of its own, one forward failing to bind does not affect the others on that machine — it goes `failed` with ssh's reason while its neighbours keep running.

When a watcher does stop trying — `--no-reconnect`, or `--max-retries` running out — it leaves its last state on disk, so the machine shows as `failed` with the reason rather than disappearing. `r` on it, or `pf restart <name>`, starts a fresh watcher.

### Shutting down

A reboot SIGTERMs every watcher. That ends the sessions, but it is not a decision to stop forwarding, so a departing watcher keeps its desired forward set: what you had running is still recorded after you boot back up.

There is no auto-start, so nothing comes back on its own. Starting any forward on a machine hands its watcher the whole desired set, which brings back the rest along with it:

```bash
pf start lovelace 8080:80    # also restores whatever else that machine had
```

`pf clean` is the other direction — it discards every session no live watcher owns, forgetting what was interrupted.

## Shell Completions

```bash
# Zsh (add to ~/.zshrc)
eval "$(pf completions zsh)"

# Bash (add to ~/.bashrc)
eval "$(pf completions bash)"

# Fish
pf completions fish | source
```

Completions include dynamic SSH host and profile name suggestions for `pf start`, and running forward names for `stop`/`restart`/`logs`.

## State & Config

```
~/.pf/
  config.toml                Saved profiles + [tui] settings
  run/<host>.desired.json    What you asked for — written by the CLI and TUI
  run/<host>.state.json      What is true — written by the watcher
  run/<host>.sock            SSH ControlPath
  run/<host>.lock            Guards read-modify-write of desired state
  logs/<host>.log            Master output + attach/detach events
```

Every file has exactly one writer, which is why intent and observed state are split rather than sharing a file. They also have different lifetimes: observed state describes a process, so it goes when that process does, while intent is the one thing here you typed and outlives the watcher that was serving it.

## Architecture

State is **declarative**. The CLI and TUI only ever edit a machine's desired forward set; a watcher reconciles reality to match it.

Each machine's watcher runs as a detached process (via `setsid()`) and loops:

1. Ensure the SSH master is alive — `ssh -O check`, spawning one if not
2. Read the desired forward set and diff it against what is currently attached
3. `ssh -O forward` the additions, `ssh -O cancel` the removals
4. Write observed state, then sleep until `SIGUSR1` or a 500ms poll

The lifecycle is ref-counted: the first forward brings a machine up, and removing the last one takes it down. On a dropped connection the watcher backs off, reconnects, and runs the *same* diff against an empty attached set — so recovery and first attach are one code path, not two.

A forward that fails to bind is not retried until the desired set changes or the master reconnects. Retrying a permanently conflicting port every 500ms would be a hot loop writing unbounded log spam.

No central daemon — each machine is independent.
