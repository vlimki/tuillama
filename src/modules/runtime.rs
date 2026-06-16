#[tokio::main]
async fn main() -> Result<()> {
    let cfg_path = config_path()?;
    let cfg = load_config(&cfg_path).unwrap_or_default();
    let model = std::env::var("OLLAMA_MODEL")
        .ok()
        .or_else(|| cfg.model.clone())
        .unwrap_or_else(|| "llama3".to_string());
    let api_url = std::env::var("OLLAMA_CHAT_URL")
        .ok()
        .or_else(|| cfg.api_url.clone())
        .unwrap_or_else(|| "http://localhost:11434/api/chat".to_string());
    let options: Option<JsonValue> = cfg.options.as_ref().and_then(|t| serde_json::to_value(t).ok());
    let ollama_api_key = std::env::var("OLLAMA_API_KEY")
        .ok()
        .or_else(|| cfg.ollama_api_key.clone());
    let web_search = false;
    let system_prompt = cfg.system_prompt.clone();
    let bold_selection = cfg.bold_selection.unwrap_or(false);
    let render_emojis = cfg.render_emojis.unwrap_or(true);
    let theme = Theme::from_config(cfg.colors.as_ref());

    let (syntax_enabled, syntax_theme_name, syntax_custom) = {
        let s = cfg.syntax.clone();
        let enabled = s.as_ref().and_then(|x| x.enabled).unwrap_or(true);
        let theme_name = s
            .as_ref()
            .and_then(|x| x.theme_name.clone())
            .or(cfg.syntax_theme.clone())
            .unwrap_or_else(|| "base16-ocean.dark".to_string());
        let custom = s.and_then(|x| x.custom);
        (enabled, theme_name, custom)
    };

    let (stream_throttle, input_poll_ms) = {
        let p = cfg.performance.clone().unwrap_or_default();
        let fps = p.stream_fps.unwrap_or(30).max(1);
        let poll = p.input_poll_ms.unwrap_or(250).max(1);
        (Duration::from_millis(1000 / fps as u64), poll)
    };

    let (preview_fmt, preview_open) = {
        let p = cfg.preview.clone().unwrap_or_default();
        let fmt = p.preview_fmt.unwrap_or_else(|| "html".to_string());
        let open = p.preview_open.unwrap_or_else(|| "xdg-open".to_string());
        (fmt, open)
    };

    let server_addr = std::env::var("TUILLAMA_SERVER_ADDR")
        .ok()
        .or_else(|| cfg.server_addr.clone())
        .unwrap_or_else(|| "127.0.0.1:7878".to_string());

    let input_poll_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(input_poll_ms));

    let mut app = App::new(
        model,
        api_url,
        options,
        ollama_api_key,
        server_addr.clone(),
        input_poll_ms.clone(),
        web_search,
        system_prompt,
        bold_selection,
        render_emojis,
        theme,
        syntax_enabled,
        syntax_theme_name,
        syntax_custom,
        stream_throttle,
        preview_fmt,
        preview_open,
    );

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, SetCursorStyle::SteadyBar)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx): (UnboundedSender<AppEvent>, UnboundedReceiver<AppEvent>) = unbounded_channel();
    let server_tx = connect_server(tx.clone(), &server_addr).await?;

    // Blocking input reader thread with configurable poll period
    let tx_input = tx.clone();
    std::thread::spawn(move || loop {
        let poll_ms = input_poll_ms.load(std::sync::atomic::Ordering::Relaxed).max(1);
        if event::poll(Duration::from_millis(poll_ms)).unwrap_or(false) {
            match event::read() {
                Ok(CEvent::Key(key)) => {
                    let _ = tx_input.send(AppEvent::Input(key));
                }
                Ok(CEvent::Resize(_, _)) => {
                    let _ = tx_input.send(AppEvent::Resize);
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
    });

    // Initial draw
    terminal.draw(|f| draw_ui(f, &mut app))?;
    app.last_draw = Instant::now();

    // Event-driven UI loop with streaming redraw throttle
    while let Some(ev) = rx.recv().await {
        let mut should_draw = true;

        match ev {
            AppEvent::Input(key) => handle_key(key, &mut app, &tx, &server_tx).await?,
            AppEvent::Resize => {
                app.render_cache.clear();
                app.pending_cache = None;
                terminal.clear()?;
            }
            AppEvent::RefreshScreen => {
                app.render_cache.clear();
                app.pending_cache = None;
                terminal.clear()?;
                app.status_message = Some("Screen refreshed".to_string());
            }
            AppEvent::OllamaError(e) => {
                app.messages.push(Message {
                    role: Role::System,
                    content: format!("Error: {}", e),
                    created_ts: now_sec(),
                    thinking: None,
                    attachments: Vec::new(),
                    sources: Vec::new(),
                });
                app.render_cache.clear();
            }
            AppEvent::ServerEvent(sev) => match sev {
                ServerEvent::Chunk {
                    request_id,
                    chat_id,
                    delta,
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            active.buffer.push_str(&delta);
                            active.generated_tokens += estimate_token_count(&delta);
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.pending_assistant.push_str(&delta);
                                app.pending_cache = None;
                                if app.chat_at_bottom {
                                    app.scroll_to_bottom_on_draw = true;
                                }
                                if app.last_draw.elapsed() < app.stream_throttle {
                                    should_draw = false;
                                }
                            }
                        }
                    }
                }
                ServerEvent::Done {
                    request_id,
                    chat_id,
                    status,
                } => {
                    let matches_active = app
                        .active_streams
                        .get(&chat_id)
                        .map(|a| a.request_id == request_id)
                        .unwrap_or(false);
                    if matches_active {
                        let active = app
                            .active_streams
                            .remove(&chat_id)
                            .unwrap_or(ActiveStream {
                                request_id: request_id.clone(),
                                buffer: String::new(),
                                thinking: String::new(),
                                sources: Vec::new(),
                                status: None,
                                started_at: Instant::now(),
                                generated_tokens: 0,
                            });
                        let elapsed_ms = active.started_at.elapsed().as_millis().max(1);
                        app.last_stream_tokens = active.generated_tokens;
                        app.last_stream_ms = elapsed_ms;
                        app.last_stream_tps = Some((active.generated_tokens as f64) / (elapsed_ms as f64 / 1000.0));
                        let content = active.buffer;
                        let thinking = (!active.thinking.is_empty()).then_some(active.thinking);
                        let has_partial = !content.is_empty() || thinking.is_some();
                        let cancelled = status.as_deref() == Some("cancelled");

                        if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                            if has_partial {
                                app.messages.push(Message {
                                    role: Role::Assistant,
                                    content: content.clone(),
                                    created_ts: now_sec(),
                                    thinking,
                                    attachments: Vec::new(),
                                    sources: active.sources.clone(),
                                });
                                if matches!(app.mode, Mode::Visual) && app.selected_msg.is_none() {
                                    app.selected_msg = Some(app.messages.len().saturating_sub(1));
                                }
                                persist_current_chat(&mut app)?;
                            } else {
                                refresh_current_stream_state(&mut app);
                            }
                            if cancelled {
                                app.status_message = Some("Stream cancelled".to_string());
                            }
                            if app.chat_at_bottom {
                                app.scroll_to_bottom_on_draw = true;
                            }
                        } else if has_partial {
                            if let Err(e) = append_assistant_to_chat(&chat_id, content, thinking, active.sources.clone()) {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Error: {}", e),
                                    created_ts: now_sec(),
                                    thinking: None,
                                    attachments: Vec::new(),
                                    sources: Vec::new(),
                                });
                            }
                            refresh_sidebar_chats(&mut app, None);
                        }
                    }
                }
                ServerEvent::Error {
                    request_id,
                    chat_id,
                    message,
                } => {
                    let matches_active = app
                        .active_streams
                        .get(&chat_id)
                        .map(|a| a.request_id == request_id)
                        .unwrap_or(false);
                    if matches_active {
                        app.active_streams.remove(&chat_id);
                        let formatted = format!("Error: {}", message);
                        if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                            refresh_current_stream_state(&mut app);
                            app.messages.push(Message {
                                role: Role::System,
                                content: formatted,
                                created_ts: now_sec(),
                                thinking: None,
                                attachments: Vec::new(),
                                sources: Vec::new(),
                            });
                        } else {
                            if let Err(e) = append_system_to_chat(&chat_id, formatted) {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Error: {}", e),
                                    created_ts: now_sec(),
                                    thinking: None,
                                    attachments: Vec::new(),
                                    sources: Vec::new(),
                                });
                            }
                            refresh_sidebar_chats(&mut app, None);
                        }
                    }
                }
                ServerEvent::Thinking {
                    request_id,
                    chat_id,
                    delta,
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            active.thinking.push_str(&delta);
                            active.generated_tokens += estimate_token_count(&delta);
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.pending_thinking.push_str(&delta);
                                if app.chat_at_bottom {
                                    app.scroll_to_bottom_on_draw = true;
                                }
                                if app.last_draw.elapsed() < app.stream_throttle {
                                    should_draw = false;
                                }
                            }
                        }
                    }
                }
                ServerEvent::Status {
                    request_id,
                    chat_id,
                    message,
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            active.status = Some(message.clone());
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.status_message = Some(message);
                            }
                        }
                    }
                }
                ServerEvent::Source {
                    request_id,
                    chat_id,
                    title,
                    url,
                    snippet,
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id && !active.sources.iter().any(|s| s.url == url) {
                            active.sources.push(Source { title, url, snippet });
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.pending_sources = active.sources.clone();
                            }
                        }
                    }
                }
                ServerEvent::ToolCallStarted {
                    request_id,
                    chat_id,
                    name,
                    ..
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            let message = format!("Running tool {name}");
                            active.status = Some(message.clone());
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.status_message = Some(message);
                            }
                        }
                    }
                }
                ServerEvent::ToolCallFinished {
                    request_id,
                    chat_id,
                    result_summary,
                    ..
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            let message = format!("Tool finished: {result_summary}");
                            active.status = Some(message.clone());
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.status_message = Some(message);
                            }
                        }
                    }
                }
                ServerEvent::ToolCallFailed {
                    request_id,
                    chat_id,
                    error,
                    ..
                } => {
                    if let Some(active) = app.active_streams.get_mut(&chat_id) {
                        if active.request_id == request_id {
                            let message = format!("Tool failed: {error}");
                            active.status = Some(message.clone());
                            if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                                app.status_message = Some(message);
                            }
                        }
                    }
                }
                ServerEvent::ClientToolRequest { .. } => {}
            },
        }

        if should_draw {
            terminal.draw(|f| draw_ui(f, &mut app))?;
            app.last_draw = Instant::now();
        }

        if app.quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    let mut stdout2 = std::io::stdout();
    execute!(stdout2, SetCursorStyle::DefaultUserShape)?;
    terminal.show_cursor()?;
    Ok(())
}



