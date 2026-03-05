use std::time::UNIX_EPOCH;

pub async fn binary_hash() -> String {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return "0".to_string(),
    };

    let metadata = match fs_err::tokio::metadata(exe).await {
        Ok(metadata) => metadata,
        Err(_) => return "0".to_string(),
    };

    let len = metadata.len();
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    format!("{len:x}{modified_nanos:x}")
}
