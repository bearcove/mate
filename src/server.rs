use crate::pane;
use crate::protocol::{CoopClient, CoopDispatcher};
use crate::tmux;
use eyre::Result;
use fs_err::tokio as fs;
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
const IDLE_NUDGE_AFTER: Duration = Duration::from_secs(60);
const IDLE_NOTIFY_DELAY: Duration = Duration::from_secs(30);

struct Request {
    session_name: String,
    source_pane: String,
    target_pane: String,
    title: Option<String>,
}

/// Registered when `mate wait` is blocking server-side for a request.
struct Waiter {
    /// The event to deliver once available (set by respond/update handlers).
    event: Option<crate::protocol::WaitEvent>,
    notify: Arc<tokio::sync::Notify>,
}

struct PaneState {
    last_content: String,
    unchanged_count: u32,
    captain_last_content: String,
    captain_unchanged_count: u32,
    notified: bool,
    idle_since: Option<Instant>,
    idle_nudged: bool,
}

struct IdleState {
    empty_since: Option<Instant>,
    notified: bool,
    last_title: Option<String>,
    source_pane: Option<String>,
    last_pane_content: Option<String>,
    pane_unchanged_since: Option<Instant>,
    parsed_pane_state: Option<pane::PaneState>,
}

#[derive(Clone)]
struct CoopServer {
    requests: Arc<Mutex<HashMap<String, Request>>>,
    request_root_dir: PathBuf,
    idle_states: Arc<Mutex<HashMap<String, IdleState>>>,
    /// Active waiters registered by `mate wait` RPC calls. Keyed by request_key.
    waiters: Arc<Mutex<HashMap<String, Waiter>>>,
}

fn request_key(session_name: &str, request_id: &str) -> String {
    format!("{session_name}/{request_id}")
}

fn orphaned_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-orphaned")
}

async fn restore_requests_from_disk(
    request_root_dir: &Path,
    response_root_dir: &Path,
    requests: &Arc<Mutex<HashMap<String, Request>>>,
) {
    let mut session_entries = match fs::read_dir(request_root_dir).await {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "failed to scan request root {} on startup: {e}",
                request_root_dir.display()
            );
            return;
        }
    };

    let mut restored: Vec<(String, Request)> = Vec::new();
    let mut skipped_with_response = 0usize;
    let mut skipped_invalid = 0usize;

    while let Ok(Some(session_entry)) = session_entries.next_entry().await {
        let Ok(file_type) = session_entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let session_name = match session_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let session_path = session_entry.path();
        let mut request_entries = match fs::read_dir(&session_path).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        while let Ok(Some(request_entry)) = request_entries.next_entry().await {
            let Ok(file_type) = request_entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let request_id = match request_entry.file_name().to_str() {
                Some(id) => id.to_string(),
                None => continue,
            };
            let response_path = response_root_dir
                .join(&session_name)
                .join(format!("{request_id}.md"));
            if fs::metadata(&response_path).await.is_ok() {
                skipped_with_response += 1;
                continue;
            }

            let request_path = request_entry.path();
            let meta = match crate::util::read_request_meta(&request_path).await {
                Some(meta) => meta,
                None => {
                    skipped_invalid += 1;
                    continue;
                }
            };

            let key = request_key(&session_name, &request_id);
            restored.push((
                key,
                Request {
                    session_name: session_name.clone(),
                    source_pane: meta.source_pane,
                    target_pane: meta.target_pane,
                    title: meta.title,
                },
            ));
        }
    }

    let restored_count = restored.len();
    {
        let mut reqs = requests.lock().await;
        for (key, request) in restored {
            reqs.insert(key, request);
        }
    }

    info!(
        "startup request restore: restored={}, skipped_with_response={}, skipped_invalid={}",
        restored_count, skipped_with_response, skipped_invalid
    );
}

async fn session_has_request_dirs(request_root_dir: &Path, session_name: &str) -> bool {
    let session_path = request_root_dir.join(session_name);
    let mut entries = match fs::read_dir(&session_path).await {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if file_type.is_dir() {
            return true;
        }
    }
    false
}

