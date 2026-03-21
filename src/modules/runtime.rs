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

    let mut app = App::new(
        model,
        api_url,
        options,
        ollama_api_key,
        web_search,
        system_prompt,
        bold_selection,
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
    let server_addr = std::env::var("TUILLAMA_SERVER_ADDR")
        .ok()
        .or_else(|| cfg.server_addr.clone())
        .unwrap_or_else(|| "127.0.0.1:7878".to_string());
    let server_tx = connect_server(tx.clone(), &server_addr).await?;

    // Blocking input reader thread with configurable poll period
    let tx_input = tx.clone();
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(input_poll_ms)).unwrap_or(false) {
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
            AppEvent::OllamaError(e) => {
                app.messages.push(Message {
                    role: Role::System,
                    content: format!("Error: {}", e),
                    thinking: None,
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
                ServerEvent::Done { request_id, chat_id } => {
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
                                status: None,
                            });
                        let content = active.buffer;
                        let thinking = (!active.thinking.is_empty()).then_some(active.thinking);

                        if app.current_chat_id.as_deref() == Some(chat_id.as_str()) {
                            app.messages.push(Message {
                                role: Role::Assistant,
                                content: content.clone(),
                                thinking,
                            });
                            if app.chat_at_bottom {
                                app.scroll_to_bottom_on_draw = true;
                            }
                            refresh_current_stream_state(&mut app);
                            persist_current_chat(&mut app)?;
                            if matches!(app.mode, Mode::Visual) && app.selected_msg.is_none() {
                                app.selected_msg = Some(app.messages.len().saturating_sub(1));
                            }
                        } else if !content.is_empty() {
                            if let Err(e) = append_assistant_to_chat(&chat_id, content, thinking) {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Error: {}", e),
                                    thinking: None,
                                });
                            }
                            app.chats = list_chats().unwrap_or_default();
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
                                thinking: None,
                            });
                        } else {
                            if let Err(e) = append_system_to_chat(&chat_id, formatted) {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Error: {}", e),
                                    thinking: None,
                                });
                            }
                            app.chats = list_chats().unwrap_or_default();
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


fn refresh_current_stream_state(app: &mut App) {
    let Some(chat_id) = app.current_chat_id.as_ref() else {
        app.pending_request_id = None;
        app.pending_assistant.clear();
        app.pending_thinking.clear();
        app.status_message = None;
        app.sending = false;
        app.pending_cache = None;
        return;
    };

    if let Some(active) = app.active_streams.get(chat_id) {
        app.pending_request_id = Some(active.request_id.clone());
        app.pending_assistant = active.buffer.clone();
        app.pending_thinking = active.thinking.clone();
        app.status_message = active.status.clone();
        app.sending = true;
    } else {
        app.pending_request_id = None;
        app.pending_assistant.clear();
        app.pending_thinking.clear();
        app.status_message = None;
        app.sending = false;
    }
    app.pending_cache = None;
}

fn append_assistant_to_chat(chat_id: &str, content: String, thinking: Option<String>) -> Result<()> {
    let mut chat = match load_chat(chat_id) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    chat.messages.push(Message {
        role: Role::Assistant,
        content,
        thinking,
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
        thinking: None,
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
    let chat = Chat {
        id: id.clone(),
        title: "Untitled chat".to_string(),
        created_ts: now,
        updated_ts: now,
        messages: Vec::new(),
    };
    save_chat(&chat)?;
    app.chats = list_chats().unwrap_or_default();
    if let Some(idx) = app.chats.iter().position(|c| c.id == id) {
        app.sidebar_idx = idx;
    }
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
    app.chats = list_chats().unwrap_or_default();
    if let Some(idx) = app.chats.iter().position(|c| c.id == id) {
        app.sidebar_idx = idx;
    }
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
                app.chats = list_chats().unwrap_or_default();
                if app.sidebar_idx >= app.chats.len() {
                    app.sidebar_idx = app.chats.len().saturating_sub(1);
                }
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

    // Global: toggle thinking visibility with Ctrl+Y
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('y')) {
        app.show_thinking = !app.show_thinking;
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
                if input.is_empty() {
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

                app.messages.push(Message {
                    role: Role::User,
                    content: input,
                    thinking: None,
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
                                thinking: None,
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
                        status: None,
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
                        thinking: None,
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

                app.input.insert(byte_cursor, c);
                app.input_cursor_col += 1;
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

                app.input.insert(byte_cursor, '\t');
                app.input_cursor_col += 1;
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
        Mode::Normal => {
            if let KeyCode::Char('?') = key.code {
                app.popup = Popup::Help;
                return Ok(());
            }

            match key.code {
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
                                // append at end
                                app.input.push_str(&clip);
                                let lines: Vec<&str> = app.input.split('\n').collect();
                                app.input_cursor_line = lines.len().saturating_sub(1);
                                let last = lines.last().copied().unwrap_or("");
                                app.input_cursor_col = UnicodeSegmentation::graphemes(last, true).count();
                            }
                            Err(e) => {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Clipboard paste failed: {}", e),
                                    thinking: None,
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
                                    thinking: None,
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
                            let content = m.content.clone();
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
