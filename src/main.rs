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
use std::time::Duration;

#[derive(Facet, Debug)]
struct Args {
    #[facet(args::subcommand)]
    command: Option<Command>,
}

#[derive(Facet, Debug)]
#[repr(u8)]
enum Command {
    /// Start the bud server in the foreground
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
    /// Capture and show the buddy pane for a request
    Spy {
        /// The request ID to spy on
        #[facet(args::positional)]
        request_id: String,
    },
    /// Steer a buddy on an in-flight request (reads from stdin)
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
    /// Respond to a task (reads from stdin)
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

const MANUAL: &str = r#"bud - cooperative agents over tmux

USAGE:
    bud                              Show this manual
    bud server                       Start the server (usually auto-started)
    bud list                         List pending/in-flight requests
    bud cancel <id>                  Cancel a pending request
    bud show <id>                    Show full task content for a request
    bud spy <id>                     Peek at buddy's pane
    cat <<'EOF' | bud steer <id>     Steer buddy on a pending request
    cat <<'EOF' | bud update <id>    Send progress update to captain
    bud wait <id>                    Wait for a response (default 90s timeout)
    bud wait <id> --timeout <secs>   Wait with custom timeout
    bud issues                       Sync GitHub issues for current repo
    cat <<'EOF' | bud assign                 Assign a task (clears worker context)
    cat <<'EOF' | bud assign --keep          Assign, keeping worker's context
    cat <<'EOF' | bud assign --title "..."   Assign with a title
    cat <<'EOF' | bud assign --issue 42      Assign with GitHub issue context
    cat <<'EOF' | bud respond <id>   Respond to a task (reads stdin)

EXAMPLES:
    # Assign a task:
    cat <<'EOF' | bud assign
    Review the error handling in src/server.rs
    EOF

    # Respond to a task:
    cat <<'EOF' | bud respond abc12345
    I reviewed it, here's what I found...
    EOF

ENVIRONMENT:
    TMUX_PANE    Automatically set by tmux. Used to identify your pane.
    BUD_SOCKET   Override the server socket path (default: /tmp/bud.sock)
"#;

fn socket_path() -> PathBuf {
    std::env::var("BUD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/bud.sock"))
}

fn pid_path() -> PathBuf {
    PathBuf::from("/tmp/bud.pid")
}

fn response_root_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-responses")
}

fn response_dir(session_name: &str) -> PathBuf {
    response_root_dir().join(session_name)
}

fn waiter_marker_path(session_name: &str, request_id: &str) -> PathBuf {
    response_dir(session_name).join(format!("{request_id}.waiter"))
}

fn request_root_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-requests")
}

fn request_dir(session_name: &str) -> PathBuf {
    request_root_dir().join(session_name)
}

fn orphaned_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-orphaned")
}

fn log_path() -> PathBuf {
    PathBuf::from("/tmp/bud-server.log")
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
        Some(Command::Cancel { request_id }) => cancel_request(&request_id),
        Some(Command::Show { request_id }) => show_request(&request_id),
        Some(Command::Spy { request_id }) => spy_request(&request_id),
        Some(Command::Steer { request_id }) => steer_request(&request_id),
        Some(Command::Update { request_id }) => update_request(&request_id),
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
            let request_path = request_dir(&session_name).join(&request_id);
            if !request_path.exists() {
                std::fs::create_dir_all(orphaned_dir())?;
                let orphaned_path = orphaned_dir().join(format!("{request_id}.md"));
                std::fs::write(&orphaned_path, &content)?;
                eprintln!("No matching request found for {request_id} in session {session_name}.");
                eprintln!("Response saved to: {}", orphaned_path.display());
                eprintln!("Ask the user what to do with it.");
                return Ok(());
            }
            // Write the response file directly — no RPC needed
            std::fs::create_dir_all(response_dir(&session_name))?;
            let path = response_dir(&session_name).join(format!("{request_id}.md"));
            std::fs::write(&path, &content)?;
            eprintln!("{}", warmth::responded());
            Ok(())
        }
        Some(Command::Wait {
            request_id,
            timeout,
        }) => {
            let timeout_secs = timeout.unwrap_or(90);
            wait_for_response(&request_id, timeout_secs)
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

    eprintln!("Starting bud server...");
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
        "bud: server failed to start (check {})",
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
            eprintln!("bud: assign failed: {first_error:?}");
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
                eprintln!("bud: assign failed after retry: {e:?}");
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
    eprintln!("  Your buddy is working now. You have nothing to do on this task until they reply.");
    eprintln!("  Their response will arrive through user input automatically.");
    eprintln!("  Use this free time to plan your next move.");
    eprintln!();
    eprintln!(
        "  bud spy {request_id}                         - peek at what your buddy's pane looks like right now"
    );
    eprintln!(
        "  bud list                                 - see all in-flight requests and their status"
    );
    eprintln!(
        "  cat <<'EOF' | bud steer {request_id}         - send a mid-task clarification or course correction"
    );
    eprintln!(
        "  cat <<'EOF' | bud update {request_id}        - (buddy uses this) send a progress update without completing"
    );
    eprintln!("  bud cancel {request_id}                      - cancel the task entirely");
}