impl crate::protocol::Coop for CoopServer {
    async fn assign(&self, req: crate::protocol::AssignRequest) -> Result<String, String> {
        if req.binary_hash != crate::hash::binary_hash().await {
            info!("binary changed, shutting down for upgrade");
            std::process::exit(0);
        }

        let existing_request_id = {
            let reqs = self.requests.lock().await;
            reqs.iter().find_map(|(key, request)| {
                if request.session_name == req.session_name {
                    Some(key.rsplit('/').next().unwrap_or(key.as_str()).to_string())
                } else {
                    None
                }
            })
        };
        if let Some(existing_id) = existing_request_id {
            return Err(format!(
                "Your mate already has an active task (ID: {existing_id}). Wait until it finishes before assigning another."
            ));
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

        let target = match tmux::find_other_pane(&source_pane).await {
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
            let state = idle_states
                .entry(session_name.clone())
                .or_insert(IdleState {
                    empty_since: None,
                    notified: false,
                    last_title: None,
                    source_pane: None,
                    last_pane_content: None,
                    pane_unchanged_since: None,
                    parsed_pane_state: None,
                });
            state.empty_since = None;
            state.notified = false;
            state.last_title = None;
            state.source_pane = None;
            state.last_pane_content = None;
            state.pane_unchanged_since = None;
            state.parsed_pane_state = None;
        }

        let request_path = self.request_root_dir.join(&session_name).join(&request_id);
        if let Err(e) = crate::util::write_request(
            &request_path,
            &source_pane,
            &target.id,
            title_for_file.as_deref(),
            &task_content,
        )
        .await
        {
            self.requests.lock().await.remove(&request_key);
            let _ = fs::remove_dir_all(&request_path).await;
            return Err(format!(
                "failed to persist request {} to {}: {e}",
                request_id,
                request_path.display()
            ));
        }

        if clear {
            if let Err(e) = tmux::send_to_pane(&target.id, "/clear").await {
                error!("failed to send /clear to pane {}: {e}", target.id);
            }
            tokio::time::sleep(Duration::from_millis(2000)).await;
        }

        let message = format!(
            "{}\n\n\
             Before you start, activate your skill: /mate\n\n\
             {task_content}\n\n\
             When you have progress to share, need clarification, or are done, send an update:\n\n\
             cat <<'MATEEOF' | mate update {request_id}\n\
             <your update here>\n\
             MATEEOF",
            crate::warmth::greeting(),
        );

        if let Err(e) = tmux::send_to_pane(&target.id, &message).await {
            error!("failed to send to pane {}: {e}", target.id);
            self.requests.lock().await.remove(&request_key);
            if let Err(remove_err) = fs::remove_dir_all(&request_path).await
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

    async fn respond(&self, req: crate::protocol::RespondRequest) -> Result<(), String> {
        let crate::protocol::RespondRequest {
            request_id,
            session_name,
            content,
        } = req;
        let request_key = request_key(&session_name, &request_id);

        let in_memory = {
            let reqs = self.requests.lock().await;
            reqs.get(&request_key)
                .map(|r| (r.source_pane.clone(), r.title.clone()))
        };
        let (source_pane, title) = if let Some(found) = in_memory {
            found
        } else {
            let path = self.request_root_dir.join(&session_name).join(&request_id);
            match crate::util::read_request_meta(&path).await {
                Some(meta) => (meta.source_pane, meta.title),
                None => return Err(format!("no request found for {request_key}")),
            }
        };

        let intro = if let Some(t) = title.as_deref() {
            format!("Fresh from your mate — re: {t}")
        } else {
            crate::warmth::delivered().to_string()
        };
        let message = format!(
            "{intro}\n{content}\n\nRemember: you're the captain. If there's follow-up work, assign it to your mate — don't do it yourself. Stay focused on the big picture!"
        );

        // Deliver to waiter if present, otherwise via tmux directly.
        // Request state is NOT cleaned up here — only `mate accept` does that.
        let waiter_notify = {
            let mut waiters = self.waiters.lock().await;
            if let Some(w) = waiters.get_mut(&request_key) {
                w.event = Some(crate::protocol::WaitEvent::Response {
                    message: message.clone(),
                });
                Some(w.notify.clone())
            } else {
                None
            }
        };

        if let Some(notify) = waiter_notify {
            notify.notify_one();
        } else if let Err(e) = tmux::send_to_pane(&source_pane, &message).await {
            return Err(format!("failed to deliver response: {e}"));
        }

        info!("delivered response for request {request_id} in session {session_name} via RPC");
        Ok(())
    }

    async fn update(&self, req: crate::protocol::UpdateRequest) -> Result<(), String> {
        let crate::protocol::UpdateRequest {
            request_id,
            session_name,
            content,
        } = req;
        let request_key = request_key(&session_name, &request_id);

        let in_memory = {
            let reqs = self.requests.lock().await;
            reqs.get(&request_key)
                .map(|r| (r.source_pane.clone(), r.title.clone()))
        };
        let (source_pane, title) = if let Some(found) = in_memory {
            found
        } else {
            let path = self.request_root_dir.join(&session_name).join(&request_id);
            match crate::util::read_request_meta(&path).await {
                Some(meta) => (meta.source_pane, meta.title),
                None => return Err(format!("no request found for {request_key}")),
            }
        };

        let title_suffix = title
            .as_deref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        let (git_section, show_commit_reminder) = crate::util::git_commit_reminder().await;
        let commit_reminder = if show_commit_reminder {
            "\n\nThis is also a good time to commit and push your mate's work so far."
        } else {
            ""
        };
        let message = format!(
            "📋 Progress update from your mate on task {request_id}{title_suffix}:\n\n{content}\n\nWhether you're happy or unhappy with this update, reply to your mate (not the user!) with:\n\ncat <<'MATEEOF' | mate steer {request_id}\n<your reply here>\nMATEEOF\n\nIf the work looks good, accept the task:\nmate accept {request_id}{commit_reminder}{git_section}"
        );

        // Deliver to waiter if present, otherwise via tmux directly.
        let waiter_notify = {
            let mut waiters = self.waiters.lock().await;
            if let Some(w) = waiters.get_mut(&request_key) {
                w.event = Some(crate::protocol::WaitEvent::Update {
                    message: message.clone(),
                });
                Some(w.notify.clone())
            } else {
                None
            }
        };

        if let Some(notify) = waiter_notify {
            notify.notify_one();
        } else if let Err(e) = tmux::send_to_pane(&source_pane, &message).await {
            return Err(format!("failed to deliver update: {e}"));
        }

        Ok(())
    }

    async fn accept(&self, req: crate::protocol::AcceptRequest) -> Result<(), String> {
        let crate::protocol::AcceptRequest {
            request_id,
            session_name,
        } = req;
        let request_key = request_key(&session_name, &request_id);
        let request_path = self.request_root_dir.join(&session_name).join(&request_id);

        let removed_request = {
            let mut reqs = self.requests.lock().await;
            reqs.remove(&request_key)
        };
        let fallback_meta = if removed_request.is_none() {
            crate::util::read_request_meta(&request_path).await
        } else {
            None
        };
        if removed_request.is_none() && fallback_meta.is_none() {
            return Err(format!("no request found for {request_key}"));
        }

        if let Err(e) = fs::remove_dir_all(&request_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!(
                "failed to remove request directory {}: {e}",
                request_path.display()
            ));
        }

        {
            let mut waiters = self.waiters.lock().await;
            waiters.remove(&request_key);
        }

        let session_empty = {
            let reqs = self.requests.lock().await;
            !reqs.values().any(|req| req.session_name == session_name)
        } && !session_has_request_dirs(&self.request_root_dir, &session_name)
            .await;

        if session_empty {
            let (source_pane, title) = if let Some(request) = removed_request.as_ref() {
                (Some(request.source_pane.clone()), request.title.clone())
            } else if let Some(meta) = fallback_meta {
                (Some(meta.source_pane), meta.title)
            } else {
                (None, None)
            };

            let mut states = self.idle_states.lock().await;
            let state = states.entry(session_name.clone()).or_insert(IdleState {
                empty_since: None,
                notified: false,
                last_title: None,
                source_pane: None,
                last_pane_content: None,
                pane_unchanged_since: None,
                parsed_pane_state: None,
            });
            state.empty_since = Some(Instant::now());
            state.notified = false;
            state.last_title = title;
            state.source_pane = source_pane;
            state.last_pane_content = None;
            state.pane_unchanged_since = None;
            state.parsed_pane_state = None;
        }

        info!("accepted request {request_id} in session {session_name} via RPC");
        Ok(())
    }

    async fn cancel(&self, req: crate::protocol::CancelRequest) -> Result<(), String> {
        let crate::protocol::CancelRequest {
            request_id,
            session_name,
        } = req;
        let request_key = request_key(&session_name, &request_id);
        let request_path = self.request_root_dir.join(&session_name).join(&request_id);

        let removed_request = {
            let mut reqs = self.requests.lock().await;
            reqs.remove(&request_key)
        };
        let fallback_meta = if removed_request.is_none() {
            crate::util::read_request_meta(&request_path).await
        } else {
            None
        };
        if removed_request.is_none() && fallback_meta.is_none() {
            return Err(format!("no request found for {request_key}"));
        }

        if let Err(e) = fs::remove_dir_all(&request_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!(
                "failed to remove request directory {}: {e}",
                request_path.display()
            ));
        }

        {
            let mut waiters = self.waiters.lock().await;
            waiters.remove(&request_key);
        }

        let session_empty = {
            let reqs = self.requests.lock().await;
            !reqs.values().any(|req| req.session_name == session_name)
        } && !session_has_request_dirs(&self.request_root_dir, &session_name)
            .await;

        if session_empty {
            let (source_pane, title) = if let Some(request) = removed_request.as_ref() {
                (Some(request.source_pane.clone()), request.title.clone())
            } else if let Some(meta) = fallback_meta {
                (Some(meta.source_pane), meta.title)
            } else {
                (None, None)
            };

            let mut states = self.idle_states.lock().await;
            let state = states.entry(session_name.clone()).or_insert(IdleState {
                empty_since: None,
                notified: false,
                last_title: None,
                source_pane: None,
                last_pane_content: None,
                pane_unchanged_since: None,
                parsed_pane_state: None,
            });
            state.empty_since = Some(Instant::now());
            state.notified = false;
            state.last_title = title;
            state.source_pane = source_pane;
            state.last_pane_content = None;
            state.pane_unchanged_since = None;
            state.parsed_pane_state = None;
        }

        info!("cancelled request {request_id} in session {session_name} via RPC");
        Ok(())
    }

    async fn steer(&self, req: crate::protocol::SteerRequest) -> Result<(), String> {
        let crate::protocol::SteerRequest {
            request_id,
            session_name,
            content,
        } = req;
        let request_key = request_key(&session_name, &request_id);

        let target_pane = {
            let reqs = self.requests.lock().await;
            if let Some(r) = reqs.get(&request_key) {
                r.target_pane.clone()
            } else {
                return Err(format!("no request found for {request_key}"));
            }
        };

        let steer_message = format!(
            "📌 Update from the captain on task {request_id}:\n\n\
             {content}\n\n\
             If you hit a decision point, want to share progress, or need clarification, send an update:\n\n\
             cat <<'MATEEOF' | mate update {request_id}\n\
             <your progress update here>\n\
             MATEEOF"
        );

        if let Err(e) = tmux::send_to_pane(&target_pane, &steer_message).await {
            return Err(format!(
                "failed to deliver steer to pane {target_pane}: {e}"
            ));
        }

        info!("delivered steer for request {request_id} in session {session_name} via RPC");
        Ok(())
    }

    async fn wait(
        &self,
        req: crate::protocol::WaitRequest,
    ) -> Result<crate::protocol::WaitEvent, String> {
        let crate::protocol::WaitRequest {
            request_id,
            session_name,
            timeout_secs,
        } = req;
        let request_key = request_key(&session_name, &request_id);

        // If request no longer exists, the response was already delivered.
        let request_dir_exists = {
            let path = self.request_root_dir.join(&session_name).join(&request_id);
            fs::metadata(&path).await.is_ok()
        };
        let exists = {
            let reqs = self.requests.lock().await;
            reqs.contains_key(&request_key) || request_dir_exists
        };
        if !exists {
            return Ok(crate::protocol::WaitEvent::Response {
                message: "(response already delivered to your pane)".to_string(),
            });
        }

        let notify = Arc::new(tokio::sync::Notify::new());
        {
            let mut waiters = self.waiters.lock().await;
            waiters.insert(
                request_key.clone(),
                Waiter {
                    event: None,
                    notify: notify.clone(),
                },
            );
        }

        let timeout = tokio::time::Duration::from_secs(timeout_secs);
        let _ = tokio::time::timeout(timeout, notify.notified()).await;

        let event = {
            let mut waiters = self.waiters.lock().await;
            waiters
                .remove(&request_key)
                .and_then(|w| w.event)
                .unwrap_or(crate::protocol::WaitEvent::Timeout)
        };

        Ok(event)
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_root_dir: PathBuf,
    request_root_dir: PathBuf,
    log_path: PathBuf,
) -> Result<()> {
    let log_file = fs_err::tokio::File::create(&log_path)
        .await
        .map_err(|e| eyre::eyre!("failed to create log file {}: {e}", log_path.display()))?
        .into_std()
        .await
        .into_file();

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("mate=info".parse()?),
        )
        .init();

    fs::create_dir_all(&response_root_dir).await?;
    fs::create_dir_all(&request_root_dir).await?;

    if fs::metadata(&socket_path).await.is_ok() {
        fs::remove_file(&socket_path).await?;
    }

    fs::write(&pid_path, std::process::id().to_string()).await?;

    info!("mate server starting on {}", socket_path.display());
    info!("watching for responses in {}", response_root_dir.display());

    let listener = UnixListener::bind(&socket_path)?;
    let requests: Arc<Mutex<HashMap<String, Request>>> = Arc::new(Mutex::new(HashMap::new()));
    let idle_states: Arc<Mutex<HashMap<String, IdleState>>> = Arc::new(Mutex::new(HashMap::new()));
    let waiters: Arc<Mutex<HashMap<String, Waiter>>> = Arc::new(Mutex::new(HashMap::new()));

    restore_requests_from_disk(&request_root_dir, &response_root_dir, &requests).await;

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
        let server_waiters = waiters.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
                request_root_dir: server_request_root,
                idle_states: server_idle_states,
                waiters: server_waiters,
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
                    run_staleness_checks(
                        &request_root_dir,
                        &response_root_dir,
                        &idle_states,
                        &mut pane_states,
                    ).await;
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
                    run_staleness_checks(
                        &request_root_dir,
                        &response_root_dir,
                        &idle_states,
                        &mut pane_states,
                    ).await;
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
        source_pane: String,
        pane_capture: String,
    }

