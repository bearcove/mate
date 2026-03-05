mod config;
mod discord;
mod github;
mod hash;
mod pane;
mod protocol;
mod server;
mod tmux;
mod util;
mod warmth;

use eyre::Result;
use facet::Facet;
use figue as args;
use std::io::Read as _;
use std::path::PathBuf;
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
    mate issues                       Sync GitHub issues for current repo
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

fn socket_path() -> PathBuf {
    std::env::var("MATE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/mate.sock"))
}

fn pid_path() -> PathBuf {
    PathBuf::from("/tmp/mate.pid")
}

fn response_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-responses")
}

fn response_dir(session_name: &str) -> PathBuf {
    response_root_dir().join(session_name)
}

fn request_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-requests")
}

fn request_dir(session_name: &str) -> PathBuf {
    request_root_dir().join(session_name)
}

fn idle_tracking_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-idle")
}

fn log_path() -> PathBuf {
    PathBuf::from("/tmp/mate-server.log")
}

fn tmux_session_name_for_pane(pane: &str) -> Result<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-t", pane, "-p", "#{session_name}"])
        .output()?;
    if !output.status.success() {
        return Err(eyre::eyre!("tmux display-message failed for pane {pane}"));
    }
    let session_name = String::from_utf8(output.stdout)?.trim().to_string();
    if session_name.is_empty() {
        return Err(eyre::eyre!("tmux returned empty session name"));
    }
    Ok(session_name)
}

fn tmux_session_name() -> Result<String> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
    tmux_session_name_for_pane(&pane)
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Err(eyre::eyre!("no input on stdin"));
    }
    Ok(buf)
}

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
        Some(Command::List) => list_requests(),
        Some(Command::Cancel { request_id }) => cancel_request(&request_id).await,
        Some(Command::Show { request_id }) => show_request(&request_id),
        Some(Command::Spy { request_id }) => spy_request(&request_id),
        Some(Command::Steer { request_id }) => steer_request(&request_id).await,
        Some(Command::Accept { request_id }) => accept_request(&request_id).await,
        Some(Command::Update { request_id }) => update_request(&request_id).await,
        Some(Command::Issues) => sync_issues_to_pane(),
        Some(Command::Assign { keep, title, issue }) => {
            let pane = std::env::var("TMUX_PANE")
                .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
            let session_name = tmux_session_name_for_pane(&pane)?;
            let content = read_stdin()?;
            ensure_server_running().await?;
            client_assign(pane, session_name, content, !keep, title, issue).await
        }
        Some(Command::Respond { request_id }) => {
            validate_request_id(&request_id)?;
            let content = read_stdin()?;
            let session_name = tmux_session_name()?;
            ensure_server_running().await?;
            rpc_respond(&request_id, &session_name, &content).await
        }
        Some(Command::Wait {
            request_id,
            timeout,
        }) => {
            let timeout_secs = timeout.unwrap_or(90);
            wait_for_response(&request_id, timeout_secs).await
        }
    }
}

async fn ensure_server_running() -> Result<()> {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = pid_str.trim().parse::<u32>()
    {
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status();
        if status.is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }

    // Server not running — clean up stale socket if any
    let socket = socket_path();
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }

    eprintln!("Starting mate server...");
    let exe = std::env::current_exe()?;
    let log_file = std::fs::File::create(log_path())?;
    std::process::Command::new(exe)
        .arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    for _ in 0..50 {
        if socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(eyre::eyre!(
        "mate: server failed to start (check {})",
        log_path().display()
    ))
}

async fn client_assign(
    source_pane: String,
    session_name: String,
    content: String,
    clear: bool,
    title: Option<String>,
    issue: Option<u64>,
) -> Result<()> {
    let binary_hash = hash::binary_hash();
    let content = if let Some(issue_number) = issue {
        let repo = github::infer_repo()?;
        let issue_content = github::read_issue_file(&repo, issue_number)?;
        format!(
            "--- GitHub Issue #{issue_number} Context ---\n{issue_content}\n--- End Issue Context ---\n\n{content}\n\nNote: This task is linked to GitHub issue #{issue_number}. Please reference #{issue_number} in any commit messages."
        )
    } else {
        content
    };

    match assign_once(
        &source_pane,
        &session_name,
        &content,
        clear,
        title.clone(),
        &binary_hash,
    )
    .await
    {
        Ok(request_id) => {
            eprintln!("{}", warmth::assigned());
            eprintln!("Request ID: {request_id}");
            print_request_followup_help(&request_id);
            Ok(())
        }
        Err(first_error) => {
            eprintln!("mate: assign failed: {first_error:?}");
            ensure_server_running().await?;
            let request_id = assign_once(
                &source_pane,
                &session_name,
                &content,
                clear,
                title,
                &binary_hash,
            )
            .await
            .map_err(|e| {
                eprintln!("mate: assign failed after retry: {e:?}");
                eyre::eyre!("assign failed after retry: {e:?}")
            })?;
            eprintln!("{}", warmth::assigned());
            eprintln!("Request ID: {request_id}");
            print_request_followup_help(&request_id);
            Ok(())
        }
    }
}