fn estimate_token_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn normalize_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

fn refresh_current_stream_state(app: &mut App) {
    let Some(chat_id) = app.current_chat_id.as_ref() else {
        app.pending_request_id = None;
        app.pending_assistant.clear();
        app.pending_thinking.clear();
        app.pending_sources.clear();
        app.status_message = None;
        app.sending = false;
        app.pending_cache = None;
        return;
    };

    if let Some(active) = app.active_streams.get(chat_id) {
        app.pending_request_id = Some(active.request_id.clone());
        app.pending_assistant = active.buffer.clone();
        app.pending_thinking = active.thinking.clone();
        app.pending_sources = active.sources.clone();
        app.status_message = active.status.clone();
        app.sending = true;
    } else {
        app.pending_request_id = None;
        app.pending_assistant.clear();
        app.pending_thinking.clear();
        app.pending_sources.clear();
        app.status_message = None;
        app.sending = false;
    }
    app.pending_cache = None;
}

fn cancel_current_stream(
    app: &mut App,
    server_tx: &UnboundedSender<ClientRequest>,
) -> Result<bool> {
    let Some(chat_id) = app.current_chat_id.clone() else {
        return Ok(false);
    };
    let Some(active) = app.active_streams.get(&chat_id) else {
        return Ok(false);
    };
    server_tx
        .send(ClientRequest::CancelStream {
            request_id: active.request_id.clone(),
            chat_id,
        })
        .map_err(|_| anyhow!("background server is unavailable"))?;
    Ok(true)
}

