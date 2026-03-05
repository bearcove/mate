mod client;
mod config;
mod discord;
mod github;
mod hash;
mod pane;
mod paths;
mod protocol;
mod server;
mod listing;
mod tmux;
mod util;
mod warmth;
mod watch;

use eyre::Result;
use facet::Facet;
use figue as args;
use paths::{
    log_path, pid_path, read_stdin, request_dir, request_root_dir, response_root_dir, socket_path,
    tmux_session_name, tmux_session_name_for_pane,
};
use tracing::trace;

#[derive(Facet, Debug)]
struct Args {
    #[facet(args::subcommand)]
    command: Option<Command>,
}

#[derive(Facet, Debug)]
#[repr(u8)]
enum Command {
    /// Start the mate server in the foreground
    Server,
    /// List pending/in-flight requests
    List,
    /// Cancel a pending request
    Cancel {
        /// The request ID to cancel
        #[facet(args::positional)]
        request_id: String,
    },
    /// Show full task details for a request
    Show {
        /// The request ID to show
        #[facet(args::positional)]
        request_id: String,
    },
    /// Capture and show the mate pane for a request
    Spy {
        /// The request ID to spy on
        #[facet(args::positional)]
        request_id: String,
    },
    /// Steer a mate on an in-flight request (reads from stdin)
    Steer {
        /// The request ID to steer
        #[facet(args::positional)]
        request_id: String,
    },
    /// Send a progress update to the captain (reads from stdin)
    Update {
        /// The request ID to update
        #[facet(args::positional)]
        request_id: String,
    },
    /// Accept a completed task (captain-only)
    Accept {
        /// The request ID to accept
        #[facet(args::positional)]
        request_id: String,
    },
    /// Sync GitHub issues for the current repo and write them to disk
    Issues,
    /// Compact the captain's context (reads summary from stdin)
    Compact,
    /// Assign a task to another agent (reads from stdin)
    Assign {
        /// Keep the worker's existing context (default: clear it)
        #[facet(args::named)]
        keep: bool,
        /// Optional short title for the task
        #[facet(args::named)]
        title: Option<String>,
        /// Attach a GitHub issue context by number
        #[facet(args::named)]
        issue: Option<u64>,
    },
    /// Respond to a task (internal/backward-compatible)
    Respond {
        /// The request ID to respond to
        #[facet(args::positional)]
        request_id: String,
    },
    /// Wait for a response with optional timeout
    Wait {
        /// The request ID to wait on
        #[facet(args::positional)]
        request_id: String,
        /// Timeout in seconds (default: 90)
        #[facet(args::named)]
        timeout: Option<u64>,
    },
    /// Watch CI for the current branch and report results to this pane
    Watch,
    /// Internal command used by `mate watch`
    _WatchInner {
        /// tmux pane id to report results to
        #[facet(args::positional)]
        pane: String,
    },
}

const MANUAL: &str = r#"mate - cooperative agents over tmux

USAGE:
    mate                              Show this manual
    mate server                       Start the server (usually auto-started)
    mate list                         List pending/in-flight requests
    mate cancel <id>                  Cancel a pending request
    mate show <id>                    Show full task content for a request
    mate spy <id>                     Peek at mate's pane
    mate accept <id>                  Accept a completed task (captain-only)
    cat <<'EOF' | mate steer <id>     Steer mate on a pending request
    cat <<'EOF' | mate update <id>    Send progress update to captain
    mate wait <id>                    Wait for a response (default 90s timeout)
    mate wait <id> --timeout <secs>   Wait with custom timeout
    mate watch                        Watch latest CI run for current branch
    mate issues                       Sync GitHub issues for current repo
    cat <<'EOF' | mate compact        Compact captain context with stdin summary
    cat <<'EOF' | mate assign                 Assign a task (clears worker context)
    cat <<'EOF' | mate assign --keep          Assign, keeping worker's context
    cat <<'EOF' | mate assign --title "..."   Assign with a title
    cat <<'EOF' | mate assign --issue 42      Assign with GitHub issue context
    cat <<'EOF' | mate respond <id>   Internal/backward-compatible response command

