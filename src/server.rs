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
    response_dir: PathBuf,
    task_dir: PathBuf,
}

impl crate::protocol::Coop for CoopServer {
    async fn assign(&self, req: crate::protocol::AssignRequest) -> String {
        let crate::protocol::AssignRequest {
            source_pane,
            content,
            clear,
        } = req;
        let request_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let response_file = self.response_dir.join(format!("{request_id}.md"));
        let task_file = self.task_dir.join(format!("{request_id}.md"));

        // Write the task content to a file
        if let Err(e) = std::fs::write(&task_file, &content) {
            error!("failed to write task file {}: {e}", task_file.display());
            return format!("ERROR: {e}");
        }

        // Find the other pane to send to
        let target = match tmux::find_other_pane(&source_pane) {
            Ok(p) => p,
            Err(e) => {
                error!("failed to find worker pane: {e}");
                return format!("ERROR: {e}");
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
            "🌱 Hey! A buddy of yours needs help with something — your assistance \
             is much appreciated and they'll be very thankful.\n\n\
             🌱 The full assignment is at: {}\n\
             Please read it, then do your best to help.\n\n\
             🌱 IMPORTANT: When you're done, you MUST send your response by executing \
             this shell command (use your Bash/shell tool — do NOT just print it as text):\n\n\
             cat <<'BUDEOF' | bud respond {request_id}\n\
             <put your full response here>\n\
             BUDEOF",
            task_file.display()
        );

        if let Err(e) = tmux::send_to_pane(&target.id, &message) {
            error!("failed to send to pane {}: {e}", target.id);
        }

        info!(
            "assigned request {request_id}: {} -> pane {}",
            task_file.display(),
            target.id
        );
        request_id
    }
}

pub async fn run_server(
    socket_path: PathBuf,
    pid_path: PathBuf,
    response_dir: PathBuf,
    task_dir: PathBuf,
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
    std::fs::create_dir_all(&task_dir)?;

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
        let resp_dir = response_dir.clone();
        let t_dir = task_dir.clone();
        tokio::spawn(async move {
            let server = CoopServer {
                requests: reqs,
                response_dir: resp_dir,
                task_dir: t_dir,
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

            let mut reqs = requests.lock().await;
            if let Some(request) = reqs.remove(&request_id) {
                seen.insert(path.clone());

                let body = match std::fs::read_to_string(&path) {
                    Ok(content) => summarize_response(&content, &path),
                    Err(_) => "(could not read response file)".to_string(),
                };
                let message = format!(
                    "🌱 Your buddy came through! Here's their response to request {request_id}:\n{body}"
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

fn summarize_response(content: &str, response_path: &std::path::Path) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    if n <= 20 {
        return content.to_string();
    }
    let head = &lines[..10];
    let tail = &lines[n - 10..];
    let cut = n - 20;
    format!(
        "{}\n[{cut} lines cut — complete response at {}]\n{}",
        head.join("\n"),
        response_path.display(),
        tail.join("\n")
    )
}
