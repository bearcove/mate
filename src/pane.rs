#![allow(dead_code)]

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
    pub context_remaining: Option<String>,
    pub activity: Option<String>,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            agent_type: None,
            state: AgentState::Unknown,
            model: None,
            context_remaining: None,
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
    let context_remaining = lines
        .iter()
        .rev()
        .find_map(|line| extract_codex_context(line));
    let has_codex_ui_marker = lines.iter().any(|line| {
        line.contains("OpenAI Codex")
            || line.contains("Run /review")
            || line.contains("/statusline")
            || line.contains("context left")
            || line.contains("gpt-")
    });

    if has_working {
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
            context_remaining,
            activity,
        });
    }

    if !has_codex_ui_marker && model.is_none() && context_remaining.is_none() {
        return None;
    }

    Some(PaneState {
        agent_type: Some(AgentType::Codex),
        state: AgentState::Idle,
        model,
        context_remaining,
        activity: None,
    })
}

fn parse_claude(lines: &[&str]) -> Option<PaneState> {
    let has_prompt = lines.iter().any(|line| {
        let trimmed = line.trim();
        trimmed == "❯" || trimmed.starts_with("❯ ")
    });
    if !has_prompt {
        return None;
    }

    let spinner_line = lines
        .iter()
        .rev()
        .find_map(|line| parse_claude_spinner_activity(line));
    if let Some(activity_line) = spinner_line {
        return Some(PaneState {
            agent_type: Some(AgentType::Claude),
            state: AgentState::Working,
            model: None,
            context_remaining: extract_tokens_phrase(lines),
            activity: Some(activity_line),
        });
    }

    Some(PaneState {
        agent_type: Some(AgentType::Claude),
        state: AgentState::Idle,
        model: None,
        context_remaining: extract_tokens_phrase(lines),
        activity: None,
    })
}

fn parse_codex_status_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.contains("gpt-") {
        return None;
    }
    let model_start = trimmed.find("gpt-")?;
    let context = extract_codex_context(trimmed);
    let context_start = context
        .as_deref()
        .and_then(|value| trimmed.find(value))
        .unwrap_or(trimmed.len());
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

fn extract_tokens_phrase(lines: &[&str]) -> Option<String> {
    lines
        .iter()
        .rev()
        .find_map(|line| extract_tokens_from_line(line))
}

fn extract_tokens_from_line(line: &str) -> Option<String> {
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
            return Some(format!("{digits} tokens"));
        }
    }

    None
}

fn extract_codex_context(line: &str) -> Option<String> {
    if let Some(left_idx) = line.find("% left") {
        let prefix = &line[..left_idx];
        let start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| !ch.is_ascii_digit())
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        let percent = prefix[start..].trim();
        if !percent.is_empty() {
            return Some(format!("{percent}% left"));
        }
    }
    if let Some(left_idx) = line.find("% context left") {
        let prefix = &line[..left_idx];
        let start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| !ch.is_ascii_digit())
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        let percent = prefix[start..].trim();
        if !percent.is_empty() {
            return Some(format!("{percent}% context left"));
        }
    }
    None
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
