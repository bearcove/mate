use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use eyre::Result;

use crate::{pane, paths, tmux, util};

pub(crate) struct IdleTracker {
    now_unix_secs: u64,
    root_dir: PathBuf,
    cache: std::collections::HashMap<(String, String), Option<u64>>,
}

impl IdleTracker {
    pub(crate) fn new(now_unix_secs: u64, root_dir: PathBuf) -> Self {
        Self {
            now_unix_secs,
            root_dir,
            cache: std::collections::HashMap::new(),
        }
    }

    pub(crate) fn update(&mut self, session: &str, pane: &str, state: &pane::AgentState) -> Option<u64> {
        let key = (session.to_string(), pane.to_string());
        let previous_idle_since = if let Some(entry) = self.cache.get(&key) {
            *entry
        } else {
            let loaded = self.load_idle_since(session, pane);
            self.cache.insert(key.clone(), loaded);
            loaded
        };
        let next_idle_since = match state {
            pane::AgentState::Idle => previous_idle_since.or(Some(self.now_unix_secs)),
            pane::AgentState::Working | pane::AgentState::Unknown => None,
        };
        if previous_idle_since != next_idle_since {
            let _ = self.persist_idle_since(session, pane, next_idle_since);
            self.cache.insert(key, next_idle_since);
        }
        next_idle_since.map(|since| self.now_unix_secs.saturating_sub(since))
    }

    fn file_path(&self, session: &str, pane: &str) -> PathBuf {
        self.root_dir.join(session).join(format!("{pane}.idle"))
    }

    fn load_idle_since(&self, session: &str, pane: &str) -> Option<u64> {
        let path = self.file_path(session, pane);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|value| value.trim().parse().ok())
    }

    fn persist_idle_since(&self, session: &str, pane: &str, idle_since: Option<u64>) -> Result<()> {
        let path = self.file_path(session, pane);
        match idle_since {
            Some(value) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, value.to_string())?;
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }
}

