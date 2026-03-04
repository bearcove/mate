mod protocol;
mod server;
mod tmux;

use eyre::Result;
use facet::Facet;
use figue as args;
use std::path::PathBuf;

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
    /// Assign a task to another agent
    Assign {
        /// Path to a file containing the task description
        #[facet(args::positional)]
        task_file: PathBuf,
    },
}

const MANUAL: &str = r#"bud - multi-agent cooperation over tmux

USAGE:
    bud                        Show this manual
    bud server                 Start the server (usually auto-started)
    bud assign <task-file>     Assign a task to another agent

WORKFLOW:
    1. Write your task to a file, e.g. /tmp/bud-task-xyz.md
    2. Run: bud assign /tmp/bud-task-xyz.md
    3. The server delivers the task to the worker agent's tmux pane
    4. The worker writes their response to the file path given to them
    5. The server detects the response and delivers it back to your pane

ENVIRONMENT:
    TMUX_PANE    Automatically set by tmux. Used to identify your pane.
    BUD_SOCKET  Override the server socket path (default: /tmp/bud.sock)
"#;

fn socket_path() -> PathBuf {
    std::env::var("BUD_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/bud.sock"))
}

fn pid_path() -> PathBuf {
    PathBuf::from("/tmp/bud.pid")
}

fn response_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-responses")
}

fn log_path() -> PathBuf {
    PathBuf::from("/tmp/bud-server.log")
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
            server::run_server(socket_path(), pid_path(), response_dir(), log_path()).await
        }
        Some(Command::Assign { task_file }) => {
            let pane = std::env::var("TMUX_PANE")
                .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
            ensure_server_running().await?;
            client_assign(pane, task_file).await
        }
    }
}

async fn ensure_server_running() -> Result<()> {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            // Check if process is alive
            let status = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status();
            if status.is_ok_and(|s| s.success()) {
                return Ok(());
            }
        }
    }

    // Server not running — clean up stale socket if any
    let socket = socket_path();
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }

    // Start it
    eprintln!("bud: starting server...");
    let exe = std::env::current_exe()?;
    let log_file = std::fs::File::create(log_path())?;
    std::process::Command::new(exe)
        .arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()?;

    // Wait for socket to appear
    for _ in 0..50 {
        if socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(eyre::eyre!("bud: server failed to start (check {})", log_path().display()))
}

async fn client_assign(source_pane: String, task_file: PathBuf) -> Result<()> {
    use roam_stream::StreamLink;

    if !task_file.exists() {
        return Err(eyre::eyre!("task file not found: {}", task_file.display()));
    }

    let stream = tokio::net::UnixStream::connect(socket_path()).await?;
    let (client, _sh) = roam::initiator(StreamLink::unix(stream))
        .establish::<protocol::CoopClient>(())
        .await?;

    let request_id = client
        .assign(protocol::AssignRequest {
            source_pane,
            task_file: task_file.to_string_lossy().into_owned(),
        })
        .await
        .map_err(|e| eyre::eyre!("{e:?}"))?;

    eprintln!("bud: task assigned (request_id={request_id})");
    eprintln!("bud: response will be delivered to your pane when ready");
    Ok(())
}