fn append_assistant_to_chat(chat_id: &str, content: String, thinking: Option<String>, sources: Vec<Source>) -> Result<()> {
    let mut chat = match load_chat(chat_id) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    chat.messages.push(Message {
        role: Role::Assistant,
        content,
        created_ts: now_sec(),
        thinking,
        attachments: Vec::new(),
        sources,
    });
    chat.updated_ts = now_sec();
    save_chat(&chat)
}

fn append_system_to_chat(chat_id: &str, content: String) -> Result<()> {
    let mut chat = match load_chat(chat_id) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    chat.messages.push(Message {
        role: Role::System,
        content,
        created_ts: now_sec(),
        thinking: None,
        attachments: Vec::new(),
        sources: Vec::new(),
    });
    chat.updated_ts = now_sec();
    save_chat(&chat)
}

fn now_sec() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

fn attachment_from_path(path_text: &str) -> Result<Attachment> {
    let expanded = if let Some(rest) = path_text.strip_prefix("~/") {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("~"))
            .join(rest)
    } else {
        PathBuf::from(path_text)
    };
    let data = fs::read(&expanded).with_context(|| format!("read attachment {}", expanded.display()))?;
    if data.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err(anyhow!("attachment exceeds max size of {} bytes", MAX_ATTACHMENT_BYTES));
    }
    let mime_type = sniff_mime_type(&expanded, &data);
    let sha256 = format!("{:x}", Sha256::digest(&data));
    let store_dir = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve data dir"))?
        .data_local_dir()
        .join("attachments");
    fs::create_dir_all(&store_dir).with_context(|| format!("create {}", store_dir.display()))?;
    let store_path = store_dir.join(&sha256);
    if !store_path.exists() {
        fs::write(&store_path, &data).with_context(|| format!("write {}", store_path.display()))?;
    }
    Ok(Attachment {
        id: sha256.chars().take(16).collect(),
        original_path: expanded.display().to_string(),
        mime_type,
        sha256,
        size_bytes: data.len() as u64,
        store_path: store_path.display().to_string(),
        data_base64: None,
    })
}

fn execute_command_palette(app: &mut App) {
    let command = app.command_input.trim().trim_start_matches(':').trim().to_string();
    if command.is_empty() {
        app.status_message = Some("Command cancelled".to_string());
        return;
    }

    if let Some(args) = command.strip_prefix("set ").map(str::trim) {
        match execute_set_command(app, args) {
            Ok(message) => app.status_message = Some(message),
            Err(e) => app.status_message = Some(format!("Set failed: {e}")),
        }
        return;
    }

    if let Some(path) = command.strip_prefix("attach ").map(str::trim).filter(|p| !p.is_empty()) {
        match attachment_from_path(path) {
            Ok(attachment) => {
                let name = attachment.original_path.clone();
                app.pending_attachments.push(attachment);
                app.status_message = Some(format!("Attached file {name}"));
            }
            Err(e) => {
                app.messages.push(Message {
                    role: Role::System,
                    content: format!("Attachment failed: {e}"),
                    created_ts: now_sec(),
                    thinking: None,
                    attachments: Vec::new(),
                    sources: Vec::new(),
                });
                app.render_cache.clear();
            }
        }
        return;
    }

    match command.as_str() {
        "web on" => {
            app.web_search = true;
            app.status_message = Some("Web search enabled".to_string());
        }
        _ => {
            app.status_message = Some(format!("Unknown command: :{command}"));
        }
    }
}

fn execute_set_command(app: &mut App, args: &str) -> Result<String> {
    let (key, value_text) = parse_set_args(args)?;
    let key = normalize_set_key(&key);

    match key.as_str() {
        "model" => app.model = value_text.to_string(),
        "api_url" => app.api_url = value_text.to_string(),
        "ollama_api_key" => app.ollama_api_key = parse_optional_string(value_text),
        "server_addr" => app.server_addr = value_text.to_string(),
        "system_prompt" => app.system_prompt = parse_optional_string(value_text),
        "bold_selection" => app.bold_selection = parse_bool(value_text)?,
        "render_emojis" => {
            app.render_emojis = parse_bool(value_text)?;
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax_theme" | "syntax.theme_name" => {
            app.syntax_theme_name = value_text.to_string();
            app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax.enabled" => {
            app.syntax_enabled = parse_bool(value_text)?;
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "performance.stream_fps" => {
            let fps = parse_u64(value_text)?.max(1);
            app.stream_throttle = Duration::from_millis(1000 / fps);
        }
        "performance.input_poll_ms" => {
            app.input_poll_ms.store(
                parse_u64(value_text)?.max(1),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        "preview.preview_fmt" => app.preview_fmt = value_text.to_string(),
        "preview.preview_open" => app.preview_open = value_text.to_string(),
        _ => {
            if let Some(color_key) = key.strip_prefix("colors.").or_else(|| theme_color_key(&key)) {
                app.theme.set_color(color_key, value_text)?;
                app.render_cache.clear();
                app.pending_cache = None;
            } else if let Some(custom_key) = key.strip_prefix("syntax.custom.") {
                let color = parse_color_name(value_text)
                    .ok_or_else(|| anyhow!("invalid color '{value_text}' for syntax.custom.{custom_key}"))?;
                let custom = app.syntax_custom.get_or_insert_with(toml::value::Table::new);
                custom.insert(custom_key.to_string(), toml::Value::String(value_text.to_string()));
                let _ = color;
                app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
                app.render_cache.clear();
                app.pending_cache = None;
            } else {
                let option_key = key.strip_prefix("options.").unwrap_or(&key);
                set_option_value(app, option_key, parse_jsonish_value(value_text));
            }
        }
    }

    Ok(format!("Set {key} = {value_text}"))
}

fn parse_set_args(args: &str) -> Result<(String, &str)> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("usage: :set <option> <value>"));
    }
    if let Some((key, value)) = trimmed.split_once('=') {
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            return Err(anyhow!("usage: :set <option> <value>"));
        }
        return Ok((key.to_string(), value));
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let key = parts.next().unwrap_or_default().trim();
    let value = parts.next().unwrap_or_default().trim();
    if key.is_empty() || value.is_empty() {
        return Err(anyhow!("usage: :set <option> <value>"));
    }
    Ok((key.to_string(), value))
}

fn normalize_set_key(key: &str) -> String {
    key.trim().trim_start_matches(':').replace('-', "_")
}

