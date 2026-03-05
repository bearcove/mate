use crate::pane;
use eyre::Result;
use rand::prelude::IndexedRandom;

const EMOJI_POOL: &[&str] = &[
    "🌵", "🍄", "🦊", "🐙", "🎯", "🔮", "🧊", "🪐", "🦑", "🎪", "🌋", "🦎", "🪸", "🧿", "🫧", "🪬",
    "🐚", "🦩", "🪻", "🧲", "🪩", "🦠", "🫎", "🪼", "🐋", "🦚", "🪷", "🧬",
];

fn gen_threemoji() -> String {
    let mut rng = rand::rng();
    let picked: Vec<&str> = EMOJI_POOL.choose_multiple(&mut rng, 3).copied().collect();
    picked.join("")
}

fn is_slash_command(text: &str) -> bool {
    let trimmed = text.trim_start();
    !trimmed.is_empty()
        && trimmed.starts_with('/')
        && !trimmed.contains('\n')
        && !trimmed.contains('\r')
}

fn prepare_outgoing_text(text: &str, marker: &str) -> String {
    if is_slash_command(text) {
        return text.to_string();
    }
    format!("{text} {marker}")
}

pub struct Pane {
    pub id: String,
    pub pid: u32,
    pub session_name: String,
}

pub struct TmuxPane {
    id: crate::pane::PaneId,
}

impl TmuxPane {
    pub fn new(id: crate::pane::PaneId) -> Self {
        Self { id }
    }
}

#[async_trait::async_trait]
impl pane::Pane for TmuxPane {
    async fn slash_command(&self, command: &str) -> Result<()> {
        if !is_slash_command(command) {
            return Err(eyre::eyre!("invalid slash command"));
        }
        send_to_pane_exact(self.id.0.as_str(), command).await
    }

    async fn chat_message(&self, message: &str) -> Result<()> {
        send_to_pane(self.id.0.as_str(), message).await
    }

    async fn snapshot(&self) -> Result<pane::PaneState> {
        let capture = capture_pane(self.id.0.as_str()).await?;
        Ok(pane::parse_pane_content(&capture))
    }
}

pub struct TmuxPaneDiscovery;

#[async_trait::async_trait]
impl pane::PaneDiscovery for TmuxPaneDiscovery {
    async fn find_peer(&self, me: &crate::pane::PaneId) -> Result<std::sync::Arc<dyn pane::Pane>> {
        let pane = find_other_pane(&me.0).await?;
        Ok(std::sync::Arc::new(TmuxPane::new(pane::PaneId(pane.id))))
    }

    async fn list_all(&self) -> Result<Vec<pane::DiscoveredPane>> {
        use std::collections::HashSet;

        let panes = list_all_panes().await?;
        let mut discovered: Vec<pane::DiscoveredPane> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for pane in panes {
            if !seen.insert(pane.id.clone()) {
                continue;
            }
            discovered.push(pane::DiscoveredPane {
                info: pane::PaneInfo {
                    id: pane::PaneId(pane.id.clone()),
                    session: pane::SessionName(pane.session_name.clone()),
                },
                pane: std::sync::Arc::new(TmuxPane::new(pane::PaneId(pane.id))),
            });
        }

        Ok(discovered)
    }
}

/// List tmux panes in the same session as the given pane.
pub async fn list_panes(pane_id: &str) -> Result<Vec<Pane>> {
    // Find which session this pane belongs to
    let session_output = tokio::process::Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{session_id}"])
        .output()
        .await?;
    if !session_output.status.success() {
        return Err(eyre::eyre!(
            "tmux display-message failed for pane {pane_id}"
        ));
    }
    let session_id = String::from_utf8(session_output.stdout)?.trim().to_string();

    let output = tokio::process::Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            &session_id,
            "-s",
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{session_name}",
        ])
        .output()
        .await?;

    if !output.status.success() {
        return Err(eyre::eyre!("tmux list-panes failed"));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let panes = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let id = parts.next()?.to_string();
            let pid = parts.next()?.parse().ok()?;
            let session_name = parts.next()?.to_string();
            Some(Pane {
                id,
                pid,
                session_name,
            })
        })
        .collect();

    Ok(panes)
}

/// List all tmux panes across all sessions.
pub async fn list_all_panes() -> Result<Vec<Pane>> {
    let output = tokio::process::Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{pane_pid}\t#{session_name}",
        ])
        .output()
        .await?;
    if !output.status.success() {
        return Err(eyre::eyre!("tmux list-panes -a failed"));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let panes = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let id = parts.next()?.to_string();
            let pid = parts.next()?.parse().ok()?;
            let session_name = parts.next()?.to_string();
            Some(Pane {
                id,
                pid,
                session_name,
            })
        })
        .collect();
    Ok(panes)
}