EXAMPLES:
    # Assign a task:
    cat <<'EOF' | mate assign
    Review the error handling in src/server.rs
    EOF

    # Send a progress update:
    cat <<'EOF' | mate update abc12345
    I reviewed it, here's what I found...
    EOF

ENVIRONMENT:
    TMUX_PANE    Automatically set by tmux. Used to identify your pane.
    MATE_SOCKET   Override the server socket path (default: /tmp/mate.sock)
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = figue::from_std_args().unwrap();

    // `mate server` initializes its own tracing subscriber (with a different default filter),
    // so avoid calling `init()` twice which would panic.
    if !matches!(args.command, Some(Command::Server)) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_env("MATE_LOG")
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    match args.command {
        None => {
            print!("{MANUAL}");
            Ok(())
        }
        Some(Command::Server) => {
            server::run_server(
                socket_path(),
                pid_path(),
                response_root_dir(),
                request_root_dir(),
                log_path(),
            )
            .await
        }
        Some(Command::List) => listing::list_requests(),
        Some(Command::Cancel { request_id }) => client::cancel_request(&request_id).await,
        Some(Command::Show { request_id }) => show_request(&request_id),
        Some(Command::Spy { request_id }) => spy_request(&request_id),
        Some(Command::Steer { request_id }) => client::steer_request(&request_id).await,
        Some(Command::Accept { request_id }) => client::accept_request(&request_id).await,
        Some(Command::Update { request_id }) => client::update_request(&request_id).await,
        Some(Command::Issues) => sync_issues_to_pane(),
        Some(Command::Compact) => compact_context(),
        Some(Command::Assign { keep, title, issue }) => {
            let pane = std::env::var("TMUX_PANE")
                .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
            let session_name = tmux_session_name_for_pane(&pane)?;
            let content = read_stdin()?;
            client::client_assign(pane, session_name, content, !keep, title, issue).await
        }
        Some(Command::Respond { request_id }) => {
            client::validate_request_id(&request_id)?;
            let content = read_stdin()?;
            let session_name = tmux_session_name()?;
            client::rpc_respond(&request_id, &session_name, &content).await
        }
        Some(Command::Wait {
            request_id,
            timeout,
        }) => {
            let timeout_secs = timeout.unwrap_or(90);
            client::wait_for_response(&request_id, timeout_secs).await
        }
        Some(Command::Watch) => watch::watch_ci(),
        Some(Command::_WatchInner { pane }) => watch::watch_ci_inner(&pane),
    }
}

fn compact_context() -> Result<()> {
    let summary = read_stdin()?;
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;

    let list_output = std::process::Command::new("mate").arg("list").output()?;
    let task_list = if list_output.status.success() {
        let stdout = String::from_utf8_lossy(&list_output.stdout)
            .trim()
            .to_string();
        if stdout.is_empty() {
            "none".to_string()
        } else {
            stdout
        }
    } else {
        "none".to_string()
    };

    let prompt = format!(
        "/captain\nYou've just been compacted. Here is your context summary from before compaction:\n\n{summary}\n\nIn-flight tasks at time of compaction:\n{task_list}"
    );

    tmux::send_to_pane(&pane, "/clear")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    tmux::send_to_pane(&pane, &prompt)?;
    Ok(())
}

#[cfg(test)]
fn format_captain_update_for_buddy(request_id: &str, message: &str) -> String {
    format!(
        "📌 Update from the captain on task {request_id}:\n\n\
         {message}\n\n\
         If you hit a decision point, want to share progress, or need clarification, send an update:\n\n\
         cat <<'MATEEOF' | mate update {request_id}\n\
         <your progress update here>\n\
         MATEEOF"
    )
}

