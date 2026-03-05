# Pane Abstraction

## Problem

`tmux::send_to_pane` is a stringly-typed mess that conflates formatting with
delivery. It sends `/clear`, uses emoji markers for paste detection, does
blocking sleeps, and every caller has to understand tmux internals. Completely
untestable without a real tmux session.

The staleness detection system (compare raw terminal content strings across
polls) is a broken simulacrum of idle detection, which we already have and
which actually works. Staleness should be removed.

`context_remaining` is currently `Option<String>` holding values like
`"98% left"` or `"1234 tokens"` — consumers have to parse it back into a
number. The conversion already exists in `listing.rs:parse_context_percent_left`
but it happens at display time instead of parse time.

## Design

### Newtypes

```rust
/// Opaque pane identifier. In tmux this is something like "%42".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(String);

/// Tmux session name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionName(String);
```

### PaneState (revised)

```rust
pub enum AgentType {
    Claude,
    Codex,
}

pub enum AgentState {
    Working,
    Idle,
    Unknown,
}

pub struct PaneState {
    pub agent_type: Option<AgentType>,
    pub state: AgentState,
    pub model: Option<String>,
    /// Percentage of context window remaining (0-100).
    /// Computed at parse time:
    ///   - Claude: 100 - ((tokens_used * 100) / 200_000)
    ///   - Codex: parsed directly from "98% left"
    pub context_remaining_percent: Option<u8>,
    pub activity: Option<String>,
}
```

### Pane trait

Two delivery primitives. One observation primitive.

```rust
trait Pane: Send + Sync {
    /// Submit a slash command (e.g. "/clear", "/captain", "/mate").
    /// Single line only.
    ///
    /// Delivery must use the same safety pipeline as chat_message:
    /// - exit copy mode best-effort
    /// - clear pending input (C-u)
    /// - send text
    /// - wait for confirmation by observing the exact command in pane capture
    /// - submit with C-m
    /// - optional bounded retry for transient tmux failures
    async fn slash_command(&self, command: &str) -> Result<()>;

    /// Submit a chat message as user input to the agent.
    /// May be multi-line. Handles paste detection, confirmation, etc.
    async fn chat_message(&self, message: &str) -> Result<()>;

    /// Observe the agent's current state.
    async fn snapshot(&self) -> Result<PaneState>;
}
```

The formatting of *what to say* stays in the caller. The `Pane` trait is purely
about delivery and observation.

### Implementations

**TmuxPane** (production)
- Holds a `PaneId`.
- `slash_command`: uses the hardened delivery path (copy-mode exit, C-u, send,
  wait for exact command echo in capture, C-m). No emoji markers.
- `chat_message`: The current dance — C-u, send-keys with emoji marker,
  wait_for_paste, C-m.
- `snapshot`: `capture-pane -p` + `parse_pane_content`.

**TestPane** (testing)
- `messages: Arc<Mutex<Vec<TestDelivery>>>` where `TestDelivery` is
  `SlashCommand(String)` or `ChatMessage(String)`.
- `snapshots: Arc<Mutex<VecDeque<PaneState>>>` — preconfigured sequence
  returned by `snapshot()`.
- Tests assert on typed `TestDelivery` values, not strings.

### PaneDiscovery (separate trait)

`find_other_pane`, `list_panes`, `list_all_panes` become a separate trait
that returns `Pane` handles instead of raw pane ID strings.

```rust
struct PaneInfo {
    pub id: PaneId,
    pub session: SessionName,
}

struct DiscoveredPane {
    pub info: PaneInfo,
    pub pane: Arc<dyn Pane>,
}

trait PaneDiscovery: Send + Sync {
    /// Find a peer agent pane in the same session.
    async fn find_peer(&self, me: &PaneId) -> Result<Arc<dyn Pane>>;

    /// List all panes across all sessions.
    async fn list_all(&self) -> Result<Vec<DiscoveredPane>>;
}
```

**TmuxPaneDiscovery**: sysinfo + tmux subprocess inspection.
**TestPaneDiscovery**: returns preconfigured `TestPane` instances.

## What gets deleted

- The staleness detection system (`unchanged_count`, `captain_unchanged_count`,
  `captain_last_content`, `STALENESS_NOTIFY_AFTER_UNCHANGED`, the raw content
  comparison loop in `staleness_checks`). Replace with idle detection on both
  panes.
- `parse_context_percent_left` in listing.rs — the conversion moves into
  the parser.
- `format_context_line` string parsing — it receives an `f64` now.
- Raw `tmux::capture_pane` / `tmux::send_to_pane` calls from server.rs,
  client.rs, listing.rs, requests.rs, watch.rs — they all go through
  `dyn Pane` instead.
