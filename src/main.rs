mod protocol;
mod server;
mod tmux;

use eyre::Result;
use facet::Facet;
use figue as args;
use std::io::Read as _;
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
    /// Assign a task to another agent (reads from stdin)
    Assign {
        /// Clear the worker's context before sending the task
        #[facet(args::named)]
        clear: bool,
    },
    /// Respond to a task (reads from stdin)
    Respond {
        /// The request ID to respond to
        #[facet(args::positional)]
        request_id: String,
    },
}

const MANUAL: &str = r#"bud - cooperative agents over tmux

USAGE:
    bud                              Show this manual
    bud server                       Start the server (usually auto-started)
    cat <<'EOF' | bud assign         Assign a task (reads stdin)
    cat <<'EOF' | bud assign --clear Assign with fresh context
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

fn response_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-responses")
}

fn task_dir() -> PathBuf {
    PathBuf::from("/tmp/bud-tasks")
}

fn log_path() -> PathBuf {
    PathBuf::from("/tmp/bud-server.log")
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
            server::run_server(socket_path(), pid_path(), response_dir(), task_dir(), log_path())
                .await
        }
        Some(Command::Assign { clear }) => {
            let pane = std::env::var("TMUX_PANE")
                .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
            let content = read_stdin()?;
            ensure_server_running().await?;
            client_assign(pane, content, clear).await
        }
        Some(Command::Respond { request_id }) => {
            let content = read_stdin()?;
            // Write the response file directly — no RPC needed
            std::fs::create_dir_all(response_dir())?;
            let path = response_dir().join(format!("{request_id}.md"));
            std::fs::write(&path, &content)?;
            eprintln!("🌱 Response sent for request {request_id}!");
            Ok(())
        }
    }
}

async fn ensure_server_running() -> Result<()> {
    let pid_file = pid_path();
    if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
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

    eprintln!("🌱 Starting bud server...");
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

async fn client_assign(source_pane: String, content: String, clear: bool) -> Result<()> {
    use roam_stream::StreamLink;

    let stream = tokio::net::UnixStream::connect(socket_path()).await?;
    let (client, _sh) = roam::initiator(StreamLink::unix(stream))
        .establish::<protocol::CoopClient>(())
        .await?;

    let request_id = client
        .assign(protocol::AssignRequest {
            source_pane,
            content,
            clear,
        })
        .await
        .map_err(|e| eyre::eyre!("{e:?}"))?;

    eprintln!("🌱 Task assigned (request_id={request_id})");
    eprintln!("🌱 Your buddy's on it now — they might be a few minutes so sit back and relax.");
    eprintln!("🌱 When they're done, you'll get the reply back as a regular chat message.");
    Ok(())
}