fn parse_optional_string(value: &str) -> Option<String> {
    match value.trim() {
        "" | "none" | "null" | "off" => None,
        other => Some(other.to_string()),
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(anyhow!("expected boolean true/false, on/off, yes/no, or 1/0")),
    }
}

fn parse_u64(value: &str) -> Result<u64> {
    value.trim().parse::<u64>().with_context(|| format!("invalid integer '{value}'"))
}

fn parse_jsonish_value(value: &str) -> JsonValue {
    let trimmed = value.trim();
    if let Ok(parsed) = serde_json::from_str::<JsonValue>(trimmed) {
        return parsed;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" => JsonValue::Bool(true),
        "false" | "off" | "no" => JsonValue::Bool(false),
        "null" | "none" => JsonValue::Null,
        _ => {
            if let Ok(i) = trimmed.parse::<i64>() {
                JsonValue::Number(i.into())
            } else if let Ok(f) = trimmed.parse::<f64>() {
                serde_json::Number::from_f64(f)
                    .map(JsonValue::Number)
                    .unwrap_or_else(|| JsonValue::String(trimmed.to_string()))
            } else {
                JsonValue::String(trimmed.to_string())
            }
        }
    }
}

fn set_option_value(app: &mut App, option_key: &str, value: JsonValue) {
    let options = app
        .options
        .get_or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
    if !options.is_object() {
        *options = JsonValue::Object(serde_json::Map::new());
    }
    if let Some(map) = options.as_object_mut() {
        if value.is_null() {
            map.remove(option_key);
        } else {
            map.insert(option_key.to_string(), value);
        }
    }
}

fn command_byte_index(input: &str, col: usize) -> usize {
    UnicodeSegmentation::graphemes(input, true)
        .take(col)
        .map(str::len)
        .sum()
}

fn sniff_mime_type(path: &std::path::Path, data: &[u8]) -> String {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png".to_string();
    }
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg".to_string();
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif".to_string();
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return "image/webp".to_string();
    }
    mime_type_for_path(path)
}

fn mime_type_for_path(path: &std::path::Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" => "text/plain",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn message_to_preview_markdown(message: &Message) -> String {
    let mut out = message.content.clone();
    if !message.sources.is_empty() {
        out.push_str("\n\n## Sources\n");
        for source in &message.sources {
            if source.title.is_empty() {
                out.push_str(&format!("- {}\n", source.url));
            } else {
                out.push_str(&format!("- [{}]({})\n", source.title, source.url));
            }
        }
    }
    out
}


fn selected_message_code_blocks(app: &App) -> Vec<String> {
    app.selected_msg
        .and_then(|idx| app.messages.get(idx))
        .map(|message| markdown_code_blocks(&message.content))
        .unwrap_or_default()
}

fn select_next_code_block(app: &mut App) {
    if app.focus != Focus::Chat {
        return;
    }
    let blocks = selected_message_code_blocks(app);
    if blocks.is_empty() {
        app.visual_selection = VisualSelection::Message;
        app.status_message = Some("No code blocks in selected message".to_string());
        return;
    }
    let next = match app.visual_selection {
        VisualSelection::Message => 0,
        VisualSelection::CodeBlock(idx) => (idx + 1) % blocks.len(),
    };
    app.visual_selection = VisualSelection::CodeBlock(next);
    app.status_message = Some(format!(
        "Selected code block {} of {}",
        next + 1,
        blocks.len()
    ));
}

fn push_clipboard_error(app: &mut App, action: &str, e: anyhow::Error) {
    app.messages.push(Message {
        role: Role::System,
        content: format!("Clipboard {} failed: {}", action, e),
        created_ts: now_sec(),
        thinking: None,
        attachments: Vec::new(),
        sources: Vec::new(),
    });
    app.render_cache.clear();
}

async fn yank_visual_selection(app: &mut App) {
    if app.focus != Focus::Chat {
        return;
    }

    let Some(message_idx) = app.selected_msg else {
        return;
    };
    let Some(message) = app.messages.get(message_idx) else {
        return;
    };

    let (contents, label) = match app.visual_selection {
        VisualSelection::Message => (message.content.clone(), "message".to_string()),
        VisualSelection::CodeBlock(code_block_idx) => {
            let blocks = markdown_code_blocks(&message.content);
            let Some(code) = blocks.get(code_block_idx).cloned() else {
                app.visual_selection = VisualSelection::Message;
                app.status_message = Some("Selected code block no longer exists".to_string());
                return;
            };
            (code, format!("code block {}", code_block_idx + 1))
        }
    };

    match write_clipboard_text(&contents).await {
        Ok(()) => {
            app.status_message = Some(format!("Yanked {} to clipboard", label));
            app.mode = Mode::Normal;
        }
        Err(e) => push_clipboard_error(app, "copy", e),
    }
}

fn refresh_sidebar_chats(app: &mut App, preferred_id: Option<&str>) {
    app.chats = if app.sidebar_search_query.trim().is_empty() {
        list_chats().unwrap_or_default()
    } else {
        search_chats(&app.sidebar_search_query).unwrap_or_default()
    };

    if let Some(id) = preferred_id {
        if let Some(idx) = app.chats.iter().position(|c| c.id == id) {
            app.sidebar_idx = idx;
            return;
        }
    }
    if app.sidebar_idx >= app.chats.len() {
        app.sidebar_idx = app.chats.len().saturating_sub(1);
    }
}

fn update_sidebar_search(app: &mut App, query: String) {
    let selected_id = app.chats.get(app.sidebar_idx).map(|c| c.id.clone());
    app.sidebar_search_query = query;
    refresh_sidebar_chats(app, selected_id.as_deref());
}

fn start_new_chat(app: &mut App) -> Result<()> {
    let now = now_sec();
    let id = gen_chat_id();
    app.current_chat_id = Some(id.clone());
    app.current_created_ts = Some(now);
    app.messages.clear();
    app.pending_assistant.clear();
    app.pending_thinking.clear();
    app.status_message = None;
    app.pending_request_id = None;
    app.sending = false;
    app.selected_msg = None;
    app.chat_scroll = 0;
    app.chat_inner_height = 0;
    app.chat_inner_width = 0;
    app.scroll_to_bottom_on_draw = false;
    app.chat_at_bottom = true;
    app.render_cache.clear();
    app.pending_cache = None;
    app.pending_attachments.clear();
    let chat = Chat {
        id: id.clone(),
        title: "Untitled chat".to_string(),
        created_ts: now,
        updated_ts: now,
        messages: Vec::new(),
    };
    save_chat(&chat)?;
    refresh_sidebar_chats(app, Some(&id));
    refresh_current_stream_state(app);
    Ok(())
}

