use crate::protocol::{CoopClient, CoopDispatcher};
use crate::tmux;
use eyre::Result;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use roam_stream::StreamLink;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{error, info, warn};

const STALENESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
const STALENESS_NOTIFY_AFTER_UNCHANGED: u32 = 4;
const IDLE_NOTIFY_DELAY: Duration = Duration::from_secs(30);

struct Request {
    session_name: String,
    source_pane: String,
    target_pane: String,
    title: Option<String>,
}

struct PaneState {
    last_content: String,
    unchanged_count: u32,
    notified: bool,
}

struct IdleState {
    empty_since: Option<Instant>,
    notified: bool,
    last_title: Option<String>,
    source_pane: Option<String>,
}

#[derive(Clone)]
struct CoopServer {
    requests: Arc<Mutex<HashMap<String, Request>>>,
    request_root_dir: PathBuf,
    idle_states: Arc<Mutex<HashMap<String, IdleState>>>,
}

fn request_key(session_name: &str, request_id: &str) -> String {
    format!("{session_name}/{request_id}")
}

fn orphaned_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-orphaned")
}

impl crate::protocol::Coop for CoopServer {
    async fn assign(&self, req: crate::protocol::AssignRequest) -> Result<String, String> {
        if req.binary_hash != crate::hash::binary_hash() {
            info!("binary changed, shutting down for upgrade");
            std::process::exit(0);
        }

        let crate::protocol::AssignRequest {
            source_pane,
            session_name,
            content,
            title,
            clear,
            binary_hash: _,
        } = req;
        let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let request_key = request_key(&session_name, &request_id);
        let title_for_file = title.clone();

        let target = match tmux::find_other_pane(&source_pane) {
            Ok(p) => p,
            Err(e) => {
                error!("failed to find worker pane: {e}");
                return Err(e.to_string());
            }
        };

        let task_content = if let Some(title) = title_for_file.as_deref() {
            format!("## {title}\n\n{content}")
        } else {
            content
        };

        self.requests.lock().await.insert(
            request_key.clone(),
            Request {
                session_name: session_name.clone(),
                source_pane: source_pane.clone(),
                target_pane: target.id.clone(),
                title,
            },
        );
        {
            let mut idle_states = self.idle_states.lock().await;
            let state = idle_states.entry(session_name.clone()).or_insert(IdleState {
                empty_since: None,
                notified: false,
                last_title: None,
                source_pane: None,
            });
            state.empty_since = None;
            state.notified = false;
            state.last_title = None;
            state.source_pane = None;
        }

        let request_path = self.request_root_dir.join(&session_name).join(&request_id);
        if let Err(e) = crate::util::write_request(
            &request_path,
            &source_pane,
            &target.id,
            title_for_file.as_deref(),
            &task_content,
        ) {
            self.requests.lock().await.remove(&request_key);
            let _ = std::fs::remove_dir_all(&request_path);
            return Err(format!(
                "failed to persist request {} to {}: {e}",
                request_id,
                request_path.display()
            ));
        }

        if clear {
            if let Err(e) = tmux::send_to_pane(&target.id, "/clear") {
                error!("failed to send /clear to pane {}: {e}", target.id);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let message = format!(
            "{}\n\n\
             {task_content}\n\n\
             If you hit a decision point, want to share progress, or need clarification, send an update:\n\n\
             cat <<'BUDEOF' | bud update {request_id}\n\
             <your progress update here>\n\
             BUDEOF\n\n\
             IMPORTANT: When you're done, you MUST send your response by executing \
             this shell command (use your Bash/shell tool — do NOT just print it as text):\n\n\
             cat <<'BUDEOF' | bud respond {request_id}\n\
             <put your full response here>\n\
             BUDEOF",
            crate::warmth::greeting(),
        );

        if let Err(e) = tmux::send_to_pane(&target.id, &message) {
            error!("failed to send to pane {}: {e}", target.id);
            self.requests.lock().await.remove(&request_key);
            if let Err(remove_err) = std::fs::remove_dir_all(&request_path)
                && remove_err.kind() != std::io::ErrorKind::NotFound
            {
                error!(
                    "failed to remove request directory {}: {remove_err}",
                    request_path.display()
                );
            }
            return Err(format!("failed to send to pane {}: {e}", target.id));
        }

        info!(
            "assigned request {request_id} -> pane {} (session {session_name})",
            target.id
        );
        Ok(request_id)
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_root_dir: PathBuf,
    request_root_dir: PathBuf,
    log_path: PathBuf,
) -> Result<()> {
    let log_file = std::fs::File::create(&log_path)?;
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bud=info".parse()?),
        )
        .init();

    std::fs::create_dir_all(&response_root_dir)?;
    std::fs::create_dir_all(&request_root_dir)?;

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    std::fs::write(&pid_path, std::process::id().to_string())?;

    info!("bud server starting on {}", socket_path.display());
    info!("watching for responses in {}", response_root_dir.display());

    let listener = UnixListener::bind(&socket_path)?;
    let requests: Arc<Mutex<HashMap<String, Request>>> = Arc::new(Mutex::new(HashMap::new()));
    let idle_states: Arc<Mutex<HashMap<String, IdleState>>> = Arc::new(Mutex::new(HashMap::new()));

    let watch_requests = requests.clone();
    let watch_idle_states = idle_states.clone();
    let watch_response_root = response_root_dir.clone();
    let watch_request_root = request_root_dir.clone();
    tokio::spawn(async move {
        watch_responses(
            watch_response_root,
            watch_request_root,
            watch_requests,
            watch_idle_states,
        )
        .await;
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let reqs = requests.clone();
        let server_request_root = request_root_dir.clone();
        let server_idle_states = idle_states.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
                request_root_dir: server_request_root,
                idle_states: server_idle_states,
            };
            let result = roam::acceptor(StreamLink::unix(stream))
                .establish::<CoopClient>(CoopDispatcher::new(server))
                .await;
            match result {
                Ok((_caller, sh)) => {
                    tokio::time::sleep(Duration::from_secs(300)).await;
                    drop(_caller);
                    drop(sh);
                }
                Err(e) => error!("connection failed: {e}"),
            }
        });
    }
}

