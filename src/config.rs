use std::path::PathBuf;

fn config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/bud/config.ini"))
}

pub fn discord_webhook_url() -> Option<String> {
    let path = config_path()?;
    let config = ini::Ini::load_from_file(path).ok()?;
    let section = config.section(Some("discord"))?;
    let webhook_url = section.get("webhook_url")?.trim();
    if webhook_url.is_empty() {
        return None;
    }
    Some(webhook_url.to_string())
}