fn delete_prev_input_grapheme(app: &mut App) {
    if app.input_cursor_line == 0 && app.input_cursor_col == 0 {
        return;
    }

    let mut lines: Vec<String> = app.input.split('\n').map(|s| s.to_string()).collect();
    if app.input_cursor_col > 0 {
        let cur = &mut lines[app.input_cursor_line];
        let grs: Vec<&str> = UnicodeSegmentation::graphemes(cur.as_str(), true).collect();
        let mut start_byte = 0usize;
        for i in 0..app.input_cursor_col - 1 {
            start_byte += grs[i].len();
        }
        let end_byte = start_byte + grs[app.input_cursor_col - 1].len();
        cur.replace_range(start_byte..end_byte, "");
        app.input_cursor_col -= 1;
    } else {
        let prev_len_gr =
            UnicodeSegmentation::graphemes(lines[app.input_cursor_line - 1].as_str(), true).count();
        let cur_line = lines.remove(app.input_cursor_line);
        let prev = &mut lines[app.input_cursor_line - 1];
        prev.push_str(&cur_line);
        app.input_cursor_line -= 1;
        app.input_cursor_col = prev_len_gr;
    }
    app.input = lines.join("\n");
}

fn stop_following_stream(app: &mut App) {
    app.scroll_to_bottom_on_draw = false;
    app.chat_at_bottom = false;
}

fn persist_current_chat(app: &mut App) -> Result<()> {
    let now = now_sec();
    if app.current_chat_id.is_none() {
        let id = gen_chat_id();
        app.current_chat_id = Some(id);
        app.current_created_ts = Some(now);
    }
    let id = app.current_chat_id.clone().unwrap();
    let created = app.current_created_ts.unwrap_or(now);
    let title = derive_title(&app.messages);
    let chat = Chat {
        id: id.clone(),
        title,
        created_ts: created,
        updated_ts: now,
        messages: app.messages.clone(),
    };
    save_chat(&chat)?;
    refresh_sidebar_chats(app, Some(&id));
    refresh_current_stream_state(app);
    Ok(())
}

