#![allow(dead_code)]

use eyre::Result;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentType {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Idle,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneState {
    pub agent_type: Option<AgentType>,
    pub state: AgentState,
    pub model: Option<String>,
    pub context_remaining_percent: Option<u8>,
    pub activity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(pub String);

#[allow(async_fn_in_trait)]
pub trait Pane: Send + Sync {
    async fn slash_command(&self, command: &str) -> Result<()>;
    async fn chat_message(&self, message: &str) -> Result<()>;
    async fn snapshot(&self) -> Result<PaneState>;
    async fn raw_capture(&self) -> Result<String>;
}

pub struct PaneInfo {
    pub id: PaneId,
    pub session: SessionName,
}

pub struct DiscoveredPane<T: Pane> {
    pub info: PaneInfo,
    pub pane: Arc<T>,
}

#[allow(async_fn_in_trait)]
pub trait PaneDiscovery: Send + Sync {
    type PaneType: Pane + Send + Sync;
    async fn find_peer(&self, me: &PaneId) -> Result<Arc<Self::PaneType>>;
    async fn list_all(&self) -> Result<Vec<DiscoveredPane<Self::PaneType>>>;
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            agent_type: None,
            state: AgentState::Unknown,
            model: None,
            context_remaining_percent: None,
            activity: None,
        }
    }
}

pub fn parse_pane_content(text: &str) -> PaneState {
    let cleaned = strip_ansi(text);
    let lines: Vec<&str> = cleaned.lines().collect();
    if lines.is_empty() {
        return PaneState::default();
    }

    let start = lines.len().saturating_sub(30);
    let recent = &lines[start..];

    if let Some(state) = parse_codex(recent) {
        return state;
    }
    if let Some(state) = parse_claude(recent) {
        return state;
    }

    PaneState::default()
}

fn parse_codex(lines: &[&str]) -> Option<PaneState> {
    let has_prompt = lines.iter().any(|line| line.trim_start().starts_with('›'));
    if !has_prompt {
        return None;
    }

    let has_working = lines
        .iter()
        .any(|line| line.contains("Working (") && line.contains(')'));
    let model = lines
        .iter()
        .rev()
        .find_map(|line| parse_codex_status_line(line));
    let context_remaining_percent = lines
        .iter()
        .rev()
        .find_map(|line| parse_codex_context_percent(line));
    let has_codex_ui_marker = lines.iter().any(|line| {
        line.contains("OpenAI Codex")
            || line.contains("Run /review")
            || line.contains("/statusline")
            || line.contains("context left")
    });
    let model_looks_codex = model
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("codex"));
    let has_codex_identity = has_codex_ui_marker || model_looks_codex;

    if has_working && has_codex_identity {
        let activity = lines
            .iter()
            .rev()
            .find_map(|line| line.trim_start().strip_prefix("• ").map(str::trim))
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        return Some(PaneState {
            agent_type: Some(AgentType::Codex),
            state: AgentState::Working,
            model,
            context_remaining_percent,
            activity,
        });
    }

    if !has_codex_identity {
        return None;
    }

    Some(PaneState {
        agent_type: Some(AgentType::Codex),
        state: AgentState::Idle,
        model,
        context_remaining_percent,
        activity: None,
    })
}

fn parse_claude(lines: &[&str]) -> Option<PaneState> {
    let has_prompt = lines.iter().any(|line| {
        let trimmed = line.trim();
        trimmed == "❯" || trimmed.starts_with("❯ ")
    });
    let spinner_line = lines
        .iter()
        .rev()
        .find_map(|line| parse_claude_spinner_activity(line));
    let has_claude_ui_marker = lines.iter().any(|line| {
        line.contains("Claude Code")
            || line.contains("claude --resume")
            || (line.contains("current:") && line.contains("latest:"))
    });
    let has_claude_completion_marker = lines.iter().any(|line| {
        line.contains("⏺ Done.")
            || (line.contains("Worked for") && (line.contains('✻') || line.contains('✽')))
    });
    let context_remaining_percent = parse_claude_context_percent(lines);
    let has_claude_identity =
        has_claude_ui_marker || has_claude_completion_marker || context_remaining_percent.is_some();

    if let Some(activity_line) = spinner_line {
        if !has_prompt || !has_claude_identity {
            return None;
        }
        return Some(PaneState {
            agent_type: Some(AgentType::Claude),
            state: AgentState::Working,
            model: None,
            context_remaining_percent,
            activity: Some(activity_line),
        });
    }

    if !has_claude_identity || (!has_prompt && !has_claude_completion_marker) {
        return None;
    }

    Some(PaneState {
        agent_type: Some(AgentType::Claude),
        state: AgentState::Idle,
        model: None,
        context_remaining_percent,
        activity: None,
    })
}

fn parse_codex_status_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.contains("gpt-") {
        return None;
    }
    let model_start = trimmed.find("gpt-")?;
    let context_start = parse_codex_context_end(trimmed).unwrap_or(trimmed.len());
    let mut model = trimmed[model_start..context_start].trim().to_string();
    if model.ends_with('·') {
        model.pop();
        model = model.trim().to_string();
    }
    if model.is_empty() {
        return None;
    }

    Some(model)
}

fn parse_claude_context_percent(lines: &[&str]) -> Option<u8> {
    lines
        .iter()
        .rev()
        .find_map(|line| parse_claude_context_percent_from_line(line))
}

fn parse_claude_context_percent_from_line(line: &str) -> Option<u8> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }

        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let digits = &line[start..i];

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        if line[i..].starts_with("tokens") {
            let tokens = digits.parse::<u64>().ok()?;
            let percent_left = 100u64.saturating_sub(tokens.saturating_mul(100) / 200_000);
            return Some(percent_left.min(100) as u8);
        }
    }

    None
}

fn parse_codex_context_end(line: &str) -> Option<usize> {
    line.find("% context left").or_else(|| line.find("% left"))
}

fn parse_codex_context_percent(line: &str) -> Option<u8> {
    parse_codex_context_percent_from_marker(line, "% context left")
        .or_else(|| parse_codex_context_percent_from_marker(line, "% left"))
}

fn parse_codex_context_percent_from_marker(line: &str, marker: &str) -> Option<u8> {
    let left_idx = line.find(marker)?;
    let prefix = &line[..left_idx];
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    let percent = prefix[start..].trim();
    if percent.is_empty() {
        return None;
    }
    let percent_left = percent.parse::<u8>().ok()?;
    Some(percent_left.min(100))
}

fn parse_claude_spinner_activity(line: &str) -> Option<String> {
    const CLAUDE_SPINNER_CHARS: &[char] = &['·', '✢', '✳', '✶', '✻', '✽'];
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !CLAUDE_SPINNER_CHARS.contains(&first) {
        return None;
    }
    if chars.next()? != ' ' {
        return None;
    }
    if !trimmed.contains('…') {
        return None;
    }
    Some(trimmed.to_string())
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }

        if ch == '\n' {
            out.push(ch);
            continue;
        }

        if ch.is_control() {
            continue;
        }

        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests;