async fn assign_once(
    source_pane: &str,
    session_name: &str,
    content: &str,
    clear: bool,
    title: Option<String>,
    binary_hash: &str,
) -> Result<String> {
    use roam_stream::StreamLink;

    let stream = tokio::net::UnixStream::connect(socket_path()).await?;
    let (client, _sh) = roam::initiator(StreamLink::unix(stream))
        .establish::<protocol::CoopClient>(())
        .await?;

    let request_id = client
        .assign(protocol::AssignRequest {
            source_pane: source_pane.to_string(),
            session_name: session_name.to_string(),
            content: content.to_string(),
            title,
            clear,
            binary_hash: binary_hash.to_string(),
        })
        .await
        .map_err(|e| eyre::eyre!("{e:?}"))?;

    Ok(request_id)
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

fn cancel_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    if !path.exists() {
        eprintln!("No task with ID {request_id} found.");
        return Ok(());
    }
    std::fs::remove_dir_all(&path)?;
    eprintln!("Task {request_id} cancelled.");
    Ok(())
}

fn steer_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let message = read_stdin()?;
    let steer = format!("📌 Update from the captain on task {request_id}:\n\n{message}");
    tmux::send_to_pane(&meta.target_pane, &steer)?;
    eprintln!(
        "Sent steer update for task {request_id} to pane {}.",
        meta.target_pane
    );
    print_request_followup_help(request_id);
    Ok(())
}

