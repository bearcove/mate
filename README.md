# bud

Cooperative agents over tmux.

A coordination server that lets AI agents in separate tmux panes assign tasks
to each other and receive results back — without either agent knowing about tmux.

## How it works

```
┌─────────────┐         ┌─────────────┐
│  Captain    │         │  Buddy      │
│  (claude)   │         │  (codex)    │
│             │         │             │
│ bud assign  │────┐    │             │
│  (stdin)    │    │    │             │
└─────────────┘    │    └─────────────┘
                   ▼
            ┌────────────┐
            │ bud server │
            │            │
            │ • pastes task to buddy pane
            │ • watches /tmp/bud-responses/
            │ • delivers response back
            └────────────┘
```

1. Captain pipes a task to `bud assign` via stdin
2. The server pastes the task directly into the buddy's tmux pane
3. The buddy does the work, then pipes their response to `bud respond <id>`
4. The server detects the response file and delivers it back to the captain's pane

Agents never deal with tmux, pane IDs, or polling. They just assign tasks and
receive results as regular chat messages.

## Usage

```
bud                              Show the manual
bud server                       Start the server (usually auto-started)
bud list                         List pending/in-flight requests
cat <<'EOF' | bud assign         Assign a task (clears buddy context)
cat <<'EOF' | bud assign --keep  Assign, keeping buddy's context
cat <<'EOF' | bud assign --title "..."  Assign with an optional title
cat <<'EOF' | bud respond <id>   Respond to a task (buddies use this)
```

The server auto-starts on first `bud assign` and auto-restarts when the
binary changes (no manual restart needed after `cargo install`).

## Paste detection

Each message is prepended with a random 3-emoji marker (e.g. `🦊🪐🧿`).
After pasting, bud polls the pane for either the emoji marker (small pastes)
or the `[Pasted text ` indicator (large pastes). This ensures the paste has
landed before submitting with Enter.

## Agent detection

Bud finds buddy panes by inspecting child processes of each tmux pane shell
using `sysinfo`. Only panes running a `claude` or `codex` binary are considered.
Pane discovery is scoped to the caller's tmux session, so multiple captain/buddy
pairs can run in separate sessions without interfering. If no buddy is found,
the error now lists inspected panes and their child process names to help debug
why discovery failed.

## Design decisions

These are intentional choices, not bugs:

- **One connection per assign**: Each `bud assign` opens a new roam connection.
  The server-side task parks with `pending().await` to keep the session alive.
  This is fine for a local dev tool doing a few requests per session — connection
  count stays bounded by usage, not by time.

- **Response files in /tmp**: Responses are written to `/tmp/bud-responses/`.
  On a shared machine this is spoofable. Bud is a local dev tool — if you're
  running it on a shared server, move state to `$XDG_RUNTIME_DIR`.

- **Notify-based response watching with polling fallback**: Bud watches
  `/tmp/bud-responses/` with `notify` for near-instant delivery. If filesystem
  notifications fail, it falls back to periodic polling.

- **8-char request IDs**: First 8 hex chars of a UUID. 4 billion possibilities.
  Collision risk is negligible for local interactive use.

- **Blocking I/O in async context**: tmux commands and sleeps use std blocking
  APIs inside tokio tasks. With one connection at a time this doesn't stall
  anything. Worth fixing if bud ever handles concurrent requests.

- **PID-based server liveness**: `ensure_server_running` checks the pid file
  and `kill -0`. PID reuse could cause false positives, but the binary hash
  auto-restart covers most staleness scenarios.

- **C-u before paste**: `send_to_pane` sends C-u to clear the input line before
  pasting. The buddy pane is dedicated to bud — there's no user-typed input to
  preserve.

- **Timeout notifications**: If a request has no response after 10 minutes,
  Bud sends the captain a one-time “your buddy might be stuck” notification.

## Requirements

- tmux
- Agents running in separate tmux panes (same session)

## Built with

- [roam](https://github.com/bearcove/roam) — RPC over unix socket
- [figue](https://github.com/bearcove/figue) — CLI argument parsing
- [facet](https://github.com/facet-rs/facet) — reflection
- [sysinfo](https://github.com/GuillaumeGomez/sysinfo) — process inspection