pub(crate) fn format_idle_seconds(idle_seconds: Option<u64>) -> String {
    idle_seconds
        .map(|seconds| seconds.to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[derive(Debug, Clone)]
pub(crate) struct RequestListRow {
    pub(crate) session: String,
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) title: Option<String>,
    pub(crate) age: String,
    pub(crate) idle_seconds: Option<u64>,
    pub(crate) response: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentListRow {
    pub(crate) session: String,
    pub(crate) pane_id: String,
    pub(crate) agent: String,
    pub(crate) role: String,
    pub(crate) state: String,
    pub(crate) idle: String,
    pub(crate) context: String,
    pub(crate) activity: String,
    pub(crate) tasks: Vec<String>,
}

fn format_idle_for_block(idle_seconds: Option<u64>) -> String {
    match idle_seconds {
        Some(seconds) => format!("{seconds}s"),
        None => "-".to_string(),
    }
}

fn parse_context_percent_left(context: &str) -> Option<u64> {
    let trimmed = context.trim();
    if let Some(left_idx) = trimmed.find("% left") {
        let prefix = &trimmed[..left_idx];
        let digits_start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| !ch.is_ascii_digit())
            .map(|(idx, ch)| idx + ch.len_utf8())
            .unwrap_or(0);
        if let Ok(percent) = prefix[digits_start..].trim().parse::<u64>() {
            return Some(percent.min(100));
        }
    }
    if let Some(tokens_str) = trimmed.strip_suffix(" tokens")
        && let Ok(tokens) = tokens_str.trim().parse::<u64>()
    {
        let percent_used = tokens.saturating_mul(100) / 200_000;
        return Some(100u64.saturating_sub(percent_used));
    }
    None
}

fn context_progress_bar(percent_left: u64) -> String {
    let clamped = percent_left.min(100);
    let filled = ((clamped + 5) / 10) as usize;
    let mut bar = String::with_capacity(12);
    bar.push('[');
    for i in 0..10 {
        if i < filled {
            bar.push('#');
        } else {
            bar.push('-');
        }
    }
    bar.push(']');
    bar
}

pub(crate) fn format_context_line(context: &str) -> String {
    let trimmed = context.trim();
    if trimmed == "-" {
        return "Context: -".to_string();
    }
    if let Some(percent_left) = parse_context_percent_left(trimmed) {
        if let Some(tokens_str) = trimmed.strip_suffix(" tokens")
            && let Ok(tokens) = tokens_str.trim().parse::<u64>()
        {
            return format!(
                "Context: {tokens} tokens -> {percent_left}% left {}",
                context_progress_bar(percent_left)
            );
        }
        return format!(
            "Context: {percent_left}% left {}",
            context_progress_bar(percent_left)
        );
    }
    format!("Context: {trimmed}")
}

pub(crate) fn render_request_blocks(rows: &[RequestListRow]) -> String {
    let mut blocks = Vec::new();
    for row in rows {
        let title = row
            .title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("-");
        blocks.push(format!(
            "Task: {} @ {} ({} -> {})\nTitle: {}\nAge/Idle/Response: {} / {} / {}",
            row.id,
            row.session,
            row.source,
            row.target,
            title,
            row.age,
            format_idle_for_block(row.idle_seconds),
            row.response
        ));
    }
    blocks.join("\n\n")
}

pub(crate) fn format_status(state: &str, activity: &str) -> String {
    let activity = activity.trim();
    let state_lower = state.to_ascii_lowercase();
    let activity_lower = activity.to_ascii_lowercase();

    if activity != "-" && activity_lower.starts_with(&state_lower) {
        return activity.to_string();
    }

    if activity != "-" {
        return format!("{state} ({activity})");
    }
    state.to_string()
}

pub(crate) fn render_agent_blocks(rows: &[AgentListRow]) -> String {
    let mut blocks = Vec::new();
    for row in rows {
        let mut lines = vec![format!(
            "Agent: {} @ {}/{} | Role: {}",
            row.agent, row.session, row.pane_id, row.role
        )];
        if !row.tasks.is_empty() {
            lines.push(format!("Task: {}", row.tasks.join(", ")));
        }
        lines.push(format_context_line(&row.context));
        let base_status = format_status(&row.state, &row.activity);
        if row.state.eq_ignore_ascii_case("idle") && row.idle != "-" {
            lines.push(format!("Status: {base_status} ({}s)", row.idle));
        } else {
            lines.push(format!("Status: {base_status}"));
        }
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

pub(crate) fn render_session_groups(request_rows: &[RequestListRow], agent_rows: &[AgentListRow]) -> String {
    let mut sessions: BTreeSet<String> = BTreeSet::new();
    for row in request_rows {
        sessions.insert(row.session.clone());
    }
    for row in agent_rows {
        sessions.insert(row.session.clone());
    }

    let mut out = String::new();
    let mut first = true;
    for session in sessions {
        if !first {
            out.push('\n');
        }
        first = false;

        out.push_str(&format!("Session {session}\n"));
        let session_requests: Vec<RequestListRow> = request_rows
            .iter()
            .filter(|row| row.session == session)
            .cloned()
            .collect();
        if !session_requests.is_empty() {
            out.push_str("Tasks:\n");
            out.push_str(&render_request_blocks(&session_requests));
            out.push('\n');
        }

        let session_agents: Vec<AgentListRow> = agent_rows
            .iter()
            .filter(|row| row.session == session)
            .cloned()
            .collect();
        if !session_agents.is_empty() {
            if !session_requests.is_empty() {
                out.push('\n');
            }
            out.push_str("Agents:\n");
            out.push_str(&render_agent_blocks(&session_agents));
            out.push('\n');
        }
    }

    out.trim_end().to_string()
}

pub(crate) fn format_agent_task_summary(request_id: &str, title: Option<&str>) -> String {
    match title.map(str::trim).filter(|value| !value.is_empty()) {
        Some(title) => format!("{request_id} ({title})"),
        None => request_id.to_string(),
    }
}

pub(crate) fn classify_agent_role(session: &str, pane_id: &str, requests: &[RequestListRow]) -> &'static str {
    let mut is_source = false;
    let mut is_target = false;
    for request in requests.iter().filter(|request| request.session == session) {
        if request.source == pane_id {
            is_source = true;
        }
        if request.target == pane_id {
            is_target = true;
        }
    }
    match (is_source, is_target) {
        (true, false) => "Captain",
        (false, true) => "Mate",
        (true, true) => "Mixed",
        (false, false) => "Unknown",
    }
}

pub(crate) fn list_requests() -> Result<()> {
    use std::time::SystemTime;

    struct Row {
        session: String,
        id: String,
        source: String,
        target: String,
        title: Option<String>,
        age: String,
        idle_seconds: Option<u64>,
        response: String,
    }

    let mut rows: Vec<Row> = Vec::new();
    let now = SystemTime::now();
    let now_unix_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let mut idle_tracker = IdleTracker::new(now_unix_secs, paths::idle_tracking_root_dir());
    let request_root = paths::request_root_dir();
    if let Ok(session_entries) = std::fs::read_dir(&request_root) {
        for session_entry in session_entries.flatten() {
            if !session_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let session_name = session_entry.file_name().to_string_lossy().to_string();
            let session_request_dir = session_entry.path();
            let session_response_dir = paths::response_dir(&session_name);
            let request_entries = match std::fs::read_dir(&session_request_dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in request_entries.flatten() {
                if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let id = entry.file_name().to_string_lossy().to_string();
                let (source_pane, target_pane, title) = util::read_request_meta(&entry.path())
                    .map(|meta| (meta.source_pane, meta.target_pane, meta.title))
                    .unwrap_or_else(|| ("(unreadable)".to_string(), "(unknown)".to_string(), None));
                let age = entry
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.created().ok().or_else(|| meta.modified().ok()))
                    .and_then(|timestamp| now.duration_since(timestamp).ok())
                    .map(util::format_age)
                    .unwrap_or_else(|| "unknown".to_string());
                let idle_seconds = tmux::capture_pane(&target_pane)
                    .ok()
                    .map(|capture| pane::parse_pane_content(&capture))
                    .and_then(|parsed| {
                        if parsed.agent_type.is_some() {
                            Some(idle_tracker.update(&session_name, &target_pane, &parsed.state))
                        } else {
                            None
                        }
                    })
                    .flatten();
                let response_exists = if session_response_dir.join(format!("{id}.md")).exists() {
                    "yes".to_string()
                } else {
                    "no".to_string()
                };
                rows.push(Row {
                    session: session_name.clone(),
                    id,
                    source: source_pane,
                    target: target_pane,
                    title,
                    age,
                    idle_seconds,
                    response: response_exists,
                });
            }
        }
    }

    if rows.is_empty() {
        eprintln!("No tasks in flight — all clear!");
    }

    rows.sort_by(|a, b| a.session.cmp(&b.session).then(a.id.cmp(&b.id)));
    let request_rows: Vec<RequestListRow> = rows
        .iter()
        .map(|row| RequestListRow {
            session: row.session.clone(),
            id: row.id.clone(),
            source: row.source.clone(),
            target: row.target.clone(),
            title: row.title.clone(),
            age: row.age.clone(),
            idle_seconds: row.idle_seconds,
            response: row.response.clone(),
        })
        .collect();

    match tmux::list_all_panes() {
        Ok(panes) => {
            let mut tasks_by_agent: HashMap<(String, String), Vec<String>> = HashMap::new();
            for row in &rows {
                tasks_by_agent
                    .entry((row.session.clone(), row.target.clone()))
                    .or_default()
                    .push(format_agent_task_summary(&row.id, row.title.as_deref()));
            }
            let mut agent_rows: Vec<AgentListRow> = Vec::new();
            for p in &panes {
                let capture = tmux::capture_pane(&p.id).unwrap_or_default();
                let parsed = pane::parse_pane_content(&capture);
                let Some(agent_type) = parsed.agent_type else {
                    continue;
                };
                let agent = match agent_type {
                    pane::AgentType::Claude => "Claude",
                    pane::AgentType::Codex => "Codex",
                };
                let state = match parsed.state {
                    pane::AgentState::Working => "Working",
                    pane::AgentState::Idle => "Idle",
                    pane::AgentState::Unknown => "Unknown",
                };
                let idle_seconds = idle_tracker.update(&p.session_name, &p.id, &parsed.state);
                let context = parsed.context_remaining.unwrap_or_else(|| "-".to_string());
                let activity = parsed
                    .activity
                    .map(|value| value.replace('\n', " "))
                    .unwrap_or_else(|| "-".to_string());
                agent_rows.push(AgentListRow {
                    session: p.session_name.clone(),
                    pane_id: p.id.clone(),
                    agent: agent.to_string(),
                    role: classify_agent_role(&p.session_name, &p.id, &request_rows).to_string(),
                    state: state.to_string(),
                    idle: format_idle_seconds(idle_seconds),
                    context,
                    activity,
                    tasks: tasks_by_agent
                        .get(&(p.session_name.clone(), p.id.clone()))
                        .cloned()
                        .unwrap_or_default(),
                });
            }

            eprintln!("{}", render_session_groups(&request_rows, &agent_rows));
        }
        Err(e) => {
            eprintln!("Panes unavailable: {e}");
        }
    }

    Ok(())
}
