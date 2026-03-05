use eyre::Result;

pub(crate) async fn ensure_server_running() -> Result<()> {
    async fn socket_accepts_connections(socket: &std::path::Path) -> bool {
        tokio::net::UnixStream::connect(socket).await.is_ok()
    }

    fn pid_is_alive(pid: u32) -> bool {
        use sysinfo::{Pid, ProcessesToUpdate, System};

        let mut system = System::new();
        let pid = Pid::from_u32(pid);
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        system.process(pid).is_some()
    }

    let socket = crate::paths::socket_path();
    if socket_accepts_connections(&socket).await {
        return Ok(());
    }

    let pid_file = crate::paths::pid_path();
    if let Ok(pid_str) = fs_err::tokio::read_to_string(&pid_file).await
        && let Ok(pid) = pid_str.trim().parse::<u32>()
        && pid_is_alive(pid)
    {
        // Server may be in the middle of startup; wait briefly for the socket to accept.
        for _ in 0..10 {
            if socket_accepts_connections(&socket).await {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Server not running — clean up stale socket if any
    if fs_err::tokio::metadata(&socket).await.is_ok() {
        let _ = fs_err::tokio::remove_file(&socket).await;
    }

    eprintln!("Starting mate server...");
    let exe = std::env::current_exe()?;
    let log_file = fs_err::tokio::File::create(crate::paths::log_path())
        .await?
        .into_std()
        .await
        .into_file();
    tokio::process::Command::new(exe)
        .arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    for _ in 0..50 {
        if socket_accepts_connections(&socket).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(eyre::eyre!(
        "mate: server failed to start (check {})",
        crate::paths::log_path().display()
    ))
}

pub(crate) async fn client_assign(
    source_pane: String,
    session_name: String,
    content: String,
    clear: bool,
    title: Option<String>,
    issue: Option<u64>,
) -> Result<()> {
    let binary_hash = crate::hash::binary_hash().await;
    let content = if let Some(issue_number) = issue {
        let repo = crate::github::infer_repo().await?;
        let issue_content = crate::github::read_issue_file(&repo, issue_number).await?;
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
            eprintln!("{}", crate::warmth::assigned());
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
            eprintln!("{}", crate::warmth::assigned());
            eprintln!("Request ID: {request_id}");
            print_request_followup_help(&request_id);
            Ok(())
        }
    }
}

pub(crate) fn print_request_followup_help(request_id: &str) {
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

pub(crate) async fn with_coop_client<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce(crate::protocol::CoopClient) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    use roam_stream::StreamLink;
    let stream = tokio::net::UnixStream::connect(crate::paths::socket_path())
        .await
        .map_err(|e| eyre::eyre!("failed to connect to mate server: {e}"))?;
    let (client, _sh) = roam::initiator(StreamLink::unix(stream))
        .establish::<crate::protocol::CoopClient>(())
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
            .assign(crate::protocol::AssignRequest {
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

pub(crate) fn validate_request_id(request_id: &str) -> Result<()> {
    if request_id.len() != 8
        || !request_id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(eyre::eyre!("invalid request ID (expected 8 hex chars)"));
    }
    Ok(())
}

pub(crate) async fn cancel_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = crate::paths::tmux_session_name().await?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .cancel(crate::protocol::CancelRequest {
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

pub(crate) async fn steer_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let message = crate::paths::read_stdin().await?;
    let session_name = crate::paths::tmux_session_name().await?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .steer(crate::protocol::SteerRequest {
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

pub(crate) async fn accept_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = crate::paths::tmux_session_name().await?;
    ensure_server_running().await?;

    with_coop_client(|client| async move {
        client
            .accept(crate::protocol::AcceptRequest {
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

pub(crate) async fn rpc_respond(request_id: &str, session_name: &str, content: &str) -> Result<()> {
    with_coop_client(|client| async move {
        client
            .respond(crate::protocol::RespondRequest {
                request_id: request_id.to_string(),
                session_name: session_name.to_string(),
                content: content.to_string(),
            })
            .await
            .map_err(|e| eyre::eyre!("{e:?}"))
    })
    .await?;
    eprintln!("{}", crate::warmth::responded());
    Ok(())
}

pub(crate) async fn update_request(request_id: &str) -> Result<()> {
    validate_request_id(request_id)?;
    let content = crate::paths::read_stdin().await?;
    let session_name = crate::paths::tmux_session_name().await?;
    ensure_server_running().await?;
    with_coop_client(|client| async move {
        client
            .update(crate::protocol::UpdateRequest {
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

pub(crate) async fn wait_for_response(request_id: &str, timeout_secs: u64) -> Result<()> {
    validate_request_id(request_id)?;
    let session_name = crate::paths::tmux_session_name().await?;
    let request_path = crate::paths::request_dir(&session_name).join(request_id);
    if !request_path.join("meta").is_file() {
        return Err(eyre::eyre!(
            "No matching request found for {request_id} in session {session_name}."
        ));
    }

    let buddy_pane = crate::util::read_request_meta(&request_path)
        .await
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
                .wait(crate::protocol::WaitRequest {
                    request_id: request_id.to_string(),
                    session_name: session_name.clone(),
                    timeout_secs: this_timeout,
                })
                .await
                .map_err(|e| eyre::eyre!("{e:?}"))?;

            match event {
                crate::protocol::WaitEvent::Update { message } => {
                    eprintln!("{message}");
                }
                crate::protocol::WaitEvent::Response { message } => {
                    eprintln!("{message}");
                    return Ok(());
                }
                crate::protocol::WaitEvent::Timeout => {
                    let elapsed_secs = start.elapsed().as_secs();
                    if elapsed_secs >= next_progress_secs {
                        let status_suffix = if buddy_pane.is_empty() {
                            String::new()
                        } else {
                            let capture = crate::tmux::capture_pane(&buddy_pane)
                                .await
                                .unwrap_or_default();
                            let parsed = crate::pane::parse_pane_content(&capture);
                            if let Some(agent_type) = parsed.agent_type {
                                let agent = match agent_type {
                                    crate::pane::AgentType::Claude => "Claude",
                                    crate::pane::AgentType::Codex => "Codex",
                                };
                                let state = match parsed.state {
                                    crate::pane::AgentState::Working => "Working",
                                    crate::pane::AgentState::Idle => "Idle",
                                    crate::pane::AgentState::Unknown => "Unknown",
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
