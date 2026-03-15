fn sanitize_for_html(input: &str) -> String {
    let mut out = input.replace('\u{2011}', "-"); // non-breaking hyphen -> hyphen
    //out = out.replace('<', "").replace('>', "");
    let re_display = Regex::new(r"(?s)\\\[\s*(.*?)\s*\\\]").unwrap();
    out = re_display
        .replace_all(&out, |caps: &Captures| format!("$$\n{}\n$$", &caps[1]))
        .to_string();
    let re_trim_inline = Regex::new(r"\$\s+([^$]*?\S)\s+\$").unwrap();
    out = re_trim_inline
        .replace_all(&out, |caps: &Captures| format!("${}$", &caps[1]))
        .to_string();
    let re_inline = Regex::new(r"\\\((.*?)\\\)").unwrap();
    out = re_inline
        .replace_all(&out, |caps: &Captures| format!("${}$", caps[1].trim()))
        .to_string();
    out
}

async fn preview_to_html_or_pdf(content: String, fmt: &str, opener: &str) -> Result<()> {
    let sanitized = sanitize_for_html(&content);
    let base = std::env::temp_dir();
    let stamp = now_sec();
    let in_path = base.join(format!("tuillama_{}_input.md", stamp));
    let out_path = match fmt {
        "pdf" => base.join(format!("tuillama_{}_output.pdf", stamp)),
        _ => base.join(format!("tuillama_{}_output.html", stamp)),
    };
    tokio::fs::write(&in_path, sanitized).await?;
    let mut cmd = Command::new("pandoc");
    cmd.arg(&in_path).arg("-s").arg("--from").arg("markdown");
    match fmt {
        "pdf" => {
            cmd.arg("--pdf-engine").arg("lualatex").arg("-o").arg(&out_path);
        }
        _ => {
            cmd.arg("--to").arg("html").arg("--mathjax").arg("-o").arg(&out_path);
        }
    }
    let status = cmd.status().await?;
    if !status.success() {
        return Err(anyhow!("pandoc failed"));
    }
    let _ = Command::new(opener)
        .arg(&out_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Ok(())
}