async fn watch_responses(
    response_root_dir: PathBuf,
    request_root_dir: PathBuf,
    requests: Arc<Mutex<HashMap<String, Request>>>,
    idle_states: Arc<Mutex<HashMap<String, IdleState>>>,
) {
    let mut pane_states: HashMap<String, PaneState> = HashMap::new();
    let discord_webhook_url = crate::config::discord_webhook_url();
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut watcher: Option<RecommendedWatcher> = match RecommendedWatcher::new(
        move |result| {
            let _ = event_tx.send(result);
        },
        Config::default(),
    ) {
        Ok(mut watcher) => {
            if let Err(e) = watcher.watch(&response_root_dir, RecursiveMode::Recursive) {
                error!(
                    "failed to start response watcher on {}: {e}; falling back to polling",
                    response_root_dir.display()
                );
                None
            } else {
                Some(watcher)
            }
        }
        Err(e) => {
            error!(
                "failed to initialize response watcher for {}: {e}; falling back to polling",
                response_root_dir.display()
            );
            None
        }
    };

    process_response_files(
        &response_root_dir,
        &request_root_dir,
        &requests,
        &idle_states,
        &mut pane_states,
    )
    .await;

    let mut staleness_tick = tokio::time::interval(STALENESS_SAMPLE_INTERVAL);
    let mut poll_tick = tokio::time::interval(Duration::from_secs(2));

    loop {
        if watcher.is_some() {
            tokio::select! {
                _ = staleness_tick.tick() => {
                    run_staleness_checks(&request_root_dir, &response_root_dir, &mut pane_states);
                    maybe_notify_idle(&idle_states, discord_webhook_url.as_deref()).await;
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(Ok(event)) if matches!(event.kind, EventKind::Create(_)) => {
                            process_response_files(
                                &response_root_dir,
                                &request_root_dir,
                                &requests,
                                &idle_states,
                                &mut pane_states,
                            )
                            .await;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => {
                            error!("response watcher event error: {e}");
                        }
                        None => {
                            error!("response watcher channel closed; falling back to polling");
                            watcher = None;
                        }
                    }
                }
            }
        } else {
            tokio::select! {
                _ = staleness_tick.tick() => {
                    run_staleness_checks(&request_root_dir, &response_root_dir, &mut pane_states);
                    maybe_notify_idle(&idle_states, discord_webhook_url.as_deref()).await;
                }
                _ = poll_tick.tick() => {
                    process_response_files(
                        &response_root_dir,
                        &request_root_dir,
                        &requests,
                        &idle_states,
                        &mut pane_states,
                    )
                    .await;
                }
            }
        }
    }
}