/// Capture the visible content of a tmux pane.
pub async fn capture_pane(pane_id: &str) -> Result<String> {
    let output = tokio::process::Command::new("tmux")
        .args(["capture-pane", "-t", pane_id, "-p"])
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Wait until the pane shows either our emoji marker (small pastes) or a paste
/// indicator from Claude Code / Codex (large pastes).
async fn wait_for_paste(pane_id: &str, marker: &str) -> Result<()> {
    for _ in 0..100 {
        let content = capture_pane(pane_id).await?;
        if content.contains(marker)
            || content.contains("[Pasted text ")
            || content.contains("[Pasted Content ")
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Timed out — send C-m anyway, best effort
    Ok(())
}

async fn wait_for_exact_text(pane_id: &str, text: &str) -> Result<()> {
    for _ in 0..100 {
        let content = capture_pane(pane_id).await?;
        if content.contains(text) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn send_to_pane_exact(pane_id: &str, text: &str) -> Result<()> {
    // Silently exit copy mode if active (no-op if not in copy mode)
    let _ = tokio::process::Command::new("tmux")
        .args(["copy-mode", "-q", "-t", pane_id])
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    // Clear any existing input (C-u kills the line without interrupting the process)
    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-u"])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (C-u) failed for pane {pane_id}"
        ));
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", text])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (text) failed for pane {pane_id}"
        ));
    }

    wait_for_exact_text(pane_id, text).await?;

    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-m"])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (C-m) failed for pane {pane_id}"
        ));
    }

    Ok(())
}

/// Send text to a tmux pane. Uses a unique emoji marker for paste detection,
/// waits for paste confirmation, then submits with C-m.
pub async fn send_to_pane(pane_id: &str, text: &str) -> Result<()> {
    let threemoji = gen_threemoji();
    let tagged = prepare_outgoing_text(text, &threemoji);
    // marker to wait for before submitting
    let marker = if is_slash_command(text) {
        text
    } else {
        threemoji.as_str()
    };

    send_to_pane_with_marker(pane_id, &tagged, marker).await
}

async fn send_to_pane_with_marker(pane_id: &str, text: &str, marker: &str) -> Result<()> {
    // Silently exit copy mode if active (no-op if not in copy mode)
    let _ = tokio::process::Command::new("tmux")
        .args(["copy-mode", "-q", "-t", pane_id])
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    // Clear any existing input (C-u kills the line without interrupting the process)
    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-u"])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (C-u) failed for pane {pane_id}"
        ));
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", text])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (text) failed for pane {pane_id}"
        ));
    }

    // Wait for our emoji marker or a paste indicator to appear
    wait_for_paste(pane_id, marker).await?;

    // Let the terminal settle after paste lands
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Submit with C-m (carriage return)
    let status = tokio::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-m"])
        .status()
        .await?;
    if !status.success() {
        return Err(eyre::eyre!(
            "tmux send-keys (C-m) failed for pane {pane_id}"
        ));
    }

    Ok(())
}

fn child_process_names(sys: &sysinfo::System, pane: &Pane) -> Vec<String> {
    use sysinfo::Pid;

    let parent = Pid::from_u32(pane.pid);
    let mut names: Vec<String> = sys
        .processes()
        .values()
        .filter(|proc| proc.parent() == Some(parent))
        .filter_map(|proc| proc.name().to_str().map(ToString::to_string))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn is_agent_pane(child_names: &[String]) -> bool {
    child_names
        .iter()
        .any(|name| matches!(name.as_str(), "claude" | "codex"))
}

/// Find a pane in the same session that is running an agent.
pub async fn find_other_pane(my_pane_id: &str) -> Result<Pane> {
    use sysinfo::{ProcessesToUpdate, System};

    let panes = list_panes(my_pane_id).await?;
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut other_panes = 0usize;
    let mut details: Vec<String> = Vec::new();

    for pane in panes {
        if pane.id == my_pane_id {
            continue;
        }
        other_panes += 1;
        let child_names = child_process_names(&sys, &pane);
        if is_agent_pane(&child_names) {
            return Ok(pane);
        }
        if child_names.is_empty() {
            details.push(format!(
                "  {}: child processes: no child processes",
                pane.id
            ));
        } else {
            details.push(format!(
                "  {}: child processes: [{}]",
                pane.id,
                child_names.join(", ")
            ));
        }
    }

    if details.is_empty() {
        return Err(eyre::eyre!(
            "no claude or codex pane found in {other_panes} other panes:\n  (no panes to inspect)\nIs your mate running?"
        ));
    }

    Err(eyre::eyre!(
        "no claude or codex pane found in {other_panes} other panes:\n{}\nIs your mate running?",
        details.join("\n")
    ))
}

#[cfg(test)]
mod tests;
