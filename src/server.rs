use crate::protocol::{CoopClient, CoopDispatcher};
use crate::tmux;
use eyre::Result;
use roam_stream::StreamLink;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{error, info};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct Request {
    source_pane: String,
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
            clear,
            binary_hash: _,
        } = req;
        let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let source_pane_for_file = source_pane.clone();

        // Find the other pane to send to
        let target = match tmux::find_other_pane(&source_pane) {
            Ok(p) => p,
            Err(e) => {
                error!("failed to find worker pane: {e}");
                return Err(e.to_string());
            }
        };

        // Store the request
        self.requests
            .lock()
            .await
            .insert(request_id.clone(), Request { source_pane });
        let request_path = self.request_dir.join(&request_id);
        if let Err(e) = std::fs::write(&request_path, &source_pane_for_file) {
            self.requests.lock().await.remove(&request_id);
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
             {content}\n\n\
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
            if let Err(remove_err) = std::fs::remove_file(&request_path)
                && remove_err.kind() != std::io::ErrorKind::NotFound
            {
                error!(
                    "failed to remove request file {}: {remove_err}",
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
                    std::future::pending::<()>().await;
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
    let mut timeout_notified: HashSet<String> = HashSet::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let mut active_request_ids: HashSet<String> = HashSet::new();
        if let Ok(request_entries) = std::fs::read_dir(&request_dir) {
            for entry in request_entries.flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(file_type) if file_type.is_file() => {}
                    _ => continue,
                }

                let request_id = match path.file_name().and_then(|s| s.to_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                active_request_ids.insert(request_id.clone());

                let response_path = response_dir.join(format!("{request_id}.md"));
                if response_path.exists() || timeout_notified.contains(&request_id) {
                    continue;
                }

                let metadata = match std::fs::metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                let created_or_modified = metadata
                    .created()
                    .ok()
                    .or_else(|| metadata.modified().ok());
                let age = match created_or_modified
                    .and_then(|timestamp| SystemTime::now().duration_since(timestamp).ok())
                {
                    Some(age) => age,
                    None => continue,
                };

                if age <= REQUEST_TIMEOUT {
                    continue;
                }

                let source_pane = match std::fs::read_to_string(&path) {
                    Ok(source_pane) => source_pane.trim().to_string(),
                    Err(_) => continue,
                };
                let message = format!(
                    "⏰ Hey captain — your task {request_id} has been pending for {}. Your buddy might be stuck!",
                    format_age(age)
                );
                if let Err(e) = tmux::send_to_pane(&source_pane, &message) {
                    error!(
                        "failed to deliver timeout notification for request {} to pane {}: {e}",
                        request_id, source_pane
                    );
                    continue;
                }

                timeout_notified.insert(request_id);
            }
        }
        timeout_notified.retain(|id| active_request_ids.contains(id));

        let entries = match std::fs::read_dir(&response_dir) {
            Ok(e) => e,
            Err(_) => continue,
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
            let source_pane = if let Some(request) = in_memory_request {
                request.source_pane
            } else {
                match std::fs::read_to_string(&request_path) {
                    Ok(source_pane) => source_pane.trim().to_string(),
                    Err(_) => continue,
                }
            };

            let body = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(_) => "(could not read response file)".to_string(),
            };
            let message = format!(
                "{}\n{body}\n\nRemember: you're the captain. If there's follow-up work, assign it to your buddy — don't do it yourself. Stay focused on the big picture!",
                crate::warmth::delivered()
            );
            if let Err(e) = tmux::send_to_pane(&source_pane, &message) {
                error!(
                    "failed to deliver response to pane {}: {e}",
                    source_pane
                );
                continue;
            }

            if let Err(e) = std::fs::remove_file(&request_path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!("failed to remove request file {}: {e}", request_path.display());
            }
            timeout_notified.remove(&request_id);
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!("failed to remove response file {}: {e}", path.display());
            }

            info!("delivered response for request {request_id}");
        }
    }
}

fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}
