use eyre::Result;

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

pub async fn notify(webhook_url: &str, message: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let payload = format!("{{\"content\":\"{}\"}}", escape_json(message));
    client
        .post(webhook_url)
        .header("content-type", "application/json")
        .body(payload)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
