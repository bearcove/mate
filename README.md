# mate

Cooperative agents over tmux.

A coordination server that lets AI agents in separate tmux panes assign tasks
to each other and receive results back — without either agent knowing about tmux.

## How it works

```
┌─────────────┐         ┌─────────────┐
│  Captain    │         │  Mate      │
│  (claude)   │         │  (codex)    │
│             │         │             │
│ mate assign  │────┐    │             │
│  (stdin)    │    │    │             │
└─────────────┘    │    └─────────────┘
                   ▼
            ┌────────────┐
            │ mate server │
            │            │
            │ • pastes task to mate pane
            │ • watches /tmp/mate-responses/
            │ • delivers response back
            └────────────┘
```

1. Captain pipes a task to `mate assign` via stdin
2. The server pastes the task directly into the mate's tmux pane
3. The mate does the work, then pipes their response to `mate respond <id>`
4. The server detects the response file and delivers it back to the captain's pane

Agents never deal with tmux, pane IDs, or polling. They just assign tasks and
receive results as regular chat messages.

## Usage

```
mate                              Show the manual
mate server                       Start the server (usually auto-started)
mate list                         List pending/in-flight requests
mate show <id>                    Show full task content for a request
mate spy <id>                     Peek at mate's pane
mate cancel <id>                  Cancel a pending request
mate wait <id>                    Wait for a response (default 90s timeout)
mate wait <id> --timeout <secs>   Wait with custom timeout
mate issues                       Sync issues to /tmp/mate-issues from cwd repo
cat <<'EOF' | mate steer <id>     Send captain-to-mate clarification
cat <<'EOF' | mate update <id>    Send mate-to-captain progress update
cat <<'EOF' | mate assign                 Assign a task (clears mate context)
cat <<'EOF' | mate assign --keep          Assign, keeping mate's context
cat <<'EOF' | mate assign --title "..."   Assign with an optional title
cat <<'EOF' | mate assign --issue 42      Assign with GitHub issue context
cat <<'EOF' | mate respond <id>           Respond to a task (mates use this)
```

### Mate issues workflow

- `mate issues` infers the repo from `git remote origin` and syncs issues to
  `/tmp/mate-issues/<owner>/<repo>/`.
- Issue data layout:
  - `all/`: one file per issue (`<number> - <title>.md`)
  - `open/`, `closed/`: state views
  - `by-created/`, `by-updated/`: date-based views
  - `labels/`, `milestones/`: optional classification directories
  - `deps/`: optional dependency graph directories
  - `new/`: draft files for creation
  - `.snapshots/`: stored snapshots for change detection
- Reference files:
  - `INDEX.md`, `DEPS.md`, `LABELS.md`, `MILESTONES.md`
- `mate issues` processes drafts and edits before syncing:
  - create files in `new/` using `new/TEMPLATE.md` format → auto-created on next sync
  - edit files in `all/` → changes pushed to GitHub on next sync (compared against `.snapshots/`)
- To assign with issue context, use:
  - `cat <<'EOF' | mate assign --issue 42`
  - the issue file content is injected into the assignment message with reminder text.
- GraphQL support:
  - dependency relationships are pulled into `DEPS.md` and optional `deps/`.

The server auto-starts on first `mate assign` and auto-restarts when the
binary changes (no manual restart needed after `cargo install`).

## Paste detection

Each message is prepended with a random 3-emoji marker (e.g. `🦊🪐🧿`).
After pasting, mate polls the pane for either the emoji marker (small pastes)
or the `[Pasted text ` indicator (large pastes). This ensures the paste has
landed before submitting with Enter.

## Agent detection

Mate finds mate panes by inspecting child processes of each tmux pane shell
using `sysinfo`. Only panes running a `claude` or `codex` binary are considered.
Pane discovery is scoped to the caller's tmux session, so multiple captain/mate
pairs can run in separate sessions without interfering. If no mate is found,
the error now lists inspected panes and their child process names to help debug
why discovery failed.

## Design decisions

These are intentional choices, not bugs:

- **One connection per assign**: Each `mate assign` opens a new roam connection.
  Server-side sessions are dropped after 5 minutes of idle time to avoid
  accumulating dead connections.

- **Response files in /tmp**: Responses are written to `/tmp/mate-responses/`.
  On a shared machine this is spoofable. Mate is a local dev tool — if you're
  running it on a shared server, move state to `$XDG_RUNTIME_DIR`.

- **Request state in per-request directories**: Pending tasks are persisted in
  `/tmp/mate-requests/<id>/` with `meta` (source pane, target pane, optional title)
  and `content` (full task body). This allows pending tasks to survive server restarts.

- **Notify-based response watching with polling fallback**: Mate watches
  `/tmp/mate-responses/` with `notify` for near-instant delivery. If filesystem
  notifications fail, it falls back to periodic polling.

- **8-char request IDs**: First 8 hex chars of a UUID. 4 billion possibilities.
  Collision risk is negligible for local interactive use.

- **Blocking I/O in async context**: tmux commands and sleeps use std blocking
  APIs inside tokio tasks. With one connection at a time this doesn't stall
  anything. Worth fixing if mate ever handles concurrent requests.

- **PID-based server liveness**: `ensure_server_running` checks the pid file
  and `kill -0`. PID reuse could cause false positives, but the binary hash
  auto-restart covers most staleness scenarios.

- **C-u before paste**: `send_to_pane` sends C-u to clear the input line before
  pasting. The mate pane is dedicated to mate — there's no user-typed input to
  preserve.

- **Staleness-based timeout notifications**: Instead of request age, Mate samples
  mate pane content every 30s and notifies once when a pane stays unchanged for
  2 minutes. The notification includes a pane-content dump for quick diagnosis.

- **Issue sync workflow**: `mate issues` keeps issue data on disk in a fixed structure,
  processes draft files from `new/`, can create missing labels and milestones, and
  keeps `INDEX.md`, `DEPS.md`, `LABELS.md`, and `MILESTONES.md` for quick navigation.

## Requirements

- tmux
- Agents running in separate tmux panes (same session)

## Built with

- [roam](https://github.com/bearcove/roam) — RPC over unix socket
- [figue](https://github.com/bearcove/figue) — CLI argument parsing
- [facet](https://github.com/facet-rs/facet) — reflection
- [sysinfo](https://github.com/GuillaumeGomez/sysinfo) — process inspection
