mod client;
mod config;
mod discord;
mod github;
mod hash;
mod issues;
mod listing;
mod pane;
mod paths;
mod protocol;
mod requests;
mod server;
mod tmux;
mod util;
mod warmth;
mod watch;

use eyre::Result;
use facet::Facet;
use figue as args;
use paths::{
    log_path, pid_path, read_stdin, request_root_dir, response_root_dir, socket_path,
    tmux_session_name, tmux_session_name_for_pane,
};

#[derive(Facet, Debug)]
struct Args {
    #[facet(args::subcommand)]
    command: Option<Command>,
}

#[derive(Facet, Debug)]
#[repr(u8)]
enum Command {
    /// Start the mate server in the foreground
    Server,
    /// List pending/in-flight requests
    List,
    /// Cancel a pending request
    Cancel {
        /// The request ID to cancel
        #[facet(args::positional)]
        request_id: String,
    },
    /// Show full task details for a request
    Show {
        /// The request ID to show
        #[facet(args::positional)]
        request_id: String,
    },
    /// Capture and show the mate pane for a request
    Spy {
        /// The request ID to spy on
        #[facet(args::positional)]
        request_id: String,
    },
    /// Steer a mate on an in-flight request (reads from stdin)
    Steer {
        /// The request ID to steer
        #[facet(args::positional)]
        request_id: String,
    },
    /// Send a progress update to the captain (reads from stdin)
    Update {
        /// The request ID to update
        #[facet(args::positional)]
        request_id: String,
    },
    /// Accept a completed task (captain-only)
    Accept {
        /// The request ID to accept
        #[facet(args::positional)]
        request_id: String,
    },
    /// Sync GitHub issues for the current repo and write them to disk
    Issues,
    /// Compact the captain's context (reads summary from stdin)
    Compact,
    /// Assign a task to another agent (reads from stdin)
    Assign {
        /// Keep the worker's existing context (default: clear it)
        #[facet(args::named)]
        keep: bool,
        /// Optional short title for the task
        #[facet(args::named)]
        title: Option<String>,
        /// Attach a GitHub issue context by number
        #[facet(args::named)]
        issue: Option<u64>,
    },
    /// Respond to a task (internal/backward-compatible)
    Respond {
        /// The request ID to respond to
        #[facet(args::positional)]
        request_id: String,
    },
    /// Wait for a response with optional timeout
    Wait {
        /// The request ID to wait on
        #[facet(args::positional)]
        request_id: String,
        /// Timeout in seconds (default: 90)
        #[facet(args::named)]
        timeout: Option<u64>,
    },
    /// Watch CI for the current branch and report results to this pane
    Watch,
    /// Internal command used by `mate watch`
    _WatchInner {
        /// tmux pane id to report results to
        #[facet(args::positional)]
        pane: String,
    },
}

const MANUAL: &str = r#"mate - cooperative agents over tmux

USAGE:
    mate                              Show this manual
    mate server                       Start the server (usually auto-started)
    mate list                         List pending/in-flight requests
    mate cancel <id>                  Cancel a pending request
    mate show <id>                    Show full task content for a request
    mate spy <id>                     Peek at mate's pane
    mate accept <id>                  Accept a completed task (captain-only)
    cat <<'EOF' | mate steer <id>     Steer mate on a pending request
    cat <<'EOF' | mate update <id>    Send progress update to captain
    mate wait <id>                    Wait for a response (default 90s timeout)
    mate wait <id> --timeout <secs>   Wait with custom timeout
    mate watch                        Watch latest CI run for current branch
    mate issues                       Sync GitHub issues for current repo
    cat <<'EOF' | mate compact        Compact captain context with stdin summary
    cat <<'EOF' | mate assign                 Assign a task (clears worker context)
    cat <<'EOF' | mate assign --keep          Assign, keeping worker's context
    cat <<'EOF' | mate assign --title "..."   Assign with a title
    cat <<'EOF' | mate assign --issue 42      Assign with GitHub issue context
    cat <<'EOF' | mate respond <id>   Internal/backward-compatible response command

EXAMPLES:
    # Assign a task:
    cat <<'EOF' | mate assign
    Review the error handling in src/server.rs
    EOF

    # Send a progress update:
    cat <<'EOF' | mate update abc12345
    I reviewed it, here's what I found...
    EOF

ENVIRONMENT:
    TMUX_PANE    Automatically set by tmux. Used to identify your pane.
    MATE_SOCKET   Override the server socket path (default: /tmp/mate.sock)
"#;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Args = figue::from_std_args().unwrap();

    // `mate server` initializes its own tracing subscriber (with a different default filter),
    // so avoid calling `init()` twice which would panic.
    if !matches!(args.command, Some(Command::Server)) {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_env("MATE_LOG")
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .init();
    }

    match args.command {
        None => {
            print!("{MANUAL}");
            Ok(())
        }
        Some(Command::Server) => {
            server::run_server(
                socket_path(),
                pid_path(),
                response_root_dir(),
                request_root_dir(),
                log_path(),
            )
            .await
        }
        Some(Command::List) => listing::list_requests().await,
        Some(Command::Cancel { request_id }) => client::cancel_request(&request_id).await,
        Some(Command::Show { request_id }) => requests::show_request(&request_id).await,
        Some(Command::Spy { request_id }) => requests::spy_request(&request_id).await,
        Some(Command::Steer { request_id }) => client::steer_request(&request_id).await,
        Some(Command::Accept { request_id }) => client::accept_request(&request_id).await,
        Some(Command::Update { request_id }) => client::update_request(&request_id).await,
        Some(Command::Issues) => issues::sync_issues(),
        Some(Command::Compact) => requests::compact_context().await,
        Some(Command::Assign { keep, title, issue }) => {
            let pane = std::env::var("TMUX_PANE")
                .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
            let session_name = tmux_session_name_for_pane(&pane).await?;
            let content = read_stdin().await?;
            client::client_assign(pane, session_name, content, !keep, title, issue).await
        }
        Some(Command::Respond { request_id }) => {
            client::validate_request_id(&request_id)?;
            let content = read_stdin().await?;
            let session_name = tmux_session_name().await?;
            client::rpc_respond(&request_id, &session_name, &content).await
        }
        Some(Command::Wait {
            request_id,
            timeout,
        }) => {
            let timeout_secs = timeout.unwrap_or(90);
            client::wait_for_response(&request_id, timeout_secs).await
        }
        Some(Command::Watch) => watch::watch_ci().await,
        Some(Command::_WatchInner { pane }) => watch::watch_ci_inner(&pane).await,
    }
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