async fn maybe_notify_idle(
    idle_states: &Arc<Mutex<HashMap<String, IdleState>>>,
    webhook_url: Option<&str>,
) {
    let Some(webhook_url) = webhook_url else {
        return;
    };

    struct PendingNotify {
        session_name: String,
        empty_since: Instant,
        last_title: Option<String>,
        source_pane: Option<String>,
    }

    let sessions_to_notify: Vec<PendingNotify> = {
        let mut states = idle_states.lock().await;
        let mut pending = Vec::new();
        for (session_name, state) in states.iter_mut() {
            let Some(empty_since) = state.empty_since else {
                continue;
            };
            if state.notified || empty_since.elapsed() < IDLE_NOTIFY_DELAY {
                continue;
            }
            // Mark before releasing lock so we can't double-fire on the next tick.
            state.notified = true;
            pending.push(PendingNotify {
                session_name: session_name.clone(),
                empty_since,
                last_title: state.last_title.clone(),
                source_pane: state.source_pane.clone(),
            });
        }
        pending
    };

    for pending in sessions_to_notify {
        let mut message = format!(
            "Your captain in session **{}** has no more tasks — time to check in!",
            pending.session_name
        );
        if let Some(last_title) = pending.last_title.as_deref() {
            message.push_str(&format!("\nLast completed: **{last_title}**"));
        }
        if let Some(source_pane) = pending.source_pane.as_deref() {
            match tmux::capture_pane(source_pane) {
                Ok(pane_capture) => {
                    message.push_str(&format!("\n||{pane_capture}||"));
                }
                Err(e) => {
                    warn!(
                        "failed to capture source pane {} for idle notification in session {}: {e}",
                        source_pane, pending.session_name
                    );
                }
            }
        }
        if let Err(e) = crate::discord::notify(webhook_url, &message).await {
            error!(
                "failed to send Discord idle notification for session {}: {e}",
                pending.session_name
            );
            let mut states = idle_states.lock().await;
            if let Some(state) = states.get_mut(&pending.session_name)
                && state.empty_since == Some(pending.empty_since)
            {
                state.notified = false;
            }
            continue;
        }
    }
}

fn run_staleness_checks(
    request_root_dir: &Path,
    response_root_dir: &Path,
    pane_states: &mut HashMap<String, PaneState>,
) {
    let mut active_request_keys: HashSet<String> = HashSet::new();

    let session_entries = match std::fs::read_dir(request_root_dir) {
        Ok(entries) => entries,
        Err(_) => {
            pane_states.clear();
            return;
        }
    };

    for session_entry in session_entries.flatten() {
        let session_path = session_entry.path();
        let session_name = match session_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        match session_entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {}
            _ => continue,
        }

        let request_entries = match std::fs::read_dir(&session_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for request_entry in request_entries.flatten() {
            let request_path = request_entry.path();
            match request_entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {}
                _ => continue,
            }

            let request_id = match request_entry.file_name().to_str() {
                Some(id) => id.to_string(),
                None => continue,
            };
            let key = request_key(&session_name, &request_id);
            active_request_keys.insert(key.clone());

            let response_path = response_root_dir
                .join(&session_name)
                .join(format!("{request_id}.md"));
            if response_path.exists() {
                continue;
            }

            let meta = match crate::util::read_request_meta(&request_path) {
                Some(meta) => meta,
                None => continue,
            };
            let pane_content = match tmux::capture_pane(&meta.target_pane) {
                Ok(content) => content,
                Err(e) => {
                    error!(
                        "failed to capture pane {} for request {}: {e}",
                        meta.target_pane, request_id
                    );
                    continue;
                }
            };

            let state = pane_states.entry(key).or_insert_with(|| PaneState {
                last_content: pane_content.clone(),
                unchanged_count: 0,
                notified: false,
            });
            if pane_content == state.last_content {
                state.unchanged_count += 1;
            } else {
                state.last_content = pane_content.clone();
                state.unchanged_count = 0;
                state.notified = false;
            }

            if state.notified || state.unchanged_count < STALENESS_NOTIFY_AFTER_UNCHANGED {
                continue;
            }

            let title_suffix = meta
                .title
                .as_deref()
                .map(|title| format!(" ({title})"))
                .unwrap_or_default();
            let message = format!(
                "⏰ Hey captain — your buddy seems stuck on task {request_id}{title_suffix}. Their pane has been unchanged for 2 minutes.\n\nPane content:\n```\n{pane_content}\n```"
            );
            if let Err(e) = tmux::send_to_pane(&meta.source_pane, &message) {
                error!(
                    "failed to deliver staleness notification for request {} to pane {}: {e}",
                    request_id, meta.source_pane
                );
                continue;
            }

            state.notified = true;
        }
    }

    pane_states.retain(|key, _| active_request_keys.contains(key));
}