    struct Candidate {
        session_name: String,
        source_pane: String,
    }

    let candidates: Vec<Candidate> = {
        let states = idle_states.lock().await;
        states
            .iter()
            .filter_map(|(session_name, state)| {
                let source_pane = state.source_pane.as_deref()?;
                let empty_since = state.empty_since?;
                if state.notified || empty_since.elapsed() < IDLE_NOTIFY_DELAY {
                    return None;
                }
                Some(Candidate {
                    session_name: session_name.clone(),
                    source_pane: source_pane.to_string(),
                })
            })
            .collect()
    };

    let mut sessions_to_notify: Vec<PendingNotify> = Vec::new();
    for candidate in candidates {
        let pane_capture = match tmux::capture_pane(&candidate.source_pane).await {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "failed to capture source pane {} for idle tracking in session {}: {e}",
                    candidate.source_pane, candidate.session_name
                );
                continue;
            }
        };

        let now = Instant::now();
        let mut states = idle_states.lock().await;
        let Some(state) = states.get_mut(&candidate.session_name) else {
            continue;
        };
        let Some(empty_since) = state.empty_since else {
            continue;
        };
        if state.notified || empty_since.elapsed() < IDLE_NOTIFY_DELAY {
            continue;
        }
        if state.source_pane.as_deref() != Some(candidate.source_pane.as_str()) {
            continue;
        }

        match state.last_pane_content.as_deref() {
            Some(previous) if previous == pane_capture => {
                let unchanged_since = state.pane_unchanged_since.get_or_insert(now);
                if unchanged_since.elapsed() < IDLE_NOTIFY_DELAY {
                    continue;
                }
                // Mark before releasing lock so we can't double-fire on the next tick.
                state.notified = true;
                sessions_to_notify.push(PendingNotify {
                    session_name: candidate.session_name,
                    empty_since,
                    last_title: state.last_title.clone(),
                    source_pane: candidate.source_pane,
                    pane_capture,
                });
            }
            _ => {
                state.last_pane_content = Some(pane_capture);
                state.pane_unchanged_since = Some(now);
            }
        }
    }

    for pending in sessions_to_notify {
        let mut message = format!(
            "Your captain in session **{}** has no more tasks — time to check in!",
            pending.session_name
        );
        if let Some(last_title) = pending.last_title.as_deref() {
            message.push_str(&format!("\nLast completed: **{last_title}**"));
        }
        let lines: Vec<&str> = pending.pane_capture.lines().collect();
        let lines = crate::util::trim_agent_footer(&lines);
        if lines.is_empty() {
            continue;
        }
        let half = lines.len() / 2;
        let bottom: String = lines[half..].join("\n");
        message.push_str(&format!("\n```\n{bottom}\n```"));
        if let Err(e) = crate::discord::notify(webhook_url, &message).await {
            error!(
                "failed to send Discord idle notification for session {}: {e}",
                pending.session_name
            );
            let mut states = idle_states.lock().await;
            if let Some(state) = states.get_mut(&pending.session_name)
                && state.empty_since == Some(pending.empty_since)
                && state.source_pane.as_deref() == Some(pending.source_pane.as_str())
            {
                state.notified = false;
            }
            continue;
        }
    }
}

