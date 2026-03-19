# pf — SSH Port Forward Manager

A Rust CLI + TUI for managing SSH tunnels as background daemons with auto-reconnect, named profiles, and a live dashboard.

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

pf list                     Table of all forwards with status
    --json                  JSON output

pf restart <NAME>           Stop + start
    --all                   Restart all

pf logs <NAME>              View watcher log
    -f, --follow            Tail the log

pf config add <NAME> <HOST> <LOCAL:REMOTE>   Save a profile
pf config remove <NAME>
pf config list

pf hosts                    List SSH hosts from ~/.ssh/config
pf clean                    Remove stale state files
pf completions <SHELL>      Generate shell completions (bash/zsh/fish)
pf tui                      Launch interactive dashboard
```

When `start` gets a single arg matching a saved profile name, it uses that profile. Otherwise it treats it as `HOST LOCAL:REMOTE`.

## SSH Config Integration

`pf` leverages `~/.ssh/config` for everything — host aliases, ProxyCommand, keys, jump hosts all work automatically. The SSH command used:

```
ssh -N -L {local}:{remote_host}:{remote} \
    -o ServerAliveInterval=30 \
    -o ServerAliveCountMax=3 \
    -o ExitOnForwardFailure=yes \
    -o ConnectTimeout=10 \
    {host}
```

## TUI Dashboard

Launch with `pf tui` for a full-featured interactive manager:

- **Top panel**: table of all forwards (name, host, ports, status, uptime, reconnects)
- **Bottom panel**: log viewer, new forward form, or profile picker
- **Status bar**: keybinding hints

### Keybindings

| Key | Action |
|-----|--------|
| `j/k` or arrows | Navigate |
| `s` | Start from saved profile |
| `n` | New ad-hoc forward |
| `x` / `d` | Stop selected (with confirmation) |
| `r` | Restart selected |
| `Enter` / `l` | View logs |
| `Esc` | Back |
| `q` | Quit |

The host input field provides **Tab-completion** from your `~/.ssh/config` hosts.

Status colors: green=running, yellow=reconnecting, red=failed, grey=stale.

## Auto-Reconnect

Each tunnel runs as an isolated watcher daemon. If SSH drops, the watcher automatically reconnects with configurable retry delay and max attempts. Kill it manually (`kill <ssh_pid>`) to test — the watcher picks it right back up.

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
  config.toml           Saved profiles
  run/<name>.json       Runtime state per forward (PIDs, status, timestamps)
  logs/<name>.log       Watcher + SSH output
```

## Architecture

Each `pf start` spawns a **watcher daemon** — a separate process (via `setsid()`) that owns one SSH tunnel:

1. Validates args, checks port conflicts and name collisions
2. Self-re-execs `pf watcher` as a detached child
3. Watcher opens log file, spawns SSH, monitors it in a loop
4. On SSH exit + auto-reconnect enabled: wait, respawn
5. On SIGTERM (from `pf stop`): kill SSH, clean up, exit

No central daemon, no threads — each tunnel is fully isolated.
