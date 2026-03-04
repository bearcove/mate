use crate::protocol::{CoopClient, CoopDispatcher};
use crate::tmux;
use eyre::Result;
use roam_stream::StreamLink;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tracing::{error, info};

struct Request {
    source_pane: String,
    response_file: PathBuf,
}

#[derive(Clone)]
struct CoopServer {
    requests: Arc<Mutex<HashMap<String, Request>>>,
    response_dir: PathBuf,
}

impl crate::protocol::Coop for CoopServer {
    async fn assign(&self, req: crate::protocol::AssignRequest) -> String {
        let crate::protocol::AssignRequest { source_pane, task_file } = req;
        let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let response_file = self.response_dir.join(format!("{request_id}.md"));

        // Read the task content
        let task_content = match std::fs::read_to_string(&task_file) {
            Ok(c) => c,
            Err(e) => {
                error!("failed to read task file {task_file}: {e}");
                return format!("ERROR: {e}");
            }
        };

        // Find the other pane to send to
        let target = match tmux::find_other_pane(&source_pane) {
            Ok(p) => p,
            Err(e) => {
                error!("failed to find worker pane: {e}");
                return format!("ERROR: {e}");
            }
        };

        // Compose the message for the worker
        let message = format!(
            "[bud request {request_id}] You have a task. Read it from: {task_file}\n\
             When done, write your response to: {}\n\
             Include the request ID '{request_id}' in your response file.",
            response_file.display()
        );

        // Store the request
        self.requests.lock().await.insert(
            request_id.clone(),
            Request {
                source_pane,
                response_file: response_file.clone(),
            },
        );

        // Send to worker pane
        if let Err(e) = tmux::send_to_pane(&target.id, &message) {
            error!("failed to send to pane {}: {e}", target.id);
        }

        info!("assigned request {request_id}: {task_file} -> pane {}", target.id);
        request_id
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_dir: PathBuf,
    log_path: PathBuf,
) -> Result<()> {
    // Set up logging to file
    let log_file = std::fs::File::create(&log_path)?;
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bud=info".parse()?),
        )
        .init();

    // Create response directory
    std::fs::create_dir_all(&response_dir)?;

    // Clean up stale socket
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    // Write PID file
    std::fs::write(&pid_path, std::process::id().to_string())?;

    info!("bud server starting on {}", socket_path.display());
    info!("watching for responses in {}", response_dir.display());

    let listener = UnixListener::bind(&socket_path)?;

    let requests: Arc<Mutex<HashMap<String, Request>>> = Arc::new(Mutex::new(HashMap::new()));

    // Spawn response watcher
    let watch_requests = requests.clone();
    let watch_dir = response_dir.clone();
    tokio::spawn(async move {
        watch_responses(watch_dir, watch_requests).await;
    });

    // Accept connections
    loop {
        let (stream, _) = listener.accept().await?;
        let reqs = requests.clone();
        let resp_dir = response_dir.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
                response_dir: resp_dir,
            };
            let result = roam::acceptor(StreamLink::unix(stream))
                .establish::<CoopClient>(CoopDispatcher::new(server))
                .await;
            match result {
                Ok((_caller, sh)) => {
                    // Hold the caller alive so the session doesn't drop
                    std::future::pending::<()>().await;
                    drop(_caller);
                    drop(sh);
                }
                Err(e) => error!("connection failed: {e}"),
            }
        });
    }
}

async fn watch_responses(dir: PathBuf, requests: Arc<Mutex<HashMap<String, Request>>>) {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if seen.contains(&path) {
                continue;
            }

            // Extract request ID from filename (e.g., "abc12345.md" -> "abc12345")
            let request_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            let mut reqs = requests.lock().await;
            if let Some(request) = reqs.remove(&request_id) {
                seen.insert(path.clone());

                // Read response content
                let response_content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        error!("failed to read response {}: {e}", path.display());
                        continue;
                    }
                };

                // Deliver back to the requesting agent's pane
                let message = format!(
                    "[bud response {request_id}] Task complete. Response:\n{response_content}"
                );
                if let Err(e) = tmux::send_to_pane(&request.source_pane, &message) {
                    error!("failed to deliver response to pane {}: {e}", request.source_pane);
                }

                info!("delivered response for request {request_id}");
            }
        }
    }
}