async fn run_staleness_checks(
    request_root_dir: &Path,
    response_root_dir: &Path,
    idle_states: &Arc<Mutex<HashMap<String, IdleState>>>,
    pane_states: &mut HashMap<String, PaneState>,
) {
    let mut active_request_keys: HashSet<String> = HashSet::new();

    let mut session_entries = match fs::read_dir(request_root_dir).await {
        Ok(entries) => entries,
        Err(_) => {
            pane_states.clear();
            return;
        }
    };

    while let Ok(Some(session_entry)) = session_entries.next_entry().await {
        let session_path = session_entry.path();
        let session_name = match session_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let Ok(file_type) = session_entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let mut request_entries = match fs::read_dir(&session_path).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        while let Ok(Some(request_entry)) = request_entries.next_entry().await {
            let request_path = request_entry.path();
            let Ok(file_type) = request_entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
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
            if fs::metadata(&response_path).await.is_ok() {
                continue;
            }

            let meta = match crate::util::read_request_meta(&request_path).await {
                Some(meta) => meta,
                None => continue,
            };
            let pane_content = match tmux::capture_pane(&meta.target_pane).await {
                Ok(content) => content,
                Err(e) => {
                    error!(
                        "failed to capture pane {} for request {}: {e}",
                        meta.target_pane, request_id
                    );
                    continue;
                }
            };
            let parsed_pane_state = pane::parse_pane_content(&pane_content);
            {
                let mut states = idle_states.lock().await;
                let state = states.entry(session_name.clone()).or_insert(IdleState {
                    empty_since: None,
                    notified: false,
                    last_title: None,
                    source_pane: None,
                    last_pane_content: None,
                    pane_unchanged_since: None,
                    parsed_pane_state: None,
                });
                state.parsed_pane_state = Some(parsed_pane_state.clone());
            }

            if matches!(parsed_pane_state.state, pane::AgentState::Unknown) {
                let unparsed = pane_content
                    .lines()
                    .rev()
                    .take(20)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                warn!(
                    "UNPARSED_PANE: session={} request={} pane={}\n{}",
                    session_name, request_id, meta.target_pane, unparsed
                );
            } else if matches!(parsed_pane_state.state, pane::AgentState::Idle) {
                info!("Mate appears idle on task {request_id} without responding");
            }

            let state = pane_states.entry(key).or_insert_with(|| PaneState {
                last_content: pane_content.clone(),
                unchanged_count: 0,
                captain_last_content: String::new(),
                captain_unchanged_count: 0,
                notified: false,
                idle_since: None,
                idle_nudged: false,
            });

            if matches!(parsed_pane_state.state, pane::AgentState::Idle) {
                if state.idle_since.is_none() {
                    state.idle_since = Some(Instant::now());
                }
            } else {
                state.idle_since = None;
            }

            if !state.idle_nudged
                && let Some(idle_since) = state.idle_since
                && idle_since.elapsed() >= IDLE_NUDGE_AFTER
            {
                let buddy_reminder = format!(
                    "⚠️ You have an open task (ID: {request_id}) but appear to be idle.\nPlease send an update:\n\ncat <<'EOF' | mate update {request_id}\n<summary of what you did>\nEOF"
                );
                match tmux::send_to_pane(&meta.target_pane, &buddy_reminder).await {
                    Ok(()) => {
                        state.idle_nudged = true;
                    }
                    Err(e) => {
                        error!(
                            "failed to send idle nudge to mate pane {} for request {}: {e}",
                            meta.target_pane, request_id
                        );
                    }
                }
            }

            if pane_content == state.last_content {
                state.unchanged_count += 1;
            } else {
                state.last_content = pane_content.clone();
                state.unchanged_count = 0;
                state.notified = false;
            }

            let captain_pane_content = match tmux::capture_pane(&meta.source_pane).await {
                Ok(content) => content,
                Err(e) => {
                    error!(
                        "failed to capture captain pane {} for request {}: {e}",
                        meta.source_pane, request_id
                    );
                    continue;
                }
            };

            if captain_pane_content == state.captain_last_content {
                state.captain_unchanged_count += 1;
            } else {
                state.captain_last_content = captain_pane_content;
                state.captain_unchanged_count = 0;
                state.notified = false;
            }

            if state.notified
                || state.unchanged_count < STALENESS_NOTIFY_AFTER_UNCHANGED
                || state.captain_unchanged_count < STALENESS_NOTIFY_AFTER_UNCHANGED
            {
                continue;
            }

            let title_suffix = meta
                .title
                .as_deref()
                .map(|title| format!(" ({title})"))
                .unwrap_or_default();
            let message = format!(
                "⏰ Hey captain — your mate seems stuck on task {request_id}{title_suffix}. Both panes have been unchanged for 2 minutes.\n\nMate pane content:\n```\n{pane_content}\n```"
            );
            if let Err(e) = tmux::send_to_pane(&meta.source_pane, &message).await {
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
    let mut session_entries = match fs::read_dir(response_root_dir).await {
        Ok(entries) => entries,
        Err(_) => return,
    };

    while let Ok(Some(session_entry)) = session_entries.next_entry().await {
        let session_path = session_entry.path();
        let session_name = match session_entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        let Ok(file_type) = session_entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let mut response_entries = match fs::read_dir(&session_path).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        while let Ok(Some(response_entry)) = response_entries.next_entry().await {
            let response_path = response_entry.path();
            let Ok(file_type) = response_entry.file_type().await else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let Some(filename) = response_path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if filename.contains(".update.")
                || filename.contains(".final.")
                || filename.ends_with(".waiter")
            {
                continue;
            }

            let request_id = match response_path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let key = request_key(&session_name, &request_id);
            let body = match fs::read_to_string(&response_path).await {
                Ok(content) => content,
                Err(_) => "(could not read response file)".to_string(),
            };
            if body.trim() == "Accepted" {
                // Legacy file-based accept marker; accept is RPC-only now.
                // Dropping this marker avoids double-processing races.
                let waiter_marker = session_path.join(format!("{request_id}.waiter"));
                let _ = fs::remove_file(&waiter_marker).await;
                if let Err(e) = fs::remove_file(&response_path).await
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    error!(
                        "failed to remove legacy accept marker {}: {e}",
                        response_path.display()
                    );
                }
                info!(
                    "ignored legacy file-based accept marker for request {request_id} in session {session_name}"
                );
                continue;
            }

            let in_memory_request = {
                let mut reqs = requests.lock().await;
                reqs.remove(&key)
            };

            let request_path = request_root_dir.join(&session_name).join(&request_id);
            let (source_pane, target_pane, title) = if let Some(request) = in_memory_request {
                (request.source_pane, request.target_pane, request.title)
            } else {
                match crate::util::read_request_meta(&request_path).await {
                    Some(meta) => (meta.source_pane, meta.target_pane, meta.title),
                    None => {
                        if let Err(e) = fs::create_dir_all(orphaned_dir()).await {
                            error!(
                                "failed to create orphaned response directory {}: {e}",
                                orphaned_dir().display()
                            );
                            continue;
                        }
                        let orphaned_path =
                            orphaned_dir().join(format!("{session_name}-{request_id}.md"));
                        let move_result = match fs::rename(&response_path, &orphaned_path).await {
                            Ok(()) => Ok(()),
                            Err(_) => {
                                if let Err(copy_err) =
                                    fs::copy(&response_path, &orphaned_path).await
                                {
                                    Err(copy_err)
                                } else {
                                    match fs::remove_file(&response_path).await {
                                        Ok(()) => Ok(()),
                                        Err(remove_err) => Err(remove_err),
                                    }
                                }
                            }
                        };
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

            let intro = if let Some(title) = title.as_deref() {
                format!("Fresh from your mate — re: {title}")
            } else {
                crate::warmth::delivered().to_string()
            };
            let (git_section, show_commit_reminder) = crate::util::git_commit_reminder().await;
            let commit_reminder = if show_commit_reminder {
                "\n\nThis is also a good time to commit and push your mate's work so far."
            } else {
                ""
            };
            let message = format!(
                "{intro}\n{body}\n\nRemember: you're the captain. If there's follow-up work, assign it to your mate — don't do it yourself. Stay focused on the big picture!{commit_reminder}{git_section}"
            );
            let waiter_marker = session_path.join(format!("{request_id}.waiter"));
            let waiter_exists = fs::metadata(&waiter_marker).await.is_ok();
            if waiter_exists {
                let final_path = session_path.join(format!("{request_id}.final.md"));
                if let Err(e) = fs::write(&final_path, &message).await {
                    error!(
                        "failed to write final wait response for request {} in session {} to {}: {e}",
                        request_id,
                        session_name,
                        final_path.display()
                    );
                    continue;
                }
            } else if let Err(e) = tmux::send_to_pane(&source_pane, &message).await {
                error!(
                    "failed to deliver response to pane {} for request {}: {e}",
                    source_pane, request_id
                );
                continue;
            }
            let _ = fs::remove_file(&waiter_marker).await;

            if let Err(e) = fs::remove_dir_all(&request_path).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!(
                    "failed to remove request directory {}: {e}",
                    request_path.display()
                );
            }
            pane_states.remove(&key);
            if let Err(e) = fs::remove_file(&response_path).await
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
                    last_pane_content: None,
                    pane_unchanged_since: None,
                    parsed_pane_state: None,
                });
                state.empty_since = Some(Instant::now());
                state.notified = false;
                state.last_title = title.clone();
                state.source_pane = Some(source_pane.clone());
                state.last_pane_content = None;
                state.pane_unchanged_since = None;
                state.parsed_pane_state = None;
            }
        }
    }
}
