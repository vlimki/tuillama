fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

// Visible substring and cursor x for a line with horizontal scrolling
fn line_view(s: &str, col_gi: usize, maxw: usize) -> (String, usize) {
    let maxw = maxw.max(1);
    let graphemes: Vec<&str> = UnicodeSegmentation::graphemes(s, true).collect();
    let widths: Vec<usize> = graphemes.iter().map(|g| UnicodeWidthStr::width(*g)).collect();

    let mut w_to_cursor = 0usize;
    for i in 0..col_gi.min(widths.len()) {
        w_to_cursor += widths[i];
    }

    let mut start = 0usize;
    let mut w_from_start_to_cursor = w_to_cursor;
    while w_from_start_to_cursor > maxw {
        w_from_start_to_cursor = w_from_start_to_cursor.saturating_sub(widths[start]);
        start += 1;
    }

    let mut out = String::new();
    let mut used = 0usize;
    for i in start..graphemes.len() {
        let w = widths[i];
        if used + w > maxw {
            break;
        }
        out.push_str(graphemes[i]);
        used += w;
    }

    let cursor_x = w_from_start_to_cursor.min(used);
    (out, cursor_x)
}

fn draw_ui(frame: &mut ratatui::Frame, app: &mut App) {
    let constraints = if app.show_sidebar {
        [Constraint::Percentage(30), Constraint::Percentage(70)]
    } else {
        [Constraint::Length(0), Constraint::Percentage(100)]
    };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(frame.size());

    if app.show_sidebar {
        draw_sidebar(frame, chunks[0], app);
    }
    draw_chat(frame, chunks[1], app);

    match &app.popup {
        Popup::ConfirmDelete { id: _, title } => {
            let area = centered_rect(60, 30, frame.size());
            frame.render_widget(Clear, area);
            let block = Block::default()
                .title(Span::styled(
                    "Confirm delete",
                    Style::default()
                        .fg(app.theme.popup_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border_chat));
            let msg = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Delete chat:",
                    Style::default().fg(app.theme.popup_accent),
                )),
                Line::from(Span::styled(
                    format!("  {}", title),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press y to confirm, n to cancel",
                    Style::default().fg(app.theme.popup_text),
                )),
            ];
            let p = Paragraph::new(Text::from(msg))
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(p, area);
        }
        Popup::Help => {
            let area = centered_rect(80, 80, frame.size());
            frame.render_widget(Clear, area);
            let block = Block::default()
                .title(Span::styled(
                    "Keybindings",
                    Style::default()
                        .fg(app.theme.popup_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border_chat));

            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(Span::styled(
                "Global",
                Style::default()
                    .fg(app.theme.heading2)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "? ",
                    Style::default()
                        .fg(app.theme.popup_accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Show/close help (NORMAL mode)"),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Ctrl+T ",
                    Style::default()
                        .fg(app.theme.popup_accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Hide/Show sidebar (chat focus)"),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Ctrl+W ",
                    Style::default()
                        .fg(app.theme.popup_accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Toggle web search"),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Ctrl+Y ",
                    Style::default()
                        .fg(app.theme.popup_accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Hide/Show thinking blocks"),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "q / Ctrl+C ",
                    Style::default()
                        .fg(app.theme.popup_accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("Quit"),
            ]));
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled(
                "Sidebar (FOCUS Sidebar)",
                Style::default()
                    .fg(app.theme.heading2)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(
                "  j/k: select chat, Enter: load, l: to chat, n: new, d: delete",
            ));
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled(
                "Chat — NORMAL",
                Style::default()
                    .fg(app.theme.heading2)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(
                "  h: to sidebar, i: insert, v: visual, p: paste clipboard",
            ));
            lines.push(Line::from(
                "  ↑/k: scroll up   ↓/j: scroll down   g: top   G: bottom",
            ));
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled(
                "Chat — VISUAL (message select)",
                Style::default()
                    .fg(app.theme.heading2)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("  v/Esc: exit visual, h: to sidebar"));
            lines.push(Line::from(
                "  j/k: select next/prev message, y: yank selected to clipboard",
            ));
            lines.push(Line::from(
                "  Enter: preview selected message (Pandoc), ↑/↓: manual scroll",
            ));
            lines.push(Line::from(""));

            lines.push(Line::from(Span::styled(
                "Chat — INSERT",
                Style::default()
                    .fg(app.theme.heading2)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(
                "  Type, Enter: newline, Ctrl+S: send to Ollama, Esc: back to NORMAL",
            ));
            lines.push(Line::from(
                "  ←/→: move cursor   ↑/↓: move line   Backspace: delete previous grapheme",
            ));
            lines.push(Line::from(""));

            let p = Paragraph::new(Text::from(lines))
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(p, area);
        }
        Popup::None => {}
    }
}

fn draw_sidebar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .chats
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let ts_utc: DateTime<Utc> = Utc.timestamp_opt(c.updated_ts, 0).unwrap();
            let ts_local = ts_utc.with_timezone(&Local);
            let ts = ts_local.format("%d/%m %H:%M").to_string();
            let selected = app.focus == Focus::Sidebar && i == app.sidebar_idx;
            let mut spans = vec![
                Span::styled(format!("[{ts}]"), Style::default().fg(app.theme.sidebar_timestamp)),
                Span::raw(" "),
                Span::styled(&c.title, Style::default().fg(app.theme.sidebar_item)),
            ];
            if selected {
                let m = if app.bold_selection {
                    Modifier::BOLD
                } else {
                    Modifier::REVERSED
                };
                for s in &mut spans {
                    s.style = s.style.add_modifier(m);
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.border_sidebar))
            .title(Span::styled(
                "Chats",
                Style::default()
                    .fg(app.theme.sidebar_title)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(list, area);
}

fn draw_chat(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    // Input height: 1 if single-line, 3 if contains newline(s)
    let input_multiline = app.input.chars().filter(|&c| c == '\n').count();
    let input_height: u16 = std::cmp::min(input_multiline as u16 + 1, 5);

    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(input_height + 2)]) // +2 borders
        .split(area);

    app.chat_inner_height = v_chunks[0].height.saturating_sub(2);
    let inner_w = v_chunks[0].width.saturating_sub(2);

    let mut text = Text::default();
    let sel = app.selected_msg.unwrap_or_else(|| app.messages.len().saturating_sub(1));

    for (i, m) in app.messages.iter().enumerate() {
        let selected = app.focus == Focus::Chat && app.mode == Mode::Visual && i == sel;

        let (prefix_text, prefix_color) = match m.role {
            Role::User => ("You:".to_string(), app.theme.user_prefix),
            Role::Assistant => (format!("{}:", app.model), app.theme.assistant_prefix),
            Role::System => ("System:".to_string(), app.theme.system_prefix),
        };
        let mut pstyle = Style::default()
            .fg(prefix_color)
            .add_modifier(Modifier::BOLD);
        pstyle = apply_selection(pstyle, selected, app.bold_selection);
        text.push_line(Line::styled(prefix_text, pstyle));

        if app.show_thinking {
            if let Some(thinking) = m.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
                let thinking_style = Style::default().fg(app.theme.status_hint);
                text.push_line(Line::styled("Thinking:", thinking_style.add_modifier(Modifier::ITALIC)));
                let rendered_thinking = render_markdown_to_text(
                    thinking,
                    &app.theme,
                    inner_w.saturating_sub(2),
                    &app.syn_ss,
                    &app.syn_theme,
                    app.syntax_enabled,
                );
                for line in rendered_thinking.lines {
                    let spans = std::iter::once(Span::styled("▏ ", thinking_style))
                        .chain(line.spans.into_iter().map(|span| {
                            Span::styled(span.content.to_string(), thinking_style.patch(span.style))
                        }))
                        .collect::<Vec<_>>();
                    text.push_line(Line::from(spans));
                }
                text.push_line(Line::from(""));
            }
        }

        // Cached body render
        let msg_h = message_hash(m);
        let key = (i, inner_w);
        let body: Text<'static> = if let Some((h, cached)) = app.render_cache.get(&key) {
            if *h == msg_h {
                cached.clone()
            } else {
                let t = render_markdown_to_text(
                    &m.content,
                    &app.theme,
                    inner_w,
                    &app.syn_ss,
                    &app.syn_theme,
                    app.syntax_enabled,
                );
                app.render_cache.insert(key, (msg_h, t.clone()));
                t
            }
        } else {
            let t = render_markdown_to_text(
                &m.content,
                &app.theme,
                inner_w,
                &app.syn_ss,
                &app.syn_theme,
                app.syntax_enabled,
            );
            app.render_cache.insert(key, (msg_h, t.clone()));
            t
        };

        let mut md = body;
        if selected {
            let m = if app.bold_selection {
                Modifier::BOLD
            } else {
                Modifier::REVERSED
            };
            md = clone_with_modifier(md, m);
        }
        for line in md.lines {
            text.push_line(line);
        }
        text.push_line(Line::from(""));
    }

    let has_pending_stream = app.sending
        || !app.pending_assistant.trim().is_empty()
        || !app.pending_thinking.trim().is_empty();
    if has_pending_stream {
        let selected = app.focus == Focus::Chat && app.mode == Mode::Visual && app.messages.len() == sel;
        let mut hdr_style = Style::default()
            .fg(app.theme.assistant_prefix)
            .add_modifier(Modifier::BOLD);
        hdr_style = apply_selection(hdr_style, selected, app.bold_selection);
        text.push_line(Line::styled(format!("{}:", app.model), hdr_style));

        if app.show_thinking && !app.pending_thinking.trim().is_empty() {
            let thinking_style = Style::default().fg(app.theme.status_hint);
            text.push_line(Line::styled("Thinking:", thinking_style.add_modifier(Modifier::ITALIC)));
            let rendered_thinking = render_markdown_to_text(
                &app.pending_thinking,
                &app.theme,
                inner_w.saturating_sub(2),
                &app.syn_ss,
                &app.syn_theme,
                app.syntax_enabled,
            );
            for line in rendered_thinking.lines {
                let spans = std::iter::once(Span::styled("▏ ", thinking_style))
                    .chain(line.spans.into_iter().map(|span| {
                        Span::styled(span.content.to_string(), thinking_style.patch(span.style))
                    }))
                    .collect::<Vec<_>>();
                text.push_line(Line::from(spans));
            }
            text.push_line(Line::from(""));
        }

        if !app.pending_assistant.is_empty() {
            // Cached pending render
            let pending_h = str_hash(&app.pending_assistant);
            let pending_text: Text<'static> = match &app.pending_cache {
                Some((w, h, t)) if *w == inner_w && *h == pending_h => t.clone(),
                _ => {
                    let t = render_markdown_to_text(
                        &app.pending_assistant,
                        &app.theme,
                        inner_w,
                        &app.syn_ss,
                        &app.syn_theme,
                        app.syntax_enabled,
                    );
                    app.pending_cache = Some((inner_w, pending_h, t.clone()));
                    t
                }
            };

            let mut md = pending_text.clone();
            if let Some(last) = md.lines.last_mut() {
                last.spans.push(Span::raw("▌"));
            } else {
                md.lines.push(Line::from("▌"));
            }
            if selected {
                let m = if app.bold_selection {
                    Modifier::BOLD
                } else {
                    Modifier::REVERSED
                };
                md = clone_with_modifier(md, m);
            }
            for line in md.lines {
                text.push_line(line);
            }
            text.push_line(Line::from(""));
        }
    }

    let mode_span = match app.mode {
        Mode::Insert => Span::styled(
            "[INSERT]",
            Style::default()
                .fg(app.theme.mode_insert)
                .add_modifier(Modifier::BOLD),
        ),
        Mode::Normal => Span::styled(
            "[NORMAL]",
            Style::default()
                .fg(app.theme.mode_normal)
                .add_modifier(Modifier::BOLD),
        ),
        Mode::Visual => Span::styled(
            "[VISUAL]",
            Style::default()
                .fg(app.theme.mode_visual)
                .add_modifier(Modifier::BOLD),
        ),
    };
    let mut title_spans = vec![
        Span::styled(
            "Chat ",
            Style::default()
                .fg(app.theme.title_chat)
                .add_modifier(Modifier::BOLD),
        ),
        mode_span,
    ];
    if matches!(app.focus, Focus::Chat) && matches!(app.mode, Mode::Normal | Mode::Visual) {
        title_spans.push(Span::styled(
            " [FOCUS]",
            Style::default()
                .fg(app.theme.mode_focus)
                .add_modifier(Modifier::BOLD),
        ));
    }
    title_spans.push(Span::styled(
        if app.web_search { " [WEB ON]" } else { " [WEB OFF]" },
        Style::default()
            .fg(if app.web_search { app.theme.mode_insert } else { app.theme.status_hint })
            .add_modifier(Modifier::BOLD),
    ));
    title_spans.push(Span::styled(
        if app.show_thinking { " [THINK ON]" } else { " [THINK OFF]" },
        Style::default()
            .fg(if app.show_thinking {
                app.theme.mode_visual
            } else {
                app.theme.status_hint
            })
            .add_modifier(Modifier::BOLD),
    ));
    if let Some(status) = app.status_message.as_deref() {
        title_spans.push(Span::styled(
            format!(" — {status}"),
            Style::default().fg(app.theme.status_hint),
        ));
    }
    let chat_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border_chat))
        .title(Line::from(title_spans));

    let messages = Paragraph::new(text)
        .block(chat_block)
        .wrap(Wrap { trim: false })
        .scroll((app.chat_scroll, 0));
    frame.render_widget(messages, v_chunks[0]);

    // Status/input bar — render multiline input (height = 1 or 3)
    let help_hint = Span::styled(
        "— Press ? for help",
        Style::default().fg(app.theme.status_hint),
    );

    let input_title_line: Line = match app.mode {
        Mode::Insert => Line::from(vec![
            Span::styled(
                "Message ",
                Style::default()
                    .fg(app.theme.title_input)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[INSERT] ",
                Style::default()
                    .fg(app.theme.mode_insert)
                    .add_modifier(Modifier::BOLD),
            ),
            help_hint.clone(),
        ]),
        Mode::Normal => {
            let base = if app.focus == Focus::Sidebar { "Sidebar " } else { "Navigation " };
            Line::from(vec![
                Span::styled(
                    base,
                    Style::default()
                        .fg(app.theme.title_input)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "[NORMAL] ",
                    Style::default()
                        .fg(app.theme.mode_normal)
                        .add_modifier(Modifier::BOLD),
                ),
                help_hint.clone(),
            ])
        }
        Mode::Visual => {
            let base = if app.focus == Focus::Sidebar { "Sidebar " } else { "Navigation " };
            Line::from(vec![
                Span::styled(
                    base,
                    Style::default()
                        .fg(app.theme.title_input)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "[VISUAL] ",
                    Style::default()
                        .fg(app.theme.mode_visual)
                        .add_modifier(Modifier::BOLD),
                ),
                help_hint.clone(),
            ])
        }
    };

    // Prepare visible input lines
    let input_inner_w = v_chunks[1].width.saturating_sub(2) as usize;
    let lines: Vec<&str> = app.input.split('\n').collect();
    let total_lines = lines.len();
    if app.input_cursor_line >= total_lines {
        app.input_cursor_line = total_lines.saturating_sub(1);
    }
    let cur_line_str = lines.get(app.input_cursor_line).copied().unwrap_or("");
    let cur_line_grs: Vec<&str> = UnicodeSegmentation::graphemes(cur_line_str, true).collect();
    if app.input_cursor_col > cur_line_grs.len() {
        app.input_cursor_col = cur_line_grs.len();
    }

    // Keep cursor visible with headroom: stop two lines before top when input is multiline (height=3)
    let view_h = input_height as usize;
    let headroom = if view_h > 1 { view_h - 1 } else { 0 }; // for h=3 -> 2 lines
    let max_top_allowed = total_lines.saturating_sub(view_h);
    let min_top_for_cursor = app.input_cursor_line.saturating_sub(headroom);
    let max_top_for_cursor = app.input_cursor_line.min(max_top_allowed);
    if app.input_top_line < min_top_for_cursor {
        app.input_top_line = min_top_for_cursor;
    }
    if app.input_top_line > max_top_for_cursor {
        app.input_top_line = max_top_for_cursor;
    }
    if app.input_top_line > max_top_allowed {
        app.input_top_line = max_top_allowed;
    }

    // Build visible text and cursor position
    let mut input_text = Text::default();
    let mut cursor_x_in_view: usize = 0;
    for i in 0..view_h {
        let li = app.input_top_line + i;
        let s = if li < total_lines { lines[li] } else { "" };
        if li == app.input_cursor_line {
            let (visible, cx) = line_view(s, app.input_cursor_col, input_inner_w);
            cursor_x_in_view = cx;
            input_text.lines.push(Line::from(visible));
        } else {
            // non-cursor line: simple clamp
            let mut visible = String::new();
            for g in UnicodeSegmentation::graphemes(s, true) {
                let w = UnicodeWidthStr::width(g);
                if UnicodeWidthStr::width(visible.as_str()) + w > input_inner_w {
                    break;
                }
                visible.push_str(g);
            }
            input_text.lines.push(Line::from(visible));
        }
    }
    // If single-line and completely empty in NORMAL, draw hint text style
    let input_para = Paragraph::new(input_text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.border_input))
            .title(input_title_line),
    );
    frame.render_widget(input_para, v_chunks[1]);

    // Cursor (thin bar) only in INSERT mode within chat focus
    if app.mode == Mode::Insert && app.focus == Focus::Chat {
        let cursor_row = (app.input_cursor_line.saturating_sub(app.input_top_line)) as u16;
        frame.set_cursor(
            v_chunks[1].x + 1 + cursor_x_in_view as u16,
            v_chunks[1].y + 1 + cursor_row,
        );
    }
}

// visual height estimate helpers
fn content_total_height(messages: &[Message], pending: Option<&str>) -> u16 {
    let mut y: u16 = 0;
    for m in messages {
        y = y.saturating_add(1);
        y = y.saturating_add(m.content.lines().count() as u16);
        y = y.saturating_add(1);
    }
    if let Some(p) = pending {
        y = y.saturating_add(1);
        y = y.saturating_add(p.lines().count() as u16);
        y = y.saturating_add(1);
    }
    y
}

fn offset_for_message(messages: &[Message], idx: usize) -> u16 {
    let mut y: u16 = 0;
    for m in &messages[..idx.min(messages.len())] {
        y = y.saturating_add(1);
        y = y.saturating_add(m.content.lines().count() as u16);
        y = y.saturating_add(1);
    }
    y
}
