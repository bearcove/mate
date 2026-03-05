use crate::pane::Pane;
use crate::{pane, tmux};
use eyre::Result;

pub(crate) async fn watch_ci() -> Result<()> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — are you inside tmux?"))?;
    let exe = std::env::current_exe()?;

    tokio::process::Command::new(exe)
        .args(["_watch-inner", &pane])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    eprintln!("Started CI watcher in background for {pane}.");
    Ok(())
}

pub(crate) async fn watch_ci_inner(pane: &str) -> Result<()> {
    match run_watch_ci_inner(pane).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let pane = tmux::TmuxPane::new(pane::PaneId(pane.to_string()));
            let _ = pane
                .chat_message(&format!("❌ CI watch failed: {err}"))
                .await;
            Ok(())
        }
    }
}

async fn run_watch_ci_inner(pane: &str) -> Result<()> {
    let branch = current_branch().await?;
    let run_id = poll_latest_run_id(&branch, std::time::Duration::from_secs(30))
        .await?
        .ok_or_else(|| eyre::eyre!("no CI run found for branch `{branch}` within 30s"))?;

    let watch_status = tokio::process::Command::new("gh")
        .args(["run", "watch", &run_id, "--exit-status"])
        .status()
        .await?;

    let pane = tmux::TmuxPane::new(pane::PaneId(pane.to_string()));
    if watch_status.success() {
        pane.chat_message("✅ CI passed.").await?;
        return Ok(());
    }

    let failed_log_output = tokio::process::Command::new("gh")
        .args(["run", "view", &run_id, "--log-failed"])
        .output()
        .await?;

    let mut summary_lines: Vec<String> = String::from_utf8_lossy(&failed_log_output.stdout)
        .lines()
        .take(50)
        .map(ToString::to_string)
        .collect();

    if summary_lines.is_empty() {
        summary_lines = String::from_utf8_lossy(&failed_log_output.stderr)
            .lines()
            .take(50)
            .map(ToString::to_string)
            .collect();
    }

    let summary = if summary_lines.is_empty() {
        "No failed log output available.".to_string()
    } else {
        summary_lines.join("\n")
    };

    let message = format!("❌ CI failed:\n```\n{summary}\n```");
    pane.chat_message(&message).await?;
    Ok(())
}

async fn current_branch() -> Result<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await?;
    if !output.status.success() {
        return Err(eyre::eyre!("failed to determine current git branch"));
    }
    let branch = String::from_utf8(output.stdout)?.trim().to_string();
    if branch.is_empty() {
        return Err(eyre::eyre!("current git branch is empty"));
    }
    Ok(branch)
}

async fn poll_latest_run_id(branch: &str, timeout: std::time::Duration) -> Result<Option<String>> {
    let started_at = std::time::Instant::now();
    loop {
        if let Some(run_id) = latest_run_id(branch).await? {
            return Ok(Some(run_id));
        }
        if started_at.elapsed() >= timeout {
            return Ok(None);
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn latest_run_id(branch: &str) -> Result<Option<String>> {
    let output = tokio::process::Command::new("gh")
        .args([
            "run",
            "list",
            "--branch",
            branch,
            "--limit",
            "1",
            "--json",
            "databaseId,status",
            "--jq",
            ".[0].databaseId",
        ])
        .output()
        .await?;
    if !output.status.success() {
        return Err(eyre::eyre!("failed to list CI runs with gh"));
    }
    let run_id = String::from_utf8(output.stdout)?.trim().to_string();
    if run_id.is_empty() || run_id == "null" {
        return Ok(None);
    }
    Ok(Some(run_id))
}
