use eyre::Result;
use rand::prelude::IndexedRandom;
use std::process::Command;

const EMOJI_POOL: &[&str] = &[
    "🌵", "🍄", "🦊", "🐙", "🎯", "🔮", "🧊", "🪐", "🦑", "🎪", "🌋", "🦎", "🪸", "🧿",
    "🫧", "🪬", "🐚", "🦩", "🪻", "🧲", "🪩", "🦠", "🫎", "🪼", "🐋", "🦚", "🪷", "🧬",
];

fn generate_marker() -> String {
    let mut rng = rand::rng();
    let picked: Vec<&str> = EMOJI_POOL.choose_multiple(&mut rng, 3).copied().collect();
    picked.join("")
}

pub struct Pane {
    pub id: String,
    pub title: String,
    pub command: String,
}

/// List all tmux panes in the current session.
pub fn list_panes() -> Result<Vec<Pane>> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}\t#{pane_title}\t#{pane_current_command}"])
        .output()?;

    if !output.status.success() {
        return Err(eyre::eyre!("tmux list-panes failed"));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let panes = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let id = parts.next()?.to_string();
            let title = parts.next()?.to_string();
            let command = parts.next()?.to_string();
            Some(Pane { id, title, command })
        })
        .collect();

    Ok(panes)
}

/// Capture the visible content of a tmux pane.
fn capture_pane(pane_id: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", pane_id, "-p"])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Wait until the pane shows either our emoji marker (small pastes) or a paste
/// indicator from Claude Code / Codex (large pastes).
fn wait_for_paste(pane_id: &str, marker: &str) -> Result<()> {
    for _ in 0..100 {
        let content = capture_pane(pane_id)?;
        if content.contains(marker)
            || content.contains("[Pasted text ")
            || content.contains("[Pasted Content ")
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // Timed out — send C-m anyway, best effort
    Ok(())
}

/// Send text to a tmux pane. Prepends a unique emoji marker, waits for it to
/// appear on screen, then submits with C-m.
pub fn send_to_pane(pane_id: &str, text: &str) -> Result<()> {
    let marker = generate_marker();
    let tagged = format!("{marker} {text}\n{marker}");

    // Clear any existing input (C-u kills the line without interrupting the process)
    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-u"])
        .status()?;
    if !status.success() {
        return Err(eyre::eyre!("tmux send-keys (C-u) failed for pane {pane_id}"));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "-l", &tagged])
        .status()?;
    if !status.success() {
        return Err(eyre::eyre!("tmux send-keys (text) failed for pane {pane_id}"));
    }

    // Wait for our emoji marker or a paste indicator to appear
    wait_for_paste(pane_id, &marker)?;
    // Let the terminal settle after paste lands
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Submit with C-m (carriage return)
    let status = Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "C-m"])
        .status()?;
    if !status.success() {
        return Err(eyre::eyre!("tmux send-keys (C-m) failed for pane {pane_id}"));
    }

    Ok(())
}

/// Find a pane that is NOT the given pane_id (i.e., find the "other" agent).
pub fn find_other_pane(my_pane_id: &str) -> Result<Pane> {
    let panes = list_panes()?;
    panes
        .into_iter()
        .find(|p| p.id != my_pane_id)
        .ok_or_else(|| eyre::eyre!("no other tmux pane found"))
}
