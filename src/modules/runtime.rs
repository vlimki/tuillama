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
    let color_config = cfg.colors.clone().unwrap_or_default();
    let theme = Theme::from_config(Some(&color_config));

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
        (Duration::from_millis(1000 / fps as u64), Arc::new(AtomicU64::new(poll)))
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

    let mut app = App::new(
        model,
        api_url,
        server_addr.clone(),
        options,
        ollama_api_key,
        web_search,
        system_prompt,
        bold_selection,
        render_emojis,
        color_config,
        theme,
        syntax_enabled,
        syntax_theme_name,
        syntax_custom.clone(),
        stream_throttle,
        input_poll_ms.clone(),
        preview_fmt,
        preview_open,
    );

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, SetCursorStyle::SteadyBar)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx): (UnboundedSender<AppEvent>, UnboundedReceiver<AppEvent>) = unbounded_channel();
    let mut server_tx = connect_server(tx.clone(), &server_addr).await?;

    // Blocking input reader thread with configurable poll period
    let tx_input = tx.clone();
    std::thread::spawn(move || loop {
        let poll_ms = input_poll_ms.load(Ordering::Relaxed).max(1);
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
            AppEvent::Input(key) => handle_key(key, &mut app, &tx, &mut server_tx).await?,
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
    let mime_type = mime_type_for_path(&expanded);
    if !mime_type.starts_with("image/") {
        return Err(anyhow!("only image attachments are supported for now"));
    }
    Ok(Attachment {
        path: expanded.display().to_string(),
        mime_type,
        data_base64: Some(general_purpose::STANDARD.encode(data)),
    })
}

async fn execute_command_palette(
    app: &mut App,
    tx: &UnboundedSender<AppEvent>,
    server_tx: &mut UnboundedSender<ClientRequest>,
) -> Result<()> {
    let command = app.command_input.trim().trim_start_matches(':').trim().to_string();
    if command.is_empty() {
        app.status_message = Some("Command cancelled".to_string());
        return Ok(());
    }

    if let Some(path) = command.strip_prefix("attach ").map(str::trim).filter(|p| !p.is_empty()) {
        match attachment_from_path(path) {
            Ok(attachment) => {
                let name = attachment.path.clone();
                app.pending_attachments.push(attachment);
                app.status_message = Some(format!("Attached image {name}"));
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
        return Ok(());
    }

    if let Some(rest) = command.strip_prefix("set ").map(str::trim) {
        execute_set_command(app, tx, server_tx, rest).await?;
        return Ok(());
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
    Ok(())
}

async fn execute_set_command(
    app: &mut App,
    tx: &UnboundedSender<AppEvent>,
    server_tx: &mut UnboundedSender<ClientRequest>,
    rest: &str,
) -> Result<()> {
    let Some((key, raw_value)) = split_set_command(rest) else {
        app.status_message = Some("Usage: :set <config-key> <value>".to_string());
        return Ok(());
    };
    let key = key.trim().trim_start_matches('.');
    let value = raw_value.trim();
    if value.is_empty() {
        app.status_message = Some(format!("Missing value for {key}"));
        return Ok(());
    }

    match key {
        "model" => app.model = value.to_string(),
        "api_url" => app.api_url = value.to_string(),
        "ollama_api_key" => app.ollama_api_key = optional_string_value(value),
        "server_addr" => {
            let new_tx = connect_server(tx.clone(), value).await?;
            *server_tx = new_tx;
            app.server_addr = value.to_string();
        }
        "system_prompt" => app.system_prompt = optional_string_value(value),
        "bold_selection" => app.bold_selection = parse_command_bool(value)?,
        "render_emojis" => {
            app.render_emojis = parse_command_bool(value)?;
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "colors" => {
            app.color_config = parse_toml_table_value(value)?;
            app.theme = Theme::from_config(Some(&app.color_config));
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax" => {
            let table = parse_toml_table_value(value)?;
            if let Some(enabled) = table.get("enabled").and_then(|v| v.as_bool()) {
                app.syntax_enabled = enabled;
            }
            if let Some(theme_name) = table.get("theme_name").and_then(|v| v.as_str()) {
                app.syntax_theme_name = theme_name.to_string();
            }
            if let Some(custom) = table.get("custom").and_then(|v| v.as_table()).cloned() {
                app.syntax_custom = Some(custom);
            }
            app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax.enabled" => {
            app.syntax_enabled = parse_command_bool(value)?;
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax.theme_name" | "syntax_theme" => {
            app.syntax_theme_name = value.to_string();
            app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "syntax.custom" => {
            app.syntax_custom = Some(parse_toml_table_value(value)?);
            app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
            app.render_cache.clear();
            app.pending_cache = None;
        }
        "performance.stream_fps" => {
            let fps = value.parse::<u64>().with_context(|| format!("parse {key}"))?.max(1);
            app.stream_throttle = Duration::from_millis(1000 / fps);
        }
        "performance.input_poll_ms" => {
            let poll = value.parse::<u64>().with_context(|| format!("parse {key}"))?.max(1);
            app.input_poll_ms.store(poll, Ordering::Relaxed);
        }
        "performance" => {
            let table = parse_toml_table_value(value)?;
            if let Some(fps) = table.get("stream_fps").and_then(toml_value_as_u64) {
                app.stream_throttle = Duration::from_millis(1000 / fps.max(1));
            }
            if let Some(poll) = table.get("input_poll_ms").and_then(toml_value_as_u64) {
                app.input_poll_ms.store(poll.max(1), Ordering::Relaxed);
            }
        }
        "preview.preview_fmt" => app.preview_fmt = value.to_string(),
        "preview.preview_open" => app.preview_open = value.to_string(),
        "preview" => {
            let table = parse_toml_table_value(value)?;
            if let Some(fmt) = table.get("preview_fmt").and_then(|v| v.as_str()) {
                app.preview_fmt = fmt.to_string();
            }
            if let Some(open) = table.get("preview_open").and_then(|v| v.as_str()) {
                app.preview_open = open.to_string();
            }
        }
        "options" => app.options = Some(parse_options_object(value)?),
        "num_ctx" => set_option_value(app, "num_ctx", parse_config_value(value)),
        _ if key.starts_with("options.") => {
            set_option_value(app, key.trim_start_matches("options."), parse_config_value(value));
        }
        _ if key.starts_with("colors.") => {
            let color_key = key.trim_start_matches("colors.");
            app.color_config.insert(color_key.to_string(), toml::Value::String(value.to_string()));
            app.theme = Theme::from_config(Some(&app.color_config));
            app.render_cache.clear();
            app.pending_cache = None;
        }
        _ if key.starts_with("syntax.custom.") => {
            let custom_key = key.trim_start_matches("syntax.custom.");
            let mut custom = app.syntax_custom.take().unwrap_or_default();
            custom.insert(custom_key.to_string(), parse_toml_scalar_value(value));
            app.syntax_custom = Some(custom);
            app.syn_theme = build_syntect_theme(&app.syntax_theme_name, app.syntax_custom.as_ref());
            app.render_cache.clear();
            app.pending_cache = None;
        }
        _ => {
            app.status_message = Some(format!("Unknown config key: {key}"));
            return Ok(());
        }
    }

    app.status_message = Some(format!("Set {key}"));
    Ok(())
}

fn split_set_command(rest: &str) -> Option<(&str, &str)> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((key, value)) = trimmed.split_once('=') {
        return Some((key.trim(), value.trim()));
    }
    let idx = trimmed.find(char::is_whitespace)?;
    Some((&trimmed[..idx], trimmed[idx..].trim()))
}

fn optional_string_value(value: &str) -> Option<String> {
    match value.trim() {
        "" | "none" | "null" => None,
        other => Some(other.to_string()),
    }
}

fn parse_command_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(anyhow!("expected boolean value, got {value}")),
    }
}

fn parse_toml_table_value(value: &str) -> Result<toml::value::Table> {
    let parsed = toml_value_from_text(value)?;
    parsed
        .as_table()
        .cloned()
        .ok_or_else(|| anyhow!("expected TOML table value"))
}

fn parse_toml_scalar_value(value: &str) -> toml::Value {
    toml_value_from_text(value).unwrap_or_else(|_| toml::Value::String(value.to_string()))
}

fn toml_value_from_text(value: &str) -> Result<toml::Value> {
    let wrapper = format!("value = {value}");
    let table: toml::value::Table = toml::from_str(&wrapper).with_context(|| format!("parse TOML value {value}"))?;
    table
        .get("value")
        .cloned()
        .ok_or_else(|| anyhow!("missing TOML value"))
}

fn parse_options_object(value: &str) -> Result<JsonValue> {
    match parse_config_value(value) {
        JsonValue::Object(map) => Ok(JsonValue::Object(map)),
        other => Err(anyhow!("options must be an object/table, got {other}")),
    }
}

fn parse_config_value(value: &str) -> JsonValue {
    serde_json::from_str(value)
        .or_else(|_| toml_value_from_text(value).and_then(|v| serde_json::to_value(v).map_err(Into::into)))
        .unwrap_or_else(|_| JsonValue::String(value.to_string()))
}

fn set_option_value(app: &mut App, key: &str, value: JsonValue) {
    let options = app.options.get_or_insert_with(|| JsonValue::Object(serde_json::Map::new()));
    if !options.is_object() {
        *options = JsonValue::Object(serde_json::Map::new());
    }
    if let Some(map) = options.as_object_mut() {
        map.insert(key.to_string(), value);
    }
}

fn toml_value_as_u64(value: &toml::Value) -> Option<u64> {
    value.as_integer().and_then(|v| u64::try_from(v).ok())
}

fn command_byte_index(input: &str, col: usize) -> usize {
    UnicodeSegmentation::graphemes(input, true)
        .take(col)
        .map(str::len)
        .sum()
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
    server_tx: &mut UnboundedSender<ClientRequest>,
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
                if let Err(e) = execute_command_palette(app, tx, server_tx).await {
                    app.status_message = Some(format!("Command failed: {e}"));
                }
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
                KeyCode::Up => {
                    stop_following_stream(app);
                    app.chat_scroll = app.chat_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    app.chat_scroll = app.chat_scroll.saturating_add(1);
                }
                _ => {}
            }
        }
        Mode::Visual => match key.code {
            KeyCode::Esc | KeyCode::Char('v') => {
                app.mode = Mode::Normal;
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
                if app.focus == Focus::Chat {
                    if let Some(i) = app.selected_msg {
                        if let Some(m) = app.messages.get(i) {
                            if let Err(e) = write_clipboard_text(&m.content).await {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Clipboard copy failed: {}", e),
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
            KeyCode::Up => {
                stop_following_stream(app);
                app.chat_scroll = app.chat_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.chat_scroll = app.chat_scroll.saturating_add(1);
            }
            _ => {}
        },
    }
    Ok(())
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn splits_set_commands_with_spaces_or_equals() {
        assert_eq!(split_set_command("model qwen3.6:27b"), Some(("model", "qwen3.6:27b")));
        assert_eq!(split_set_command("options.num_ctx = 32768"), Some(("options.num_ctx", "32768")));
    }

    #[test]
    fn parses_config_values_for_set_options() {
        assert_eq!(parse_config_value("32768"), JsonValue::from(32768));
        assert_eq!(parse_config_value("true"), JsonValue::from(true));
        assert_eq!(parse_config_value("qwen3.6:27b"), JsonValue::from("qwen3.6:27b"));
    }
}
