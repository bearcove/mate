use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RequestMeta {
    pub source_pane: String,
    pub target_pane: String,
    pub title: Option<String>,
}

pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub fn trim_agent_footer<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let mut end = lines.len();
    while end > 0 {
        let line = lines[end - 1];
        if is_agent_footer_line(line) {
            end -= 1;
            continue;
        }
        break;
    }
    &lines[..end]
}

fn is_agent_footer_line(line: &str) -> bool {
    if line.trim().is_empty() {
        return true;
    }

    if line.starts_with("✻ Worked for") {
        return true;
    }

    if line.starts_with("▸▸ bypass permissions") {
        return true;
    }

    if line.contains("tokens") && line.chars().any(|ch| ch.is_ascii_digit()) {
        return true;
    }

    if line.contains("current:") && line.contains("latest:") {
        return true;
    }

    if line.contains("gpt-") || line.contains("claude-") {
        return true;
    }

    if line.starts_with("› Run /") {
        return true;
    }

    if line.contains("· left ·") || line.contains("% left") {
        return true;
    }

    if line.contains("esc to interrupt") {
        return true;
    }

    false
}

pub fn write_request(
    dir: &Path,
    source_pane: &str,
    target_pane: &str,
    title: Option<&str>,
    content: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;

    let meta = match title {
        Some(title) if !title.trim().is_empty() => {
            format!("{source_pane}\n{target_pane}\n{}", title.trim())
        }
        _ => format!("{source_pane}\n{target_pane}"),
    };

    std::fs::write(dir.join("meta"), meta)?;
    std::fs::write(dir.join("content"), content)?;
    Ok(())
}

pub fn read_request_meta(dir: &Path) -> Option<RequestMeta> {
    let content = std::fs::read_to_string(dir.join("meta")).ok()?;
    let mut lines = content.lines();
    let source_pane = lines.next()?.trim().to_string();
    let target_pane = lines.next()?.trim().to_string();
    if source_pane.is_empty() || target_pane.is_empty() {
        return None;
    }
    let title = lines
        .next()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToString::to_string);
    Some(RequestMeta {
        source_pane,
        target_pane,
        title,
    })
}

#[allow(dead_code)] // used by mate retry (coming soon)
pub fn read_request_content(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("content")).ok()
}
