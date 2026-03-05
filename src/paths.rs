use eyre::Result;
use std::path::PathBuf;
use tokio::io::AsyncReadExt as _;

pub(crate) fn socket_path() -> PathBuf {
    std::env::var("MATE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/mate.sock"))
}

pub(crate) fn pid_path() -> PathBuf {
    PathBuf::from("/tmp/mate.pid")
}

pub(crate) fn response_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-responses")
}

pub(crate) fn response_dir(session_name: &str) -> PathBuf {
    response_root_dir().join(session_name)
}

pub(crate) fn request_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-requests")
}

pub(crate) fn request_dir(session_name: &str) -> PathBuf {
    request_root_dir().join(session_name)
}

pub(crate) fn idle_tracking_root_dir() -> PathBuf {
    PathBuf::from("/tmp/mate-idle")
}

pub(crate) fn log_path() -> PathBuf {
    PathBuf::from("/tmp/mate-server.log")
}

pub(crate) async fn tmux_session_name_for_pane(pane: &str) -> Result<String> {
    let output = tokio::process::Command::new("tmux")
        .args(["display-message", "-t", pane, "-p", "#{session_name}"])
        .output()
        .await?;
    if !output.status.success() {
        return Err(eyre::eyre!("tmux display-message failed for pane {pane}"));
    }
    let session_name = String::from_utf8(output.stdout)?.trim().to_string();
    if session_name.is_empty() {
        return Err(eyre::eyre!("tmux returned empty session name"));
    }
    Ok(session_name)
}

pub(crate) async fn tmux_session_name() -> Result<String> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
    tmux_session_name_for_pane(&pane).await
}

pub(crate) async fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    let mut stdin = tokio::io::stdin();
    stdin.read_to_string(&mut buf).await?;
    if buf.trim().is_empty() {
        return Err(eyre::eyre!("no input on stdin"));
    }
    Ok(buf)
}