async fn process_response_files(
    response_root_dir: &Path,
    request_root_dir: &Path,
    requests: &Arc<Mutex<HashMap<String, Request>>>,
    idle_states: &Arc<Mutex<HashMap<String, IdleState>>>,
    pane_states: &mut HashMap<String, PaneState>,
) {
    let session_entries = match std::fs::read_dir(response_root_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for session_entry in session_entries.flatten() {
        let session_path = session_entry.path();
        let session_name = match session_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        match session_entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {}
            _ => continue,
        }

        let response_entries = match std::fs::read_dir(&session_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for response_entry in response_entries.flatten() {
            let response_path = response_entry.path();
            match response_entry.file_type() {
                Ok(file_type) if file_type.is_file() => {}
                _ => continue,
            }

            let request_id = match response_path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let key = request_key(&session_name, &request_id);

            let in_memory_request = {
                let mut reqs = requests.lock().await;
                reqs.remove(&key)
            };

            let request_path = request_root_dir.join(&session_name).join(&request_id);
            let (source_pane, target_pane, title) = if let Some(request) = in_memory_request {
                (request.source_pane, request.target_pane, request.title)
            } else {
                match crate::util::read_request_meta(&request_path) {
                    Some(meta) => (meta.source_pane, meta.target_pane, meta.title),
                    None => {
                        if let Err(e) = std::fs::create_dir_all(orphaned_dir()) {
                            error!(
                                "failed to create orphaned response directory {}: {e}",
                                orphaned_dir().display()
                            );
                            continue;
                        }
                        let orphaned_path =
                            orphaned_dir().join(format!("{session_name}-{request_id}.md"));
                        let move_result = std::fs::rename(&response_path, &orphaned_path).or_else(
                            |_| {
                                std::fs::copy(&response_path, &orphaned_path)?;
                                std::fs::remove_file(&response_path)
                            },
                        );
                        match move_result {
                            Ok(()) => {
                                warn!(
                                    "orphaned response for request {} in session {}, saved to {}",
                                    request_id,
                                    session_name,
                                    orphaned_path.display()
                                );
                            }
                            Err(e) => {
                                error!(
                                    "failed to persist orphaned response {} for session {} to {}: {e}",
                                    request_id,
                                    session_name,
                                    orphaned_path.display()
                                );
                            }
                        }
                        continue;
                    }
                }
            };

            let body = match std::fs::read_to_string(&response_path) {
                Ok(content) => content,
                Err(_) => "(could not read response file)".to_string(),
            };
            let intro = if let Some(title) = title.as_deref() {
                format!("Fresh from your buddy — re: {title}")
            } else {
                crate::warmth::delivered().to_string()
            };
            let message = format!(
                "{intro}\n{body}\n\nRemember: you're the captain. If there's follow-up work, assign it to your buddy — don't do it yourself. Stay focused on the big picture!"
            );
            if let Err(e) = tmux::send_to_pane(&source_pane, &message) {
                error!("failed to deliver response to pane {}: {e}", source_pane);
                continue;
            }

            if let Err(e) = std::fs::remove_dir_all(&request_path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!("failed to remove request directory {}: {e}", request_path.display());
            }
            pane_states.remove(&key);
            if let Err(e) = std::fs::remove_file(&response_path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!(
                    "failed to remove response file {}: {e}",
                    response_path.display()
                );
            }

            info!(
                "delivered response for request {request_id} in session {session_name} (target pane {target_pane})"
            );

            let session_empty = {
                let reqs = requests.lock().await;
                !reqs.values().any(|req| req.session_name == session_name)
            };
            if session_empty {
                let mut states = idle_states.lock().await;
                let state = states.entry(session_name.clone()).or_insert(IdleState {
                    empty_since: None,
                    notified: false,
                    last_title: None,
                    source_pane: None,
                });
                state.empty_since = Some(Instant::now());
                state.notified = false;
                state.last_title = title.clone();
                state.source_pane = Some(source_pane.clone());
            }
        }
    }
}