async fn handle_key(
    key: KeyEvent,
    app: &mut App,
    tx: &UnboundedSender<AppEvent>,
    server_tx: &UnboundedSender<ClientRequest>,
) -> Result<()> {
    // Popups
    match &app.popup {
        Popup::ConfirmDelete { id, .. } => match key.code {
            KeyCode::Char('y') => {
                let del_id = id.clone();
                delete_chat_file(&del_id)?;
                app.active_streams.remove(&del_id);
                if app.current_chat_id.as_deref() == Some(&del_id) {
                    app.current_chat_id = None;
                    app.current_created_ts = None;
                    app.messages.clear();
                    app.selected_msg = None;
                    app.chat_scroll = 0;
                    app.render_cache.clear();
                    refresh_current_stream_state(app);
                }
                app.popup = Popup::None;
                refresh_sidebar_chats(app, None);
                return Ok(());
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                app.popup = Popup::None;
                return Ok(());
            }
            _ => {
                return Ok(());
            }
        },
        Popup::Help => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => {
                    app.popup = Popup::None;
                }
                _ => {}
            }
            return Ok(());
        }
        Popup::None => {}
    }

    // Global exits
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return Ok(());
    }
    if key.code == KeyCode::Char('q') && app.mode == Mode::Normal {
        app.quit = true;
        return Ok(());
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('x')) {
        let _ = cancel_current_stream(app, server_tx)?;
        return Ok(());
    }

    // Global: toggle sidebar visibility with Ctrl+T
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('t')) {
        app.show_sidebar = !app.show_sidebar;
        if app.show_sidebar {
            app.focus = Focus::Sidebar; // auto-focus Chats when showing
            app.mode = Mode::Normal;
        } else {
            app.focus = Focus::Chat;
        }
        return Ok(());
    }

    // Global: toggle web search with Ctrl+W
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('w')) {
        app.web_search = !app.web_search;
        return Ok(());
    }

    // Global: toggle stats panel with Ctrl+P
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('p')) {
        app.show_stats_panel = !app.show_stats_panel;
        return Ok(());
    }

    // Global: toggle thinking visibility with Ctrl+Y
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('y')) {
        app.show_thinking = !app.show_thinking;
        return Ok(());
    }

    if app.sidebar_search_active {
        match key.code {
            KeyCode::Esc => {
                app.sidebar_search_active = false;
                update_sidebar_search(app, String::new());
            }
            KeyCode::Enter => {
                app.sidebar_search_active = false;
            }
            KeyCode::Backspace => {
                let mut query = app.sidebar_search_query.clone();
                query.pop();
                update_sidebar_search(app, query);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                let mut query = app.sidebar_search_query.clone();
                query.push(c);
                update_sidebar_search(app, query);
            }
            _ => {}
        }
        return Ok(());
    }

    if key.code == KeyCode::Esc
        && app.focus == Focus::Sidebar
        && !app.sidebar_search_query.is_empty()
    {
        update_sidebar_search(app, String::new());
        return Ok(());
    }

    match app.mode {
        Mode::Insert => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
            }
            // Send with Ctrl+S
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let input = app.input.trim().to_string();
                if input.is_empty() && app.pending_attachments.is_empty() {
                    return Ok(());
                }

                if let Some(chat_id) = app.current_chat_id.as_ref() {
                    if app.active_streams.contains_key(chat_id) {
                        return Ok(());
                    }
                }

                app.input.clear();
                app.input_cursor_line = 0;
                app.input_cursor_col = 0;
                app.input_top_line = 0;

                let attachments = std::mem::take(&mut app.pending_attachments);
                app.messages.push(Message {
                    role: Role::User,
                    content: if input.is_empty() { "[image]".to_string() } else { input },
                    created_ts: now_sec(),
                    thinking: None,
                    attachments,
                    sources: Vec::new(),
                });
                app.render_cache.remove(&(app.messages.len() - 1, app.chat_inner_height));
                persist_current_chat(app)?;
                let chat_id = app
                    .current_chat_id
                    .clone()
                    .ok_or_else(|| anyhow!("missing current chat id after persist"))?;

                let mut convo = app.messages.clone();
                convo.retain(|m| !matches!(m.role, Role::System) || !m.content.starts_with("Error:"));
                if !convo.iter().any(|m| matches!(m.role, Role::System)) {
                    if let Some(sp) = app.system_prompt.clone() {
                        convo.insert(
                            0,
                            Message {
                                role: Role::System,
                                content: sp,
                                created_ts: now_sec(),
                                thinking: None,
                                attachments: Vec::new(),
                                sources: Vec::new(),
                            },
                        );
                    }
                }

                let request_id = gen_chat_id();
                let req = ClientRequest::StartStream {
                    request_id: request_id.clone(),
                    chat_id: chat_id.clone(),
                    created_ts: app.current_created_ts.unwrap_or_else(now_sec),
                    api_url: app.api_url.clone(),
                    model: app.model.clone(),
                    options: app.options.clone(),
                    ollama_api_key: app.ollama_api_key.clone(),
                    web_search: app.web_search,
                    messages: convo,
                };
                app.active_streams.insert(
                    chat_id,
                    ActiveStream {
                        request_id: request_id.clone(),
                        buffer: String::new(),
                        thinking: String::new(),
                        sources: Vec::new(),
                        status: None,
                        started_at: Instant::now(),
                        generated_tokens: 0,
                    },
                );
                refresh_current_stream_state(app);
                if app.chat_at_bottom {
                    app.scroll_to_bottom_on_draw = true;
                }
                if server_tx.send(req).is_err() {
                    app.active_streams.retain(|_, v| v.request_id != request_id);
                    refresh_current_stream_state(app);
                    app.messages.push(Message {
                        role: Role::System,
                        content: "Error: background server is unavailable".to_string(),
                        created_ts: now_sec(),
                        thinking: None,
                        attachments: Vec::new(),
                        sources: Vec::new(),
                    });
                }
            }
            // Enter inserts newline (multiline)
            KeyCode::Enter => {
                let lines: Vec<&str> = app.input.split('\n').collect();
                let cur = lines.get(app.input_cursor_line).copied().unwrap_or("");
                // byte index within current line
                let mut col_byte = 0usize;
                for (i, g) in UnicodeSegmentation::graphemes(cur, true).enumerate() {
                    if i >= app.input_cursor_col {
                        break;
                    }
                    col_byte += g.len();
                }
                // byte index in whole string at cursor
                let mut byte_cursor = 0usize;
                for li in 0..app.input_cursor_line {
                    byte_cursor += lines[li].len() + 1; // include '\n'
                }
                byte_cursor += col_byte;

                app.input.insert(byte_cursor, '\n');
                app.input_cursor_line += 1;
                app.input_cursor_col = 0;
                // input_top_line adjust with headroom (handled in draw)
            }
            KeyCode::Backspace => {
                delete_prev_input_grapheme(app);
            }
            KeyCode::Left => {
                if app.input_cursor_col > 0 {
                    app.input_cursor_col -= 1;
                } else if app.input_cursor_line > 0 {
                    // move to end of previous line
                    app.input_cursor_line -= 1;
                    let prev = app.input.split('\n').nth(app.input_cursor_line).unwrap_or("");
                    app.input_cursor_col = UnicodeSegmentation::graphemes(prev, true).count();
                }
            }
            KeyCode::Right => {
                let cur = app.input.split('\n').nth(app.input_cursor_line).unwrap_or("");
                let cur_len = UnicodeSegmentation::graphemes(cur, true).count();
                if app.input_cursor_col < cur_len {
                    app.input_cursor_col += 1;
                } else if app.input_cursor_line + 1 < app.input.split('\n').count() {
                    app.input_cursor_line += 1;
                    app.input_cursor_col = 0;
                }
            }
            KeyCode::Up => {
                if app.input_cursor_line > 0 {
                    app.input_cursor_line -= 1;
                    // clamp column to target line length
                    let tgt = app.input.split('\n').nth(app.input_cursor_line).unwrap_or("");
                    let len = UnicodeSegmentation::graphemes(tgt, true).count();
                    if app.input_cursor_col > len {
                        app.input_cursor_col = len;
                    }
                }
            }
            KeyCode::Down => {
                let total = app.input.split('\n').count();
                if app.input_cursor_line + 1 < total {
                    app.input_cursor_line += 1;
                    let tgt = app.input.split('\n').nth(app.input_cursor_line).unwrap_or("");
                    let len = UnicodeSegmentation::graphemes(tgt, true).count();
                    if app.input_cursor_col > len {
                        app.input_cursor_col = len;
                    }
                }
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                delete_prev_input_grapheme(app);
            }
            KeyCode::Char(c) => {
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    return Ok(());
                }

                // insert at cursor
                let lines: Vec<&str> = app.input.split('\n').collect();
                let cur = lines.get(app.input_cursor_line).copied().unwrap_or("");
                let mut col_byte = 0usize;
                for (i, g) in UnicodeSegmentation::graphemes(cur, true).enumerate() {
                    if i >= app.input_cursor_col {
                        break;
                    }
                    col_byte += g.len();
                }
                let mut byte_cursor = 0usize;
                for li in 0..app.input_cursor_line {
                    byte_cursor += lines[li].len() + 1;
                }
                byte_cursor += col_byte;

                if c == '\t' {
                    app.input.insert_str(byte_cursor, "    ");
                    app.input_cursor_col += 4;
                } else {
                    app.input.insert(byte_cursor, c);
                    app.input_cursor_col += 1;
                }
            }
            KeyCode::Tab => {
                let lines: Vec<&str> = app.input.split('\n').collect();
                let cur = lines.get(app.input_cursor_line).copied().unwrap_or("");
                let mut col_byte = 0usize;
                for (i, g) in UnicodeSegmentation::graphemes(cur, true).enumerate() {
                    if i >= app.input_cursor_col {
                        break;
                    }
                    col_byte += g.len();
                }
                let mut byte_cursor = 0usize;
                for li in 0..app.input_cursor_line {
                    byte_cursor += lines[li].len() + 1;
                }
                byte_cursor += col_byte;

                app.input.insert_str(byte_cursor, "    ");
                app.input_cursor_col += 4;
            }
            // manual chat scroll while typing
            KeyCode::PageUp => {
                stop_following_stream(app);
                app.chat_scroll = app.chat_scroll.saturating_sub(app.chat_inner_height.max(1));
            }
            KeyCode::PageDown => {
                app.chat_scroll = app.chat_scroll.saturating_add(app.chat_inner_height.max(1));
            }
            _ => {}
        },
        Mode::Command => match key.code {
            KeyCode::Esc => {
                app.command_input.clear();
                app.command_cursor_col = 0;
                app.mode = Mode::Normal;
                app.status_message = Some("Command cancelled".to_string());
            }
            KeyCode::Enter => {
                execute_command_palette(app);
                app.command_input.clear();
                app.command_cursor_col = 0;
                app.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                if app.command_cursor_col > 0 {
                    let start = command_byte_index(&app.command_input, app.command_cursor_col - 1);
                    let end = command_byte_index(&app.command_input, app.command_cursor_col);
                    app.command_input.replace_range(start..end, "");
                    app.command_cursor_col -= 1;
                }
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if app.command_cursor_col > 0 {
                    let start = command_byte_index(&app.command_input, app.command_cursor_col - 1);
                    let end = command_byte_index(&app.command_input, app.command_cursor_col);
                    app.command_input.replace_range(start..end, "");
                    app.command_cursor_col -= 1;
                }
            }
            KeyCode::Left => {
                app.command_cursor_col = app.command_cursor_col.saturating_sub(1);
            }
            KeyCode::Right => {
                let len = UnicodeSegmentation::graphemes(app.command_input.as_str(), true).count();
                app.command_cursor_col = (app.command_cursor_col + 1).min(len);
            }
            KeyCode::Char(c) => {
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                {
                    return Ok(());
                }
                let byte_cursor = command_byte_index(&app.command_input, app.command_cursor_col);
                app.command_input.insert(byte_cursor, c);
                app.command_cursor_col += 1;
            }
            _ => {}
        },
        Mode::Normal => {
            if let KeyCode::Char('?') = key.code {
                app.popup = Popup::Help;
                return Ok(());
            }

            match key.code {
                KeyCode::Char(':') => {
                    app.command_input.clear();
                    app.command_cursor_col = 0;
                    app.mode = Mode::Command;
                    app.status_message = Some("Command mode".to_string());
                }
                KeyCode::Char('/') => {
                    if app.focus == Focus::Sidebar {
                        app.sidebar_search_active = true;
                    }
                }
                KeyCode::Char('r') => {
                    let _ = tx.send(AppEvent::RefreshScreen);
                }
                KeyCode::Char('h') => {
                    if app.show_sidebar {
                        app.focus = Focus::Sidebar;
                    }
                }
                KeyCode::Char('l') => {
                    app.focus = Focus::Chat;
                }
                KeyCode::Char('v') => {
                    if app.focus == Focus::Chat {
                        app.mode = Mode::Visual;
                        app.visual_selection = VisualSelection::Message;
                        if app.selected_msg.is_none() {
                            app.selected_msg = Some(app.messages.len().saturating_sub(1));
                        }
                    }
                }
                KeyCode::Char('i') => {
                    if app.focus == Focus::Chat {
                        app.mode = Mode::Insert;
                        // place cursor at end of input
                        let lines: Vec<&str> = app.input.split('\n').collect();
                        app.input_cursor_line = lines.len().saturating_sub(1);
                        let last = lines.last().copied().unwrap_or("");
                        app.input_cursor_col = UnicodeSegmentation::graphemes(last, true).count();
                    }
                }
                KeyCode::Char('n') => {
                    start_new_chat(app)?;
                }
                KeyCode::Char('d') => {
                    if app.focus == Focus::Sidebar && !app.chats.is_empty() {
                        let meta = &app.chats[app.sidebar_idx];
                        app.popup = Popup::ConfirmDelete {
                            id: meta.id.clone(),
                            title: meta.title.clone(),
                        };
                    }
                }
                // Paste appends to current input (not overwrite)
                KeyCode::Char('p') => {
                    if app.focus == Focus::Chat {
                        match read_clipboard_text().await {
                            Ok(clip) => {
                                // append at end, but expand tabs so the terminal layout stays stable
                                app.input.push_str(&normalize_tabs(&clip));
                                let lines: Vec<&str> = app.input.split('\n').collect();
                                app.input_cursor_line = lines.len().saturating_sub(1);
                                let last = lines.last().copied().unwrap_or("");
                                app.input_cursor_col = UnicodeSegmentation::graphemes(last, true).count();
                            }
                            Err(e) => {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Clipboard paste failed: {}", e),
                                    created_ts: now_sec(),
                                    thinking: None,
                                    attachments: Vec::new(),
                                    sources: Vec::new(),
                                });
                                app.render_cache.clear();
                            }
                        }
                    }
                }
                KeyCode::Char('j') => match app.focus {
                    Focus::Sidebar => {
                        if app.show_sidebar && !app.chats.is_empty() {
                            app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len() - 1);
                        }
                    }
                    Focus::Chat => {
                        app.chat_scroll = app.chat_scroll.saturating_add(1);
                    }
                },
                KeyCode::Char('k') => match app.focus {
                    Focus::Sidebar => {
                        if app.show_sidebar && !app.chats.is_empty() {
                            app.sidebar_idx = app.sidebar_idx.saturating_sub(1);
                        }
                    }
                    Focus::Chat => {
                        stop_following_stream(app);
                        app.chat_scroll = app.chat_scroll.saturating_sub(1);
                    }
                },
                KeyCode::Char('g') => {
                    if app.focus == Focus::Chat {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            app.scroll_to_bottom_on_draw = true;
                        } else {
                            stop_following_stream(app);
                            app.chat_scroll = 0;
                        }
                    }
                }
                KeyCode::Char('G') => {
                    if app.focus == Focus::Chat {
                        app.scroll_to_bottom_on_draw = true;
                    }
                }
                KeyCode::Enter => match app.focus {
                    Focus::Sidebar => {
                        if app.show_sidebar && !app.chats.is_empty() {
                            let meta = &app.chats[app.sidebar_idx];
                            if let Ok(chat) = load_chat(&meta.id) {
                                app.current_chat_id = Some(chat.id.clone());
                                app.current_created_ts = Some(chat.created_ts);
                                app.messages = chat.messages;
                                app.render_cache.clear();
                                refresh_current_stream_state(app);
                                app.scroll_to_bottom_on_draw = true;
                                app.focus = Focus::Chat;
                            }
                        }
                    }
                    Focus::Chat => {}
                },
                KeyCode::Up => match app.focus {
                    Focus::Sidebar => {
                        if app.show_sidebar && !app.chats.is_empty() {
                            app.sidebar_idx = app.sidebar_idx.saturating_sub(1);
                        }
                    }
                    Focus::Chat => {
                        stop_following_stream(app);
                        app.chat_scroll = app.chat_scroll.saturating_sub(1);
                    }
                },
                KeyCode::Down => match app.focus {
                    Focus::Sidebar => {
                        if app.show_sidebar && !app.chats.is_empty() {
                            app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len() - 1);
                        }
                    }
                    Focus::Chat => {
                        app.chat_scroll = app.chat_scroll.saturating_add(1);
                    }
                },
                _ => {}
            }
        }
        Mode::Visual => match key.code {
            KeyCode::Esc | KeyCode::Char('v') => {
                app.mode = Mode::Normal;
                app.visual_selection = VisualSelection::Message;
            }
            KeyCode::Char('h') => {
                if app.show_sidebar {
                    app.focus = Focus::Sidebar;
                }
            }
            KeyCode::Char('l') => {
                app.focus = Focus::Chat;
            }
            KeyCode::Char('i') => {
                if app.focus == Focus::Chat {
                    app.mode = Mode::Insert;
                    let lines: Vec<&str> = app.input.split('\n').collect();
                    app.input_cursor_line = lines.len().saturating_sub(1);
                    let last = lines.last().copied().unwrap_or("");
                    app.input_cursor_col = UnicodeSegmentation::graphemes(last, true).count();
                }
            }
            KeyCode::Char('y') => {
                yank_visual_selection(app).await;
            }
            KeyCode::Char('c') => {
                select_next_code_block(app);
            }
            KeyCode::Char('m') => {
                app.visual_selection = VisualSelection::Message;
                app.status_message = Some("Selected whole message".to_string());
            }
            KeyCode::Char('j') => match app.focus {
                Focus::Sidebar => {
                    if app.show_sidebar && !app.chats.is_empty() {
                        app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len() - 1);
                    }
                }
                Focus::Chat => {
                    if app.messages.is_empty() {
                        return Ok(());
                    }
                    let cur = app
                        .selected_msg
                        .unwrap_or_else(|| app.messages.len().saturating_sub(1));
                    let next = (cur + 1).min(app.messages.len().saturating_sub(1));
                    app.selected_msg = Some(next);
                    app.visual_selection = VisualSelection::Message;
                    let target_y = offset_for_message(app, app.chat_inner_width.max(1), next);
                    let top = app.chat_scroll;
                    let bottom = top.saturating_add(app.chat_inner_height.max(1));
                    if target_y < top || target_y >= bottom {
                        app.chat_scroll = target_y;
                    }
                }
            },
            KeyCode::Char('k') => match app.focus {
                Focus::Sidebar => {
                    if app.show_sidebar && !app.chats.is_empty() {
                        app.sidebar_idx = app.sidebar_idx.saturating_sub(1);
                    }
                }
                Focus::Chat => {
                    if app.messages.is_empty() {
                        return Ok(());
                    }
                    let cur = app
                        .selected_msg
                        .unwrap_or_else(|| app.messages.len().saturating_sub(1));
                    let next = cur.saturating_sub(1);
                    app.selected_msg = Some(next);
                    app.visual_selection = VisualSelection::Message;
                    let target_y = offset_for_message(app, app.chat_inner_width.max(1), next);
                    let top = app.chat_scroll;
                    let bottom = top.saturating_add(app.chat_inner_height.max(1));
                    if target_y < top || target_y >= bottom {
                        stop_following_stream(app);
                        app.chat_scroll = target_y;
                    }
                }
            },
            KeyCode::Enter => match app.focus {
                Focus::Sidebar => {
                    if app.show_sidebar && !app.chats.is_empty() {
                        let meta = &app.chats[app.sidebar_idx];
                        if let Ok(chat) = load_chat(&meta.id) {
                            app.current_chat_id = Some(chat.id.clone());
                            app.current_created_ts = Some(chat.created_ts);
                            app.messages = chat.messages;
                            app.render_cache.clear();
                            refresh_current_stream_state(app);
                            app.selected_msg = Some(app.messages.len().saturating_sub(1));
                            app.chat_scroll =
                                offset_for_message(app, app.chat_inner_width.max(1), app.selected_msg.unwrap_or(0));
                            app.focus = Focus::Chat;
                        }
                    }
                }
                Focus::Chat => {
                    if let Some(i) = app.selected_msg {
                        if let Some(m) = app.messages.get(i) {
                            let tx2 = tx.clone();
                            let content = message_to_preview_markdown(m);
                            let fmt = app.preview_fmt.clone();
                            let opener = app.preview_open.clone();
                            tokio::spawn(async move {
                                if let Err(e) = preview_to_html_or_pdf(content, &fmt, &opener).await {
                                    let _ = tx2.send(AppEvent::OllamaError(format!(
                                        "Preview error: {}",
                                        e
                                    )));
                                }
                            });
                        }
                    }
                }
            },
            KeyCode::Up => match app.focus {
                Focus::Sidebar => {
                    if app.show_sidebar && !app.chats.is_empty() {
                        app.sidebar_idx = app.sidebar_idx.saturating_sub(1);
                    }
                }
                Focus::Chat => {
                    stop_following_stream(app);
                    app.chat_scroll = app.chat_scroll.saturating_sub(1);
                }
            },
            KeyCode::Down => match app.focus {
                Focus::Sidebar => {
                    if app.show_sidebar && !app.chats.is_empty() {
                        app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len() - 1);
                    }
                }
                Focus::Chat => {
                    app.chat_scroll = app.chat_scroll.saturating_add(1);
                }
            },
            _ => {}
        },
    }
    Ok(())
}