fn update_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let message = read_stdin()?;
    let session_name = tmux_session_name()?;
    let path = request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let title_suffix = meta
        .title
        .as_deref()
        .map(|title| format!(" ({title})"))
        .unwrap_or_default();
    let git_status = std::process::Command::new("git")
        .args(["status", "--short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let git_section = if git_status.is_empty() {
        String::new()
    } else {
        format!("\n\ngit status:\n```\n{git_status}\n```")
    };
    let update = format!(
        "📋 Progress update from your buddy on task {request_id}{title_suffix}:\n\n{message}\n\nWhether you're happy or unhappy with this update, reply to your buddy (not the user!) with:\n\ncat <<'BUDEOF' | bud steer {request_id}\n<your reply here>\nBUDEOF\n\nThis is also a good time to commit and push your buddy's work so far.{git_section}"
    );
    let waiter_marker = waiter_marker_path(&session_name, request_id);
    if waiter_marker.exists() {
        std::fs::create_dir_all(response_dir(&session_name))?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let update_path =
            response_dir(&session_name).join(format!("{request_id}.update.{timestamp}.md"));
        std::fs::write(&update_path, &update)?;
        eprintln!(
            "Queued progress update for task {request_id} at {}.",
            update_path.display()
        );
    } else {
        tmux::send_to_pane(&meta.source_pane, &update)?;
        eprintln!(
            "Sent progress update for task {request_id} to pane {}.",
            meta.source_pane
        );
    }
    Ok(())
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

fn list_requests() -> Result<()> {
    use std::time::SystemTime;

    struct Row {
        session: String,
        id: String,
        source: String,
        target: String,
        title: Option<String>,
        age: String,
        response: &'static str,
    }

    let mut rows: Vec<Row> = Vec::new();
    let now = SystemTime::now();
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
                let response_exists = if session_response_dir.join(format!("{id}.md")).exists() {
                    "yes"
                } else {
                    "no"
                };
                rows.push(Row {
                    session: session_name.clone(),
                    id,
                    source: source_pane,
                    target: target_pane,
                    title,
                    age,
                    response: response_exists,
                });
            }
        }
    }

    if rows.is_empty() {
        eprintln!("No tasks in flight — all clear!");
    } else {
        rows.sort_by(|a, b| a.session.cmp(&b.session).then(a.id.cmp(&b.id)));
        let show_title = rows.iter().any(|r| r.title.is_some());
        if show_title {
            eprintln!(
                "SESSION              REQUEST     SOURCE        TARGET       TITLE                      AGE         RESPONSE"
            );
            eprintln!(
                "-------------------  ----------  ------------  -----------  -------------------------  ----------  --------"
            );
            for r in &rows {
                eprintln!(
                    "{:<19}  {:<10}  {:<12}  {:<11}  {:<25}  {:<10}  {}",
                    r.session,
                    r.id,
                    r.source,
                    r.target,
                    r.title.as_deref().unwrap_or("-"),
                    r.age,
                    r.response
                );
            }
        } else {
            eprintln!(
                "SESSION              REQUEST     SOURCE        TARGET       AGE         RESPONSE"
            );
            eprintln!(
                "-------------------  ----------  ------------  -----------  ----------  --------"
            );
            for r in &rows {
                eprintln!(
                    "{:<19}  {:<10}  {:<12}  {:<11}  {:<10}  {}",
                    r.session, r.id, r.source, r.target, r.age, r.response
                );
            }
        }
    }

    eprintln!();
    match tmux::list_all_panes() {
        Ok(panes) => {
            let mut agent_rows: Vec<(String, String, String, String, String, String)> = Vec::new();
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
                let context = parsed.context_remaining.unwrap_or_else(|| "-".to_string());
                let activity = parsed
                    .activity
                    .map(|value| value.replace('\n', " "))
                    .unwrap_or_else(|| "-".to_string());
                agent_rows.push((
                    p.session_name.clone(),
                    p.id.clone(),
                    agent.to_string(),
                    state.to_string(),
                    context,
                    activity,
                ));
            }

            if agent_rows.is_empty() {
                eprintln!("No agent panes detected.");
            } else {
                eprintln!(
                    "SESSION              PANE       AGENT    STATE    CONTEXT            ACTIVITY"
                );
                eprintln!(
                    "-------------------  ---------  -------  -------  -----------------  ----------------------------------------"
                );
                for (session, pane_id, agent, state, context, activity) in agent_rows {
                    eprintln!(
                        "{:<19}  {:<9}  {:<7}  {:<7}  {:<17}  {}",
                        session, pane_id, agent, state, context, activity
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Panes unavailable: {e}");
        }
    }

    Ok(())
}

fn sync_issues_to_pane() -> Result<()> {
    let repo = github::infer_repo()?;
    eprintln!("Syncing issues for {repo}...");

    let (created, failed) = process_pending_issue_drafts(&repo)?;

    let issues = github::sync_issues(&repo)?;
    let result = github::write_issue_files(&repo, &issues)?;

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
        "Issues synced for {repo} — {} open, {} closed.\n\n  Browse all:       ls {}/\n  Browse open:      ls {}/\n  Browse closed:    ls {}/\n  By created date:  ls {}/\n  By updated date:  ls {}/",
        result.open_count,
        result.closed_count,
        result.all_dir.display(),
        result.open_dir.display(),
        result.closed_dir.display(),
        result.by_created_dir.display(),
        result.by_updated_dir.display(),
    ));
    if let Some(labels_dir) = result.labels_dir.as_ref() {
        summary.push_str(&format!(
            "\n  Browse by label:  ls {}/",
            labels_dir.display()
        ));
    }
    if let Some(milestones_dir) = result.milestones_dir.as_ref() {
        summary.push_str(&format!(
            "\n  Browse by milestone: ls {}/",
            milestones_dir.display()
        ));
    }
    if let Some(deps_dir) = result.deps_path.as_ref() {
        summary.push_str(&format!("\n  Browse deps:      ls {}/", deps_dir.display()));
    }
    summary.push_str(&format!(
        "\n  Read the index:   cat {}\n  Read deps:        cat {}\n  Read labels:      cat {}\n  Read milestones:  cat {}\n  Read an issue:    cat {}/all/<filename>.md\n  Create an issue:  Write to {}/<name>.md then run: bud issues\n\nPick an issue to work on, then assign it to your buddy with: bud assign",
        result.index_path.display(),
        result.deps_markdown_path.display(),
        result.labels_markdown_path.display(),
        result.milestones_markdown_path.display(),
        result.base_dir.display()
        ,
        result.new_dir.display()
    ));
    println!("{summary}");
    Ok(())
}