#[cfg(test)]
mod tests {
    use crate::listing::{
        AgentListRow, IdleTracker, RequestListRow,
        classify_agent_role, format_agent_task_summary, format_context_line, format_idle_seconds,
        format_status, render_agent_blocks, render_request_blocks,
        render_session_groups,
    };
    use super::{
        cleanup_created_draft, format_captain_update_for_buddy, format_missing_draft_message,
        DraftCleanupOutcome, DraftMissingStage,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn captain_update_includes_buddy_response_instructions() {
        let request_id = "deadbeef";
        let update = format_captain_update_for_buddy(request_id, "Please focus on parser tests.");

        assert!(update.contains("📌 Update from the captain on task deadbeef:"));
        assert!(update.contains("cat <<'MATEEOF' | mate update deadbeef"));
        assert!(!update.contains("mate accept deadbeef"));
        assert!(update.contains("<your progress update here>"));
        assert!(!update.contains("<your reply here>"));
        assert!(!update.contains("mate respond deadbeef"));
    }

    #[test]
    fn missing_draft_message_mentions_concurrency_only_with_evidence() {
        let path = Path::new("/tmp/mate-issues/example/new/draft.md");

        let neutral = format_missing_draft_message(path, DraftMissingStage::AfterCreate, false);
        assert!(neutral.contains("already removed after create"));
        assert!(!neutral.to_ascii_lowercase().contains("concurrent"));

        let concurrent = format_missing_draft_message(path, DraftMissingStage::AfterCreate, true);
        assert!(concurrent.contains("Concurrent `mate issues` run detected."));
    }

    #[test]
    fn cleanup_created_draft_handles_removed_and_missing_states() {
        let root = std::env::temp_dir().join(format!("mate-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp directory");
        let existing = root.join("existing.md");
        std::fs::write(&existing, "draft").expect("write draft file");

        let removed = cleanup_created_draft(&existing).expect("remove existing draft");
        assert_eq!(removed, DraftCleanupOutcome::Removed);
        assert!(!existing.exists(), "existing draft should be removed");

        let missing = root.join("missing.md");
        let missing_outcome = cleanup_created_draft(&missing).expect("remove missing draft");
        assert_eq!(missing_outcome, DraftCleanupOutcome::Missing);

        std::fs::remove_dir_all(&root).expect("remove temp directory");
    }

    #[test]
    fn idle_tracker_updates_and_resets_on_activity() {
        let root = std::env::temp_dir().join(format!("mate-idle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create idle test directory");

        let mut tracker = IdleTracker::new(100, root.clone());
        assert_eq!(
            tracker.update("sess", "%42", &crate::pane::AgentState::Idle),
            Some(0)
        );

        let mut tracker = IdleTracker::new(108, root.clone());
        assert_eq!(
            tracker.update("sess", "%42", &crate::pane::AgentState::Idle),
            Some(8)
        );

        let mut tracker = IdleTracker::new(120, root.clone());
        assert_eq!(
            tracker.update("sess", "%42", &crate::pane::AgentState::Working),
            None
        );

        let idle_file = PathBuf::from(&root).join("sess").join("%42.idle");
        assert!(
            !idle_file.exists(),
            "idle tracking file should be removed after activity resumes"
        );

        let mut tracker = IdleTracker::new(130, root.clone());
        assert_eq!(
            tracker.update("sess", "%42", &crate::pane::AgentState::Idle),
            Some(0)
        );

        std::fs::remove_dir_all(&root).expect("remove idle test directory");
    }

    #[test]
    fn list_headers_include_idle_seconds_column() {
        let request_blocks = render_request_blocks(&[RequestListRow {
            session: "sess".to_string(),
            id: "deadbeef".to_string(),
            source: "%1".to_string(),
            target: "%2".to_string(),
            title: Some("example title".to_string()),
            age: "12s".to_string(),
            idle_seconds: Some(42),
            response: "no".to_string(),
        }]);
        let agent_blocks = render_agent_blocks(&[AgentListRow {
            session: "sess".to_string(),
            pane_id: "%2".to_string(),
            agent: "Codex".to_string(),
            role: "Mate".to_string(),
            state: "Idle".to_string(),
            idle: "42".to_string(),
            context: "98% left".to_string(),
            activity: "Running checks".to_string(),
            tasks: vec!["deadbeef (Example)".to_string()],
        }]);

        assert!(request_blocks.contains("Age/Idle/Response:"));
        assert!(request_blocks.contains("42s"));
        assert!(agent_blocks.contains("Task: deadbeef (Example)"));
        assert!(agent_blocks.contains("Context: 98% left"));
        assert!(agent_blocks.contains("Status:"));
        assert!(!agent_blocks.contains("\nIdle:"));
        assert_eq!(format_idle_seconds(Some(42)), "42");
        assert_eq!(format_idle_seconds(None), "-");
    }

    #[test]
    fn request_blocks_follow_grouped_shape() {
        let blocks = render_request_blocks(&[RequestListRow {
            session: "session-alpha".to_string(),
            id: "deadbeef".to_string(),
            source: "%1".to_string(),
            target: "%2".to_string(),
            title: Some("Long title for readability".to_string()),
            age: "12s".to_string(),
            idle_seconds: Some(7),
            response: "no".to_string(),
        }]);
        assert!(blocks.contains("Task: deadbeef @ session-alpha (%1 -> %2)"));
        assert!(blocks.contains("Title: Long title for readability"));
        assert!(blocks.contains("Age/Idle/Response: 12s / 7s / no"));
    }

    #[test]
    fn agent_blocks_follow_grouped_shape() {
        let blocks = render_agent_blocks(&[AgentListRow {
            session: "3".to_string(),
            pane_id: "%24".to_string(),
            agent: "Claude".to_string(),
            role: "Mate".to_string(),
            state: "Working".to_string(),
            idle: "0".to_string(),
            context: "35% left".to_string(),
            activity: "17s - esc to interrupt".to_string(),
            tasks: vec!["805fbe4a (static-edit-verifier-167)".to_string()],
        }]);
        assert!(blocks.contains("Agent: Claude @ 3/%24 | Role: Mate"));
        assert!(blocks.contains("Task: 805fbe4a (static-edit-verifier-167)"));
        assert!(blocks.contains("Context: 35% left [####------]"));
        assert!(blocks.contains("Status: Working (17s - esc to interrupt)"));
        assert!(!blocks.contains("Working (Working"));
        assert!(!blocks.contains("\nIdle:"));
    }

    #[test]
    fn block_renderer_separates_multiple_entries_with_blank_line() {
        let requests = render_request_blocks(&[
            RequestListRow {
                session: "s".to_string(),
                id: "aaaaaaaa".to_string(),
                source: "%1".to_string(),
                target: "%2".to_string(),
                title: Some("one".to_string()),
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            },
            RequestListRow {
                session: "s".to_string(),
                id: "bbbbbbbb".to_string(),
                source: "%1".to_string(),
                target: "%3".to_string(),
                title: Some("two".to_string()),
                age: "2m".to_string(),
                idle_seconds: Some(5),
                response: "yes".to_string(),
            },
        ]);
        assert!(requests.contains("no\n\nTask: bbbbbbbb"));
    }

    #[test]
    fn agent_blocks_omit_task_line_when_none_assigned() {
        let blocks = render_agent_blocks(&[AgentListRow {
            session: "3".to_string(),
            pane_id: "%6".to_string(),
            agent: "Codex".to_string(),
            role: "Unknown".to_string(),
            state: "Idle".to_string(),
            idle: "0".to_string(),
            context: "-".to_string(),
            activity: "-".to_string(),
            tasks: Vec::new(),
        }]);
        assert!(!blocks.contains("Task: -"));
        assert!(!blocks.contains("\nTask:"));
        assert!(blocks.contains("Status: Idle (0s)"));
    }

    #[test]
    fn agent_task_summary_includes_title_when_present() {
        assert_eq!(
            format_agent_task_summary("deadbeef", Some("My title")),
            "deadbeef (My title)"
        );
        assert_eq!(format_agent_task_summary("deadbeef", None), "deadbeef");
    }

    #[test]
    fn claude_tokens_context_normalizes_to_percent_line() {
        assert_eq!(
            format_context_line("73740 tokens"),
            "Context: 73740 tokens -> 64% left [######----]"
        );
    }

    #[test]
    fn session_grouping_contains_session_heading_and_both_sections() {
        let output = render_session_groups(
            &[RequestListRow {
                session: "3".to_string(),
                id: "805fbe4a".to_string(),
                source: "%6".to_string(),
                target: "%24".to_string(),
                title: Some("static-edit-verifier-167".to_string()),
                age: "35m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            }],
            &[AgentListRow {
                session: "3".to_string(),
                pane_id: "%24".to_string(),
                agent: "Codex".to_string(),
                role: "Mate".to_string(),
                state: "Working".to_string(),
                idle: "0".to_string(),
                context: "35% left".to_string(),
                activity: "17s - esc to interrupt".to_string(),
                tasks: vec!["805fbe4a (static-edit-verifier-167)".to_string()],
            }],
        );
        assert!(output.contains("Session 3"));
        assert!(output.contains("Tasks:"));
        assert!(output.contains("Agents:"));
        assert!(output.contains("Agent: Codex @ 3/%24"));
        assert!(output.contains("Task: 805fbe4a (static-edit-verifier-167)"));
    }

    #[test]
    fn session_grouping_omits_empty_section_placeholders() {
        let output = render_session_groups(
            &[RequestListRow {
                session: "3".to_string(),
                id: "deadbeef".to_string(),
                source: "%6".to_string(),
                target: "%24".to_string(),
                title: None,
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            }],
            &[],
        );
        assert!(output.contains("Session 3"));
        assert!(output.contains("Tasks:"));
        assert!(!output.contains("Agents:"));
        assert!(!output.contains("Agent: -"));
        assert!(!output.contains("Task: -"));
    }

    #[test]
    fn classify_agent_role_captain_buddy_mixed_unknown() {
        let requests = vec![
            RequestListRow {
                session: "3".to_string(),
                id: "a".to_string(),
                source: "%6".to_string(),
                target: "%24".to_string(),
                title: None,
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            },
            RequestListRow {
                session: "3".to_string(),
                id: "b".to_string(),
                source: "%24".to_string(),
                target: "%6".to_string(),
                title: None,
                age: "1m".to_string(),
                idle_seconds: Some(0),
                response: "no".to_string(),
            },
        ];
        assert_eq!(classify_agent_role("3", "%7", &requests), "Unknown");
        assert_eq!(classify_agent_role("3", "%6", &requests), "Mixed");
        assert_eq!(classify_agent_role("3", "%24", &requests), "Mixed");
        assert_eq!(
            classify_agent_role(
                "3",
                "%1",
                &[RequestListRow {
                    session: "3".to_string(),
                    id: "x".to_string(),
                    source: "%1".to_string(),
                    target: "%2".to_string(),
                    title: None,
                    age: "1m".to_string(),
                    idle_seconds: Some(0),
                    response: "no".to_string(),
                }]
            ),
            "Captain"
        );
        assert_eq!(
            classify_agent_role(
                "3",
                "%2",
                &[RequestListRow {
                    session: "3".to_string(),
                    id: "x".to_string(),
                    source: "%1".to_string(),
                    target: "%2".to_string(),
                    title: None,
                    age: "1m".to_string(),
                    idle_seconds: Some(0),
                    response: "no".to_string(),
                }]
            ),
            "Mate"
        );
    }

    #[test]
    fn status_format_dedups_repeated_state_prefix() {
        assert_eq!(
            format_status("Working", "Working (17s - esc to interrupt)"),
            "Working (17s - esc to interrupt)"
        );
        assert_eq!(
            format_status("Working", "17s - esc to interrupt"),
            "Working (17s - esc to interrupt)"
        );
    }

    #[test]
    fn idle_status_merges_idle_seconds_on_status_line() {
        let blocks = render_agent_blocks(&[AgentListRow {
            session: "3".to_string(),
            pane_id: "%6".to_string(),
            agent: "Codex".to_string(),
            role: "Captain".to_string(),
            state: "Idle".to_string(),
            idle: "24".to_string(),
            context: "67% left".to_string(),
            activity: "-".to_string(),
            tasks: vec!["deadbeef".to_string()],
        }]);
        assert!(blocks.contains("Status: Idle (24s)"));
        assert!(!blocks.contains("\nIdle:"));
    }
}

fn show_request(request_id: &str) -> Result<()> {
    client::validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let content = util::read_request_content(&path)
        .ok_or_else(|| eyre::eyre!("Task {request_id} is missing request content."))?;
    eprintln!("Task {request_id}");
    eprintln!("Source: {}  Target: {}", meta.source_pane, meta.target_pane);
    eprintln!("Title: {}", meta.title.as_deref().unwrap_or("(none)"));
    eprintln!();
    eprintln!("{content}");
    Ok(())
}

fn spy_request(request_id: &str) -> Result<()> {
    client::validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let pane_content = tmux::capture_pane(&meta.target_pane)?;
    eprintln!("Pane {}:\n{}", meta.target_pane, pane_content);
    Ok(())
}

 

fn sync_issues_to_pane() -> Result<()> {
    trace!("sync_issues_to_pane: enter");
    let repo = github::infer_repo()?;
    eprintln!("Syncing issues for {repo}...");

    trace!("sync_issues_to_pane: before process_pending_issue_drafts");
    let (created, failed) = process_pending_issue_drafts(&repo)?;

    trace!("sync_issues_to_pane: before github::sync_issues");
    let issues = github::sync_issues(&repo)?;
    trace!("sync_issues_to_pane: before github::write_issue_files");
    let result = github::write_issue_files(&repo, &issues)?;
    trace!("sync_issues_to_pane: after github::write_issue_files");

    let mut summary = String::new();
    if !result.issue_edits_applied.is_empty() {
        summary.push_str("Applied issue edits:\n");
        for update in &result.issue_edits_applied {
            summary.push_str(&format!(
                "  Updated issue #{}: {}\n",
                update.number,
                update.changes.join(", ")
            ));
        }
        summary.push('\n');
    }
    if !created.is_empty() {
        summary.push_str(&format!("Created {} new issues:\n", created.len()));
        for pending in &created {
            summary.push_str(&format!(
                "  #{number}: {title} — {url}\n",
                number = pending.number,
                title = pending.title,
                url = pending.url
            ));
        }
        summary.push('\n');
    }

    for failure in &failed {
        summary.push_str(&format!(
            "Failed to create {}: {}\n",
            failure.filename, failure.error
        ));
    }
    if !failed.is_empty() {
        summary.push('\n');
    }

    if !result.issue_edit_errors.is_empty() {
        summary.push_str("Issue edit failures:\n");
        for failure in &result.issue_edit_errors {
            summary.push_str(&format!("- {failure}\n"));
        }
        summary.push('\n');
    }

    summary.push_str(&format!(
        "Synced {repo} — {} open, {} closed. Index: {}\n",
        result.open_count,
        result.closed_count,
        result.index_path.display()
    ));
    println!("{summary}");
    Ok(())
}

struct PendingIssueCreated {
    number: u64,
    url: String,
    title: String,
}

struct PendingIssueFailed {
    filename: String,
    error: String,
}

#[derive(Clone, Copy)]
enum DraftMissingStage {
    BeforeRead,
    AfterCreate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DraftCleanupOutcome {
    Removed,
    Missing,
}

fn format_missing_draft_message(
    path: &std::path::Path,
    stage: DraftMissingStage,
    has_concurrency_evidence: bool,
) -> String {
    let base = match stage {
        DraftMissingStage::BeforeRead => {
            format!(
                "Skipping draft {}: file disappeared before read.",
                path.display()
            )
        }
        DraftMissingStage::AfterCreate => {
            format!("Draft {} already removed after create.", path.display())
        }
    };
    if has_concurrency_evidence {
        format!("{base} Concurrent `mate issues` run detected.")
    } else {
        base
    }
}

fn cleanup_created_draft(path: &std::path::Path) -> std::io::Result<DraftCleanupOutcome> {
    trace!("cleanup_created_draft: attempt {}", path.display());
    match fs_err::remove_file(path) {
        Ok(()) => {
            trace!("cleanup_created_draft: removed {}", path.display());
            Ok(DraftCleanupOutcome::Removed)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            trace!(
                "cleanup_created_draft: missing before remove {}",
                path.display()
            );
            Ok(DraftCleanupOutcome::Missing)
        }
        Err(e) => {
            trace!(
                "cleanup_created_draft: failed remove {} => {e}",
                path.display()
            );
            Err(e)
        }
    }
}

fn process_pending_issue_drafts(
    repo: &str,
) -> Result<(Vec<PendingIssueCreated>, Vec<PendingIssueFailed>)> {
    use std::io::ErrorKind;

    let base_dir = github::issue_repo_dir(repo);
    let new_dir = base_dir.join("new");
    trace!(
        "process_pending_issue_drafts: base {} new {}",
        base_dir.display(),
        new_dir.display()
    );
    if !new_dir.is_dir() {
        trace!("process_pending_issue_drafts: new dir missing");
        return Ok((Vec::new(), Vec::new()));
    }

    let failed_dir = base_dir.join("failed");
    trace!(
        "process_pending_issue_drafts: failed_dir {}",
        failed_dir.display()
    );
    fs_err::create_dir_all(&failed_dir)?;
    trace!("process_pending_issue_drafts: ensured failed_dir");

    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    let entries = fs_err::read_dir(&new_dir)?;
    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(e) => {
                trace!("process_pending_issue_drafts: read_dir entry error: {e}");
                continue;
            }
        };
        let raw_path = entry.path();
        trace!(
            "process_pending_issue_drafts: discovered (pre-filter) {}",
            raw_path.display()
        );
        if !entry.file_type().is_ok_and(|ft| ft.is_file()) {
            trace!(
                "process_pending_issue_drafts: filtered non-file {}",
                raw_path.display()
            );
            continue;
        }
        if raw_path.extension().is_none_or(|ext| ext != "md") {
            trace!(
                "process_pending_issue_drafts: filtered non-md {}",
                raw_path.display()
            );
            continue;
        }
        if entry.file_name().to_string_lossy() == "TEMPLATE.md" {
            trace!(
                "process_pending_issue_drafts: filtered TEMPLATE {}",
                raw_path.display()
            );
            continue;
        }
        trace!("process_pending_issue_drafts: kept {}", raw_path.display());
        paths.push(raw_path);
    }
    paths.sort();

    if paths.is_empty() {
        trace!("process_pending_issue_drafts: no drafts");
        return Ok((Vec::new(), Vec::new()));
    }
    trace!(
        "process_pending_issue_drafts: processing {} draft(s)",
        paths.len()
    );

    let mut existing_labels = github::sync_labels_set(repo)?;
    let mut existing_milestones = github::sync_milestones_set(repo)?;
    let mut created = Vec::new();
    let mut failed = Vec::new();

    for path in paths {
        eprintln!("[#38] processing draft path: {}", path.display());
        let original_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(String::new);
        trace!("process_pending_issue_drafts: reading {}", path.display());
        let content = match fs_err::read_to_string(&path) {
            Ok(content) => {
                trace!("process_pending_issue_drafts: read ok {}", path.display());
                content
            }
            Err(e) => {
                eprintln!("[#38] read failed before read for {}: {e}", path.display());
                if e.kind() == ErrorKind::NotFound {
                    eprintln!(
                        "{}",
                        format_missing_draft_message(&path, DraftMissingStage::BeforeRead, false)
                    );
                    continue;
                }
                if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
                    eprintln!("Failed {original_name}: move_to_failed_failed: {move_err}");
                }
                failed.push(PendingIssueFailed {
                    filename: original_name,
                    error: format!("read failed at {}: {e}", path.display()),
                });
                continue;
            }
        };

        eprintln!("[#38] read succeeded for {}", path.display());
        trace!("process_pending_issue_drafts: parse {}", path.display());
        let draft = match github::parse_new_issue(&content) {
            Ok(issue) => issue,
            Err(e) => {
                trace!(
                    "process_pending_issue_drafts: parse failed {} => {e}",
                    path.display()
                );
                if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
                    trace!(
                        "process_pending_issue_drafts: move_file result {} -> {} failed: {move_err}",
                        path.display(),
                        failed_dir.join(&original_name).display()
                    );
                    eprintln!("Failed {original_name}: move_to_failed_failed: {move_err}");
                }
                failed.push(PendingIssueFailed {
                    filename: original_name,
                    error: format!("parse failed for {}: {e}", path.display()),
                });
                continue;
            }
        };

        let mut prep_error: Option<String> = None;
        for label in &draft.labels {
            if existing_labels.contains(label) {
                continue;
            }
            if let Err(e) = github::ensure_label_exists(repo, label) {
                prep_error = Some(format!("label '{label}' creation failed: {e}"));
                break;
            }
            existing_labels.insert(label.clone());
        }
        if prep_error.is_none()
            && let Some(milestone) = draft.milestone.as_deref()
            && !existing_milestones.contains(milestone)
        {
            if let Err(e) = github::ensure_milestone_exists(repo, milestone) {
                prep_error = Some(format!("milestone '{milestone}' creation failed: {e}"));
            } else {
                existing_milestones.insert(milestone.to_string());
            }
        }

        if let Some(error_message) = prep_error {
            trace!(
                "process_pending_issue_drafts: prep error for {} => {error_message}",
                path.display()
            );
            if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
                eprintln!("Failed {original_name}: move_to_failed_failed: {move_err}");
            }
            failed.push(PendingIssueFailed {
                filename: original_name,
                error: error_message,
            });
            continue;
        }

        match github::create_issue(repo, &draft) {
            Ok((number, url)) => {
                eprintln!("[#38] created issue {} for {}", number, path.display());
                trace!(
                    "process_pending_issue_drafts: cleanup_created_draft before {}",
                    path.display()
                );
                match cleanup_created_draft(&path) {
                    Ok(DraftCleanupOutcome::Removed) => {}
                    Ok(DraftCleanupOutcome::Missing) => {
                        eprintln!(
                            "{}",
                            format_missing_draft_message(
                                &path,
                                DraftMissingStage::AfterCreate,
                                false
                            )
                        );
                    }
                    Err(e) => {
                        trace!(
                            "process_pending_issue_drafts: cleanup failed {} => {e}",
                            path.display()
                        );
                        if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
                            eprintln!("Failed {original_name}: move_to_failed_failed: {move_err}");
                        }
                        failed.push(PendingIssueFailed {
                            filename: original_name,
                            error: format!("cleanup failed at {}: {e}", path.display()),
                        });
                        continue;
                    }
                }
                eprintln!("[#38] cleanup succeeded for {}", path.display());
                trace!(
                    "process_pending_issue_drafts: created and cleaned up {} as {}",
                    path.display(),
                    number
                );
                created.push(PendingIssueCreated {
                    number,
                    url,
                    title: draft.title,
                });
            }
            Err(e) => {
                trace!(
                    "process_pending_issue_drafts: create failed {} => {e}",
                    path.display()
                );
                if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
                    eprintln!("Failed {original_name}: move_to_failed_failed: {move_err}");
                }
                failed.push(PendingIssueFailed {
                    filename: original_name,
                    error: format!("create failed for {}: {e}", path.display()),
                });
            }
        }
    }

    Ok((created, failed))
}

fn move_file(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    use std::io::ErrorKind;

    trace!("move_file: {} -> {}", from.display(), to.display());
    if to.exists() {
        trace!("move_file: dest exists before remove {}", to.display());
        fs_err::remove_file(to)?;
    }
    if let Err(e) = fs_err::rename(from, to) {
        if e.kind() == ErrorKind::NotFound {
            trace!("move_file: source not found {}", from.display());
            return Ok(());
        }
        trace!(
            "move_file: rename failed {} -> {}: {e}",
            from.display(),
            to.display()
        );
        return Err(e.into());
    }
    trace!(
        "move_file: rename ok {} -> {}",
        from.display(),
        to.display()
    );
    Ok(())
}
