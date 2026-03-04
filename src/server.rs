use crate::protocol::{CoopClient, CoopDispatcher};
use crate::tmux;
use eyre::Result;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use roam_stream::StreamLink;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{error, info};

const STALENESS_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);
const STALENESS_NOTIFY_AFTER_UNCHANGED: u32 = 4;

struct Request {
    source_pane: String,
    target_pane: String,
    title: Option<String>,
}

struct PaneState {
    last_content: String,
    unchanged_count: u32,
    notified: bool,
}

#[derive(Clone)]
struct CoopServer {
    requests: Arc<Mutex<HashMap<String, Request>>>,
    request_dir: PathBuf,
}

impl crate::protocol::Coop for CoopServer {
    async fn assign(&self, req: crate::protocol::AssignRequest) -> Result<String, String> {
        if req.binary_hash != crate::hash::binary_hash() {
            info!("binary changed, shutting down for upgrade");
            std::process::exit(0);
        }

        let crate::protocol::AssignRequest {
            source_pane,
            content,
            title,
            clear,
            binary_hash: _,
        } = req;
        let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let title_for_file = title.clone();

        // Find the other pane to send to
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

        // Store the request
        self.requests
            .lock()
            .await
            .insert(
                request_id.clone(),
                Request {
                    source_pane: source_pane.clone(),
                    target_pane: target.id.clone(),
                    title,
                },
            );
        let request_path = self.request_dir.join(&request_id);
        if let Err(e) = crate::util::write_request(
            &request_path,
            &source_pane,
            &target.id,
            title_for_file.as_deref(),
            &task_content,
        ) {
            self.requests.lock().await.remove(&request_id);
            let _ = std::fs::remove_dir_all(&request_path);
            return Err(format!(
                "failed to persist request {} to {}: {e}",
                request_id,
                request_path.display()
            ));
        }

        // Optionally clear the worker's context first
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
            self.requests.lock().await.remove(&request_id);
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

        info!("assigned request {request_id} -> pane {}", target.id);
        Ok(request_id)
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_dir: PathBuf,
    request_dir: PathBuf,
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

    std::fs::create_dir_all(&response_dir)?;
    std::fs::create_dir_all(&request_dir)?;

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    std::fs::write(&pid_path, std::process::id().to_string())?;

    info!("bud server starting on {}", socket_path.display());
    info!("watching for responses in {}", response_dir.display());

    let listener = UnixListener::bind(&socket_path)?;
    let requests: Arc<Mutex<HashMap<String, Request>>> = Arc::new(Mutex::new(HashMap::new()));

    // Spawn response watcher
    let watch_requests = requests.clone();
    let watch_response_dir = response_dir.clone();
    let watch_request_dir = request_dir.clone();
    tokio::spawn(async move {
        watch_responses(watch_response_dir, watch_request_dir, watch_requests).await;
    });

    // Accept connections
    loop {
        let (stream, _) = listener.accept().await?;
        let reqs = requests.clone();
        let server_request_dir = request_dir.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
                request_dir: server_request_dir,
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
    response_dir: PathBuf,
    request_dir: PathBuf,
    requests: Arc<Mutex<HashMap<String, Request>>>,
) {
    let mut pane_states: HashMap<String, PaneState> = HashMap::new();
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut watcher: Option<RecommendedWatcher> =
        match RecommendedWatcher::new(
            move |result| {
                let _ = event_tx.send(result);
            },
            Config::default(),
        ) {
            Ok(mut watcher) => {
                if let Err(e) = watcher.watch(&response_dir, RecursiveMode::NonRecursive) {
                    error!(
                        "failed to start response watcher on {}: {e}; falling back to polling",
                        response_dir.display()
                    );
                    None
                } else {
                    Some(watcher)
                }
            }
            Err(e) => {
                error!(
                    "failed to initialize response watcher for {}: {e}; falling back to polling",
                    response_dir.display()
                );
                None
            }
        };

    process_response_files(&response_dir, &request_dir, &requests, &mut pane_states).await;

    let mut staleness_tick = tokio::time::interval(STALENESS_SAMPLE_INTERVAL);
    let mut poll_tick = tokio::time::interval(Duration::from_secs(2));

    loop {
        if watcher.is_some() {
            tokio::select! {
                _ = staleness_tick.tick() => {
                    run_staleness_checks(&request_dir, &response_dir, &mut pane_states);
                }
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(Ok(event)) if matches!(event.kind, EventKind::Create(_)) => {
                            process_response_files(&response_dir, &request_dir, &requests, &mut pane_states).await;
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
                    run_staleness_checks(&request_dir, &response_dir, &mut pane_states);
                }
                _ = poll_tick.tick() => {
                    process_response_files(&response_dir, &request_dir, &requests, &mut pane_states).await;
                }
            }
        }
    }
}

fn run_staleness_checks(
    request_dir: &std::path::Path,
    response_dir: &std::path::Path,
    pane_states: &mut HashMap<String, PaneState>,
) {
    let mut active_request_ids: HashSet<String> = HashSet::new();
    if let Ok(request_entries) = std::fs::read_dir(request_dir) {
        for entry in request_entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {}
                _ => continue,
            }

            let request_id = match path.file_name().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            active_request_ids.insert(request_id.clone());

            let response_path = response_dir.join(format!("{request_id}.md"));
            if response_path.exists() {
                continue;
            }

            let meta = match crate::util::read_request_meta(&path) {
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

            let state = pane_states
                .entry(request_id.clone())
                .or_insert_with(|| PaneState {
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
    pane_states.retain(|id, _| active_request_ids.contains(id));
}

async fn process_response_files(
    response_dir: &std::path::Path,
    request_dir: &std::path::Path,
    requests: &Arc<Mutex<HashMap<String, Request>>>,
    pane_states: &mut HashMap<String, PaneState>,
) {
    let entries = match std::fs::read_dir(response_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let request_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let in_memory_request = {
            let mut reqs = requests.lock().await;
            reqs.remove(&request_id)
        };
        let request_path = request_dir.join(&request_id);
        let (source_pane, target_pane, title) = if let Some(request) = in_memory_request {
            (request.source_pane, request.target_pane, request.title)
        } else {
            match crate::util::read_request_meta(&request_path) {
                Some(meta) => (meta.source_pane, meta.target_pane, meta.title),
                None => continue,
            }
        };

        let body = match std::fs::read_to_string(&path) {
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
        pane_states.remove(&request_id);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            error!("failed to remove response file {}: {e}", path.display());
        }

        info!("delivered response for request {request_id} (target pane {target_pane})");
    }
}
