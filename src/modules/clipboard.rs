async fn read_clipboard_text() -> Result<String> {
    let try1 = Command::new("xclip")
        .args(["-selection", "clipboard", "-o", "-t", "text/plain"])
        .output()
        .await;

    let out = match try1 {
        Ok(o) if o.status.success() => o.stdout,
        _ => {
            let o2 = Command::new("xclip")
                .args(["-selection", "clipboard", "-o"])
                .output()
                .await
                .with_context(|| "xclip -o fallback failed")?;
            if !o2.status.success() {
                return Err(anyhow!("xclip returned non-zero status"));
            }
            o2.stdout
        }
    };

    Ok(String::from_utf8_lossy(&out).to_string())
}

async fn write_clipboard_text(s: &str) -> Result<()> {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard", "-i", "-t", "text/plain"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .with_context(|| "spawn xclip -i failed")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(s.as_bytes()).await?;
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow!("xclip copy failed"));
    }
    Ok(())
}