fn print_request_followup_help(request_id: &str) {
    eprintln!();
    eprintln!("What's next:");
    eprintln!("  Your mate is working now. You have nothing to do on this task until they reply.");
    eprintln!("  Their response will arrive through user input automatically.");
    eprintln!("  Use this free time to plan your next move.");
    eprintln!();
    eprintln!(
        "  mate spy {request_id}                         - peek at what your mate's pane looks like right now"
    );
    eprintln!(
        "  mate list                                 - see all in-flight requests and their status"
    );
    eprintln!(
        "  cat <<'EOF' | mate steer {request_id}         - send a mid-task clarification or course correction"
    );
    eprintln!(
        "  cat <<'EOF' | mate update {request_id}        - (mate uses this) send a progress update without completing"
    );
    eprintln!("  mate accept {request_id}                      - accept the task and close it");
    eprintln!("  mate cancel {request_id}                      - cancel the task entirely");
}

async fn with_coop_client<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(protocol::CoopClient) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    use roam_stream::StreamLink;
    let stream = tokio::net::UnixStream::connect(socket_path())
        .await
        .map_err(|e| eyre::eyre!("failed to connect to mate server: {e}"))?;
    let (client, _sh) = roam::initiator(StreamLink::unix(stream))
        .establish::<protocol::CoopClient>(())
        .await?;
    f(client).await
}

