use std::path::Path;
use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("install") => install(),
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!("Available commands: install");
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: cargo xtask <command>");
            eprintln!("Available commands: install");
            std::process::exit(1);
        }
    }
}

fn install() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask should live under workspace root");

    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "mate"])
        .current_dir(workspace_root)
        .status()
        .expect("Failed to run cargo build");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    let src = workspace_root.join("target/release/mate");
    let home = std::env::var("HOME").expect("HOME not set");
    let bin_dir = Path::new(&home).join(".cargo/bin");
    std::fs::create_dir_all(&bin_dir).expect("Failed to create ~/.cargo/bin");
    let dst = bin_dir.join("mate");

    std::fs::copy(&src, &dst).expect("Failed to copy binary");
    println!("Copied mate to {}", dst.display());

    #[cfg(target_os = "macos")]
    {
        println!("Signing installed binary...");
        let status = Command::new("codesign")
            .arg("--sign")
            .arg("-")
            .arg("--force")
            .arg(&dst)
            .status()
            .expect("Failed to run codesign");
        if !status.success() {
            eprintln!("Warning: codesign failed, continuing anyway");
        }
    }

    println!("Verifying installation...");
    if try_verify_with_version(&dst) {
        return;
    }
    verify_without_args(&dst);
}

fn try_verify_with_version(dst: &Path) -> bool {
    match Command::new(dst).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let printed = version.trim();
            if printed.is_empty() {
                println!("Installed: (version command succeeded)");
            } else {
                println!("Installed: {}", printed);
            }
            true
        }
        Ok(_) => false,
        Err(_) => false,
    }
}

fn verify_without_args(dst: &Path) {
    let output = Command::new(dst)
        .output()
        .expect("Failed to run installed mate binary");
    if !output.status.success() {
        eprintln!("Error: installed binary failed to execute");
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        std::process::exit(1);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        println!(
            "Installed and runnable: {}",
            stdout.lines().next().unwrap_or("")
        );
    } else {
        println!("Installed and runnable");
    }
}
