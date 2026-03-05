use eyre::Result;

use crate::pane::{self, Pane};
use crate::{client, paths, tmux, util};

pub(crate) async fn compact_context() -> Result<()> {
    let summary = paths::read_stdin().await?;
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;

    let list_output = tokio::process::Command::new("mate")
        .arg("list")
        .output()
        .await?;
    let task_list = if list_output.status.success() {
        let stdout = String::from_utf8_lossy(&list_output.stdout)
            .trim()
            .to_string();
        if stdout.is_empty() {
            "none".to_string()
        } else {
            stdout
        }
    } else {
        "none".to_string()
    };

    let prompt = format!(
        "/captain\nYou've just been compacted. Here is your context summary from before compaction:\n\n{summary}\n\nIn-flight tasks at time of compaction:\n{task_list}"
    );

    let pane = tmux::TmuxPane::new(pane::PaneId(pane));
    pane.slash_command("/clear").await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    pane.chat_message(&prompt).await?;
    Ok(())
}

pub(crate) async fn show_request(request_id: &str) -> Result<()> {
    client::validate_request_id(request_id)?;
    let session_name = paths::tmux_session_name().await?;
    let path = paths::request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .await
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let content = util::read_request_content(&path)
        .await
        .ok_or_else(|| eyre::eyre!("Task {request_id} is missing request content."))?;
    eprintln!("Task {request_id}");
    eprintln!("Source: {}  Target: {}", meta.source_pane, meta.target_pane);
    eprintln!("Title: {}", meta.title.as_deref().unwrap_or("(none)"));
    eprintln!();
    eprintln!("{content}");
    Ok(())
}

pub(crate) async fn spy_request(request_id: &str) -> Result<()> {
    client::validate_request_id(request_id)?;
    let session_name = paths::tmux_session_name().await?;
    let path = paths::request_dir(&session_name).join(request_id);
    let meta = util::read_request_meta(&path)
        .await
        .ok_or_else(|| eyre::eyre!("No task with ID {request_id} found."))?;
    let pane_content = tmux::capture_pane(&meta.target_pane).await?;
    eprintln!("Pane {}:\n{}", meta.target_pane, pane_content);
    Ok(())
}
