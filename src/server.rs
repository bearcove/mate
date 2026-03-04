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
}

#[derive(Clone)]
struct CoopServer {
    requests: Arc<Mutex<HashMap<String, Request>>>,
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
        }

        info!("assigned request {request_id} -> pane {}", target.id);
        Ok(request_id)
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_dir: PathBuf,
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
    let watch_dir = response_dir.clone();
    tokio::spawn(async move {
        watch_responses(watch_dir, watch_requests).await;
    });

    // Accept connections
    loop {
        let (stream, _) = listener.accept().await?;
        let reqs = requests.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
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

            let request_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            let request = {
                let mut reqs = requests.lock().await;
                reqs.remove(&request_id)
            };
            if let Some(request) = request {
                seen.insert(path.clone());

                let body = match std::fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(_) => "(could not read response file)".to_string(),
                };
                let message = format!(
                    "{}\n{body}",
                    crate::warmth::delivered()
                );
                if let Err(e) = tmux::send_to_pane(&request.source_pane, &message) {
                    error!(
                        "failed to deliver response to pane {}: {e}",
                        request.source_pane
                    );
                }

                info!("delivered response for request {request_id}");
            }
        }
    }
}