async fn assign_once(
    source_pane: &str,
    session_name: &str,
    content: &str,
    clear: bool,
    title: Option<String>,
    binary_hash: &str,
) -> Result<String> {
    with_coop_client(|client| async move {
        client
            .assign(protocol::AssignRequest {
                source_pane: source_pane.to_string(),
                session_name: session_name.to_string(),
                content: content.to_string(),
                title,
                clear,
                binary_hash: binary_hash.to_string(),
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await
}

fn validate_request_id(request_id: &str) -> Result<()> {
    if request_id.len() != 8
        || !request_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(eyre::eyre!("invalid request ID (expected 8 hex chars)"));
    }
    Ok(())
}

async fn cancel_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .cancel(protocol::CancelRequest {
                request_id: request_id.to_string(),
                session_name,
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;
    eprintln!("Task {request_id} cancelled.");
    Ok(())
}

async fn steer_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let message = read_stdin()?;
    let session_name = tmux_session_name()?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .steer(protocol::SteerRequest {
                request_id: request_id.to_string(),
                session_name,
                content: message,
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;
    eprintln!("Sent steer update for task {request_id}.");
    print_request_followup_help(request_id);
    Ok(())
}

async fn accept_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    ensure_server_running().await?;

    with_coop_client(|client| async move {
        client
            .accept(protocol::AcceptRequest {
                request_id: request_id.to_string(),
                session_name,
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;

    eprintln!("Task {request_id} accepted.");

    Ok(())
}

async fn rpc_respond(request_id: &str, session_name: &str, content: &str) -> Result<()> {
    with_coop_client(|client| async move {
        client
            .respond(protocol::RespondRequest {
                request_id: request_id.to_string(),
                session_name: session_name.to_string(),
                content: content.to_string(),
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;
    eprintln!("{}", warmth::responded());
    Ok(())
}

async fn update_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let content = read_stdin()?;
    let session_name = tmux_session_name()?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .update(protocol::UpdateRequest {
                request_id: request_id.to_string(),
                session_name,
                content,
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;
    eprintln!("Update sent for task {request_id}.");
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
    use super::{
        AgentListRow, DraftCleanupOutcome, DraftMissingStage, IdleTracker, RequestListRow,
        classify_agent_role, cleanup_created_draft, format_agent_task_summary,
        format_captain_update_for_buddy, format_context_line, format_idle_seconds,
        format_missing_draft_message, format_status, render_agent_blocks, render_request_blocks,
        render_session_groups,
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
    validate_request_id(request_id)?;
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
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let pane_content = tmux::capture_pane(&meta.target_pane)?;
    eprintln!("Pane {}:\n{}", meta.target_pane, pane_content);
    Ok(())
}

struct IdleTracker {
    now_unix_secs: u64,
    root_dir: PathBuf,
    cache: std::collections::HashMap<(String, String), Option<u64>>,
}

impl IdleTracker {
    fn new(now_unix_secs: u64, root_dir: PathBuf) -> Self {
        Self {
            now_unix_secs,
            root_dir,
            cache: std::collections::HashMap::new(),
        }
    }

    fn update(&mut self, session: &str, pane: &str, state: &pane::AgentState) -> Option<u64> {
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

fn format_idle_seconds(idle_seconds: Option<u64>) -> String {
    idle_seconds
        .map(|seconds| seconds.to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[derive(Debug, Clone)]
struct RequestListRow {
    session: String,
    id: String,
    source: String,
    target: String,
    title: Option<String>,
    age: String,
    idle_seconds: Option<u64>,
    response: String,
}

#[derive(Debug, Clone)]
struct AgentListRow {
    session: String,
    pane_id: String,
    agent: String,
    role: String,
    state: String,
    idle: String,
    context: String,
    activity: String,
    tasks: Vec<String>,
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

fn format_context_line(context: &str) -> String {
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

fn render_request_blocks(rows: &[RequestListRow]) -> String {
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

fn format_status(state: &str, activity: &str) -> String {
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

fn render_agent_blocks(rows: &[AgentListRow]) -> String {
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

fn render_session_groups(request_rows: &[RequestListRow], agent_rows: &[AgentListRow]) -> String {
    let mut sessions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
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

fn format_agent_task_summary(request_id: &str, title: Option<&str>) -> String {
    match title.map(str::trim).filter(|value| !value.is_empty()) {
        Some(title) => format!("{request_id} ({title})"),
        None => request_id.to_string(),
    }
}

fn classify_agent_role(session: &str, pane_id: &str, requests: &[RequestListRow]) -> &'static str {
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

fn list_requests() -> Result<()> {
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
    let mut idle_tracker = IdleTracker::new(now_unix_secs, idle_tracking_root_dir());
    let request_root = request_root_dir();
    if let Ok(session_entries) = std::fs::read_dir(&request_root) {
        for session_entry in session_entries.flatten() {
            if !session_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let session_name = session_entry.file_name().to_string_lossy().to_string();
            let session_request_dir = session_entry.path();
            let session_response_dir = response_dir(&session_name);
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
            let mut tasks_by_agent: std::collections::HashMap<(String, String), Vec<String>> =
                std::collections::HashMap::new();
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

async fn wait_for_response(request_id: &str, timeout_secs: u64) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let request_path = request_dir(&session_name).join(request_id);
    if !request_path.join("meta").is_file() {
        return Err(eyre::eyre!(
            "No matching request found for {request_id} in session {session_name}."
        ));
    }

    let buddy_pane = util::read_request_meta(&request_path)
        .map(|meta| meta.target_pane)
        .unwrap_or_default();

    ensure_server_running().await?;

    eprintln!("Waiting for response on {request_id} for up to {timeout_secs}s...");

    with_coop_client(|client| async move {
        let start = tokio::time::Instant::now();
        let mut next_progress_secs = 10u64;
        // Each RPC wait call blocks for up to 30s server-side.
        let rpc_timeout_secs = 30u64;

        loop {
            let elapsed_secs = start.elapsed().as_secs();
            if elapsed_secs >= timeout_secs {
                return Err(eyre::eyre!(
                    "Timed out waiting for response on {request_id} after {timeout_secs}s"
                ));
            }
            let remaining = timeout_secs - elapsed_secs;
            let this_timeout = rpc_timeout_secs.min(remaining);

            let event = client
                .wait(protocol::WaitRequest {
                    request_id: request_id.to_string(),
                    session_name: session_name.clone(),
                    timeout_secs: this_timeout,
                })
                .await
                .map_err(|e| eyre::eyre!("{e:?}"))?;

            match event {
                protocol::WaitEvent::Update { message } => {
                    eprintln!("{message}");
                }
                protocol::WaitEvent::Response { message } => {
                    eprintln!("{message}");
                    return Ok(());
                }
                protocol::WaitEvent::Timeout => {
                    let elapsed_secs = start.elapsed().as_secs();
                    if elapsed_secs >= next_progress_secs {
                        let status_suffix = if buddy_pane.is_empty() {
                            String::new()
                        } else {
                            let capture = tmux::capture_pane(&buddy_pane).unwrap_or_default();
                            let parsed = pane::parse_pane_content(&capture);
                            if let Some(agent_type) = parsed.agent_type {
                                let agent = match agent_type {
                                    pane::AgentType::Claude => "Claude",
                                    pane::AgentType::Codex => "Codex",
                                };
                                let state = match parsed.state {
                                    pane::AgentState::Working => "Working",
                                    pane::AgentState::Idle => "Idle",
                                    pane::AgentState::Unknown => "Unknown",
                                };
                                let context =
                                    parsed.context_remaining.unwrap_or_else(|| "-".to_string());
                                let mut suffix = format!(" · {agent} · {state} · {context}");
                                if let Some(activity) = parsed.activity {
                                    let activity = activity.replace('\n', " ");
                                    if !activity.trim().is_empty() {
                                        suffix.push_str(&format!(" · {activity}"));
                                    }
                                }
                                suffix
                            } else {
                                " · Unknown".to_string()
                            }
                        };
                        eprintln!("Waiting for response... ({elapsed_secs}s){status_suffix}");
                        next_progress_secs += 10;
                    }
                }
            }
        }
    })
    .await
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
        if !raw_path.extension().is_some_and(|ext| ext == "md") {
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