fn wait_for_response(request_id: &str, timeout_secs: u64) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = tmux_session_name()?;
    let request_path = request_dir(&session_name).join(request_id);
    let request_meta = request_path.join("meta");
    if !request_meta.is_file() {
        return Err(eyre::eyre!(
            "No matching request found for {request_id} in session {session_name}."
        ));
    }

    let response_path = response_dir(&session_name).join(format!("{request_id}.md"));
    let final_path = response_dir(&session_name).join(format!("{request_id}.final.md"));
    let update_prefix = format!("{request_id}.update.");
    let poll_interval = Duration::from_secs(2);
    let mut waited = 0u64;
    let mut next_progress = 10u64;
    let buddy_pane = util::read_request_meta(&request_path)
        .map(|meta| meta.target_pane)
        .unwrap_or_default();
    let mut seen_updates: std::collections::HashSet<String> = std::collections::HashSet::new();
    std::fs::create_dir_all(response_dir(&session_name))?;
    let waiter_marker = waiter_marker_path(&session_name, request_id);
    std::fs::write(&waiter_marker, std::process::id().to_string())?;

    struct WaiterMarkerGuard {
        path: std::path::PathBuf,
    }
    impl Drop for WaiterMarkerGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    let _waiter_guard = WaiterMarkerGuard {
        path: waiter_marker.clone(),
    };

    if final_path.exists() {
        let response = std::fs::read_to_string(&final_path)?;
        eprintln!("{response}");
        let _ = std::fs::remove_file(&final_path);
        return Ok(());
    }
    if !request_path.exists() {
        eprintln!("Response for {request_id} has been delivered.");
        return Ok(());
    }

    eprintln!("Waiting for response on {request_id} for up to {timeout_secs}s...");

    while waited < timeout_secs {
        std::thread::sleep(poll_interval);
        waited += 2;
        let _ = std::fs::write(&waiter_marker, waited.to_string());

        if let Ok(entries) = std::fs::read_dir(response_dir(&session_name)) {
            let mut updates: Vec<(String, std::path::PathBuf)> = entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !name.starts_with(&update_prefix) || !name.ends_with(".md") {
                        return None;
                    }
                    Some((name, entry.path()))
                })
                .collect();
            updates.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, update_file) in updates {
                if seen_updates.contains(&name) {
                    continue;
                }
                match std::fs::read_to_string(&update_file) {
                    Ok(update) => {
                        eprintln!("{update}");
                        seen_updates.insert(name);
                        let _ = std::fs::remove_file(&update_file);
                    }
                    Err(_) => {
                        seen_updates.insert(name);
                    }
                }
            }
        }

        if final_path.exists() {
            let response = std::fs::read_to_string(&final_path)?;
            eprintln!("{response}");
            let _ = std::fs::remove_file(&final_path);
            return Ok(());
        }
        if !request_path.exists() {
            if final_path.exists() {
                let response = std::fs::read_to_string(&final_path)?;
                eprintln!("{response}");
                let _ = std::fs::remove_file(&final_path);
                return Ok(());
            }
            if response_path.exists() {
                let response = std::fs::read_to_string(&response_path)?;
                eprintln!("{response}");
                return Ok(());
            }
            eprintln!("Response for {request_id} has been delivered.");
            return Ok(());
        }

        if waited >= next_progress {
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
                    let context = parsed.context_remaining.unwrap_or_else(|| "-".to_string());
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
            eprintln!("Waiting for response... ({waited}s){status_suffix}");
            next_progress += 10;
        }
    }

    Err(eyre::eyre!(
        "Timed out waiting for response on {request_id} after {timeout_secs}s"
    ))
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

fn process_pending_issue_drafts(
    repo: &str,
) -> Result<(Vec<PendingIssueCreated>, Vec<PendingIssueFailed>)> {
    use std::io::ErrorKind;

    let base_dir = github::issue_repo_dir(repo);
    let new_dir = base_dir.join("new");
    if !new_dir.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }

    let failed_dir = base_dir.join("failed");
    fs_err::create_dir_all(&failed_dir)?;

    let mut entries: Vec<fs_err::DirEntry> = fs_err::read_dir(&new_dir)?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_file()))
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .filter(|entry| entry.file_name().to_string_lossy() != "TEMPLATE.md")
        .collect();
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_string());

    if entries.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut existing_labels = github::sync_labels_set(repo)?;
    let mut existing_milestones = github::sync_milestones_set(repo)?;
    let mut created = Vec::new();
    let mut failed = Vec::new();

    for entry in entries {
        let path = entry.path();
        let original_name = entry.file_name().to_string_lossy().to_string();
        let content = match fs_err::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                if e.kind() == ErrorKind::NotFound {
                    eprintln!(
                        "Skipping draft {}: file disappeared before read (likely concurrent `bud issues` run).",
                        path.display()
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

        let draft = match github::parse_new_issue(&content) {
            Ok(issue) => issue,
            Err(e) => {
                if let Err(move_err) = move_file(&path, &failed_dir.join(&original_name)) {
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
                if let Err(e) = fs_err::remove_file(&path) {
                    if e.kind() == ErrorKind::NotFound {
                        eprintln!(
                            "Draft {} already removed after create (likely concurrent run).",
                            path.display()
                        );
                    } else {
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
                created.push(PendingIssueCreated {
                    number,
                    url,
                    title: draft.title,
                });
            }
            Err(e) => {
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

    if to.exists() {
        fs_err::remove_file(to)?;
    }
    if let Err(e) = fs_err::rename(from, to) {
        if e.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        return Err(e.into());
    }
    Ok(())
}
