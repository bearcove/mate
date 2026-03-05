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
            context_remaining,
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
        context_remaining,
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
    let context_remaining = extract_tokens_phrase(lines);
    let has_claude_identity =
        has_claude_ui_marker || has_claude_completion_marker || context_remaining.is_some();

    if let Some(activity_line) = spinner_line {
        if !has_prompt || !has_claude_identity {
            return None;
        }
        return Some(PaneState {
            agent_type: Some(AgentType::Claude),
            state: AgentState::Working,
            model: None,
            context_remaining,
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
        context_remaining,
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

#[cfg(test)]
mod tests {
    use super::{AgentState, AgentType, parse_pane_content};

    #[test]
    fn detects_claude_working_snapshot_from_recording() {
        let text = "\
✽ Combobulating… (0s)

────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  esc to interrupt                                                             0 tokens
                                                       current: 2.1.68 · latest: 2.1.68
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, Some(AgentType::Claude));
        assert_eq!(parsed.state, AgentState::Working);
    }

    #[test]
    fn detects_claude_idle_snapshot_from_recording() {
        let text = "\
⏺ Done.
✻ Worked for 1m 14s

Resume this session with:
claude --resume eea841a9-c5e8-4176-a995-c52ddd9a3c23
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, Some(AgentType::Claude));
        assert_eq!(parsed.state, AgentState::Idle);
    }

    #[test]
    fn detects_codex_working_snapshot_from_recording() {
        let text = "\
• Working (35s • esc to interrupt) · 1 background terminal running · /ps to view · /clean to close

› Run /review on my current changes

  gpt-5.3-codex medium · 98% left · ~/bearcove/mucp
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, Some(AgentType::Codex));
        assert_eq!(parsed.state, AgentState::Working);
    }

    #[test]
    fn detects_codex_idle_snapshot_from_recording() {
        let text = "\
╭────────────────────────────────────────────────────╮
│ >_ OpenAI Codex (v0.107.0)                         │
│ model:     gpt-5.3-codex medium   /model to change │
╰────────────────────────────────────────────────────╯

› Run /review on my current changes
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, Some(AgentType::Codex));
        assert_eq!(parsed.state, AgentState::Idle);
    }

    #[test]
    fn does_not_misclassify_plain_shell_prompt_as_claude() {
        let text = "\
~/repo
❯ ls -la
Cargo.toml
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, None);
        assert_eq!(parsed.state, AgentState::Unknown);
    }

    #[test]
    fn does_not_misclassify_generic_gpt_status_as_codex() {
        let text = "\
› run the checks
gpt-4.1 mini · 80% left · ~/repo
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, None);
        assert_eq!(parsed.state, AgentState::Unknown);
    }

    #[test]
    fn does_not_misclassify_generic_working_status_as_codex() {
        let text = "\
›
• Working (12s • esc to interrupt)
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, None);
        assert_eq!(parsed.state, AgentState::Unknown);
    }

    #[test]
    fn does_not_misclassify_spinner_like_line_as_claude() {
        let text = "\
❯
✻ Indexing… (0s)
";
        let parsed = parse_pane_content(text);
        assert_eq!(parsed.agent_type, None);
        assert_eq!(parsed.state, AgentState::Unknown);
    }
}
