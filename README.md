# bud

Cooperative agents over tmux.

A coordination server that lets AI agents in separate tmux panes assign tasks
to each other and receive results back — without either agent knowing about tmux.

## How it works

```
┌─────────────┐         ┌─────────────┐
│  Agent A    │         │  Agent B    │
│  (lead)     │         │  (worker)   │
│             │         │             │
│ bud assign  │────┐    │             │
│  task.md    │    │    │             │
└─────────────┘    │    └─────────────┘
                   ▼
            ┌────────────┐
            │ bud server │
            │            │
            │ • reads task file
            │ • sends to worker pane
            │ • watches /tmp/bud-responses/
            │ • delivers response back
            └────────────┘
```

1. Agent A writes a task to a file and runs `bud assign task.md`
2. The server reads the file and sends the task to the worker's tmux pane
3. The worker does the work and writes its response to the path it was given
4. The server detects the response file and delivers it back to Agent A's pane

Agents never deal with tmux, pane IDs, or polling. They just assign tasks and
receive results as regular chat messages.

## Usage

```
bud                        Show the manual
bud server                 Start the server (usually auto-started)
bud assign <task-file>     Assign a task to another agent
```

The server auto-starts on first `bud assign` if it isn't already running.

## Requirements

- tmux
- Agents running in separate tmux panes

## Built with

- [roam](https://github.com/bearcove/roam) — RPC over unix socket
- [figue](https://github.com/bearcove/figue) — CLI argument parsing
- [facet](https://github.com/facet-rs/facet) — reflection
