use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpStream,
};

const CLIENT_MAX_FILE_BYTES: u64 = 200_000;

fn client_arg_str<'a>(args: &'a JsonValue, key: &str) -> Option<&'a str> {
    args.as_object()?.get(key)?.as_str()
}

fn client_arg_u64(args: &JsonValue, key: &str) -> Option<u64> {
    args.as_object()?.get(key)?.as_u64()
}

fn client_display_path(path: &std::path::Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn client_expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("~"))
            .join(rest)
    } else {
        PathBuf::from(path)
    }
}

fn client_file_entry_json(path: &std::path::Path) -> JsonValue {
    let meta = fs::metadata(path).ok();
    serde_json::json!({
        "path": client_display_path(path),
        "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
        "size_bytes": meta.as_ref().map(|m| m.len()).unwrap_or(0),
    })
}

fn client_walk_files(root: &std::path::Path, out: &mut Vec<PathBuf>, limit: usize) -> Result<()> {
    if out.len() >= limit {
        return Ok(());
    }
    for entry in fs::read_dir(root).with_context(|| format!("read directory {}", root.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        let meta = fs::metadata(&path)?;
        if meta.is_dir() {
            client_walk_files(&path, out, limit)?;
        } else if meta.is_file() {
            out.push(path);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(())
}

fn execute_client_file_tool(name: &str, args: JsonValue) -> JsonValue {
    match execute_client_file_tool_result(name, args) {
        Ok(value) => value,
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

fn execute_client_file_tool_result(name: &str, args: JsonValue) -> Result<JsonValue> {
    match name {
        "file_list" => {
            let base = client_expand_home(client_arg_str(&args, "path").unwrap_or("."));
            let mut entries = Vec::new();
            if let Some(pattern) = client_arg_str(&args, "glob") {
                let pattern_path = base.join(pattern);
                for entry in glob::glob(&pattern_path.to_string_lossy())?.take(500) {
                    entries.push(client_file_entry_json(&entry?));
                }
            } else {
                for entry in fs::read_dir(&base).with_context(|| format!("read directory {}", base.display()))?.take(500) {
                    entries.push(client_file_entry_json(&entry?.path()));
                }
            }
            Ok(serde_json::json!({ "path": client_display_path(&base), "entries": entries }))
        }
        "file_read" => {
            let rel = client_arg_str(&args, "path").ok_or_else(|| anyhow!("missing required path"))?;
            let path = client_expand_home(rel);
            let meta = fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
            if !meta.is_file() {
                anyhow::bail!("path is not a file");
            }
            if meta.len() > CLIENT_MAX_FILE_BYTES {
                anyhow::bail!("file exceeds max byte limit ({})", CLIENT_MAX_FILE_BYTES);
            }
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            let start = client_arg_u64(&args, "start_line").unwrap_or(1).max(1) as usize;
            let end = client_arg_u64(&args, "end_line").map(|n| n as usize).unwrap_or(usize::MAX);
            let lines: Vec<JsonValue> = text.lines().enumerate().filter_map(|(idx, line)| {
                let line_no = idx + 1;
                (line_no >= start && line_no <= end).then(|| serde_json::json!({ "line": line_no, "text": line }))
            }).collect();
            Ok(serde_json::json!({ "path": client_display_path(&path), "start_line": start, "end_line": end.min(text.lines().count()), "lines": lines }))
        }
        "file_search" => {
            let query = client_arg_str(&args, "query").ok_or_else(|| anyhow!("missing required query"))?;
            let re = regex::RegexBuilder::new(query).case_insensitive(true).build().ok();
            let base = client_expand_home(client_arg_str(&args, "path").unwrap_or("."));
            let mut files = Vec::new();
            if let Some(pattern) = client_arg_str(&args, "glob") {
                let pattern_path = base.join(pattern);
                for entry in glob::glob(&pattern_path.to_string_lossy())?.take(1000) {
                    let path = entry?;
                    if fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false) {
                        files.push(path);
                    }
                }
            } else {
                client_walk_files(&base, &mut files, 1000)?;
            }
            let mut matches = Vec::new();
            for path in files {
                if matches.len() >= 200 { break; }
                let meta = fs::metadata(&path)?;
                if meta.len() > CLIENT_MAX_FILE_BYTES { continue; }
                let Ok(text) = fs::read_to_string(&path) else { continue; };
                for (idx, line) in text.lines().enumerate() {
                    let is_match = re.as_ref().map(|r| r.is_match(line)).unwrap_or_else(|| line.to_ascii_lowercase().contains(&query.to_ascii_lowercase()));
                    if is_match {
                        matches.push(serde_json::json!({ "path": client_display_path(&path), "line": idx + 1, "text": line }));
                        if matches.len() >= 200 { break; }
                    }
                }
            }
            Ok(serde_json::json!({ "query": query, "path": client_display_path(&base), "matches": matches }))
        }
        other => Ok(serde_json::json!({ "error": format!("unsupported client tool: {other}") })),
    }
}

async fn connect_server(
    tx: UnboundedSender<AppEvent>,
    server_addr: &str,
) -> Result<UnboundedSender<ClientRequest>> {
    let stream = TcpStream::connect(server_addr).await?;
    let (read_half, mut write_half) = stream.into_split();

    let (req_tx, mut req_rx) = unbounded_channel::<ClientRequest>();
    let req_tx_for_tools = req_tx.clone();
    tokio::spawn(async move {
        while let Some(req) = req_rx.recv().await {
            match serde_json::to_string(&req) {
                Ok(s) => {
                    if write_half.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    tokio::spawn(async move {
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    let _ = tx.send(AppEvent::OllamaError("Server disconnected".to_string()));
                    break;
                }
                Ok(_) => {
                    if let Ok(ev) = serde_json::from_str::<ServerEvent>(line.trim_end()) {
                        match ev {
                            ServerEvent::ClientToolRequest { tool_call_id, name, args, .. } => {
                                let req_tx2 = req_tx_for_tools.clone();
                                tokio::task::spawn_blocking(move || {
                                    let result = execute_client_file_tool(&name, args);
                                    let _ = req_tx2.send(ClientRequest::ClientToolResult { tool_call_id, result });
                                });
                            }
                            other => {
                                let _ = tx.send(AppEvent::ServerEvent(other));
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::OllamaError(format!("Server read error: {}", e)));
                    break;
                }
            }
        }
    });

    Ok(req_tx)
}
