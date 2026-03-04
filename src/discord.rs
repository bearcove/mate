use eyre::Result;

pub async fn notify(webhook_url: &str, message: &str, attachment: Option<&str>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut form = reqwest::multipart::Form::new()
        .text("payload_json", format!("{{\"content\":\"{}\"}}", escape_json(message)));
    if let Some(content) = attachment {
        let part = reqwest::multipart::Part::bytes(content.as_bytes().to_vec())
            .file_name("pane.txt")
            .mime_str("text/plain")?;
        form = form.part("files[0]", part);
    }
    client
        .post(webhook_url)
        .multipart(form)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

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
