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

const TAB_DISPLAY: &str = "    ";
const TAB_WIDTH: usize = 4;

fn display_grapheme(g: &str) -> &str {
    if g == "\t" { TAB_DISPLAY } else { g }
}

fn display_width(g: &str) -> usize {
    if g == "\t" {
        TAB_WIDTH
    } else {
        UnicodeWidthStr::width(g)
    }
}

fn line_view(s: &str, col_gi: usize, maxw: usize) -> (String, usize) {
    let maxw = maxw.max(1);
    let graphemes: Vec<&str> = UnicodeSegmentation::graphemes(s, true).collect();
    let widths: Vec<usize> = graphemes.iter().map(|g| display_width(g)).collect();

    let mut w_to_cursor = 0usize;
    for width in widths.iter().take(col_gi.min(widths.len())) {
        w_to_cursor += width;
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
        out.push_str(display_grapheme(graphemes[i]));
        used += w;
    }

    let cursor_x = w_from_start_to_cursor.min(used);
    (out, cursor_x)
}

fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return "Unknown time".to_string();
    }
    let ts_utc: DateTime<Utc> = match Utc.timestamp_opt(ts, 0).single() {
        Some(ts) => ts,
        None => return "Unknown time".to_string(),
    };
    ts_utc
        .with_timezone(&Local)
        .format("%b %d, %Y · %H:%M")
        .to_string()
}

fn message_timestamp(m: &Message, fallback: Option<i64>) -> String {
    let ts = if m.created_ts > 0 {
        m.created_ts
    } else {
        fallback.unwrap_or_default()
    };
    format_timestamp(ts)
}

fn approx_token_count(text: &str) -> usize {
    let chars = text.chars().count();
    ((chars as f32) / 4.0).ceil() as usize
}

fn context_window_tokens(app: &App) -> Option<usize> {
    app.options.as_ref().and_then(|options| {
        options.get("num_ctx").and_then(|value| {
            value
                .as_u64()
                .map(|n| n as usize)
                .or_else(|| value.as_str().and_then(|s| s.parse::<usize>().ok()))
        })
    })
}

fn format_token_budget(tokens: usize, context_window: usize) -> String {
    format!("{} / {}", compact_token_count(tokens), compact_token_count(context_window))
}

fn compact_token_count(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn wrapped_text_height(text: &Text, width: u16) -> u16 {
    let width = width.max(1) as usize;
    text.lines
        .iter()
        .map(|line| {
            let line_width: usize = line
                .spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            std::cmp::max(1, line_width.div_ceil(width)) as u16
        })
        .sum()
}

fn section_block<'a>(title: impl Into<Line<'a>>, bg: Color, border: Color, borders: Borders) -> Block<'a> {
    Block::default()
        .style(Style::default().bg(bg))
        .borders(borders)
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1))
        .title(title)
}

fn panel_block(bg: Color, border: Color, borders: Borders) -> Block<'static> {
    Block::default()
        .style(Style::default().bg(bg))
        .borders(borders)
        .border_style(Style::default().fg(border))
        .padding(Padding::horizontal(1))
}

fn draw_ui(frame: &mut ratatui::Frame, app: &mut App) {
    frame.render_widget(Block::default().style(Style::default().bg(app.theme.app_bg)), frame.size());

    let constraints = if app.show_sidebar {
        [Constraint::Length(34), Constraint::Min(72)]
    } else {
        [Constraint::Length(0), Constraint::Min(100)]
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
        Popup::ConfirmDelete { title, .. } => {
            let area = centered_rect(60, 30, frame.size());
            frame.render_widget(Clear, area);
            let block = Block::default()
                .style(Style::default().bg(app.theme.panel_bg))
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
            let area = centered_rect(82, 82, frame.size());
            frame.render_widget(Clear, area);
            let block = Block::default()
                .style(Style::default().bg(app.theme.panel_bg))
                .title(Span::styled(
                    "Keybindings",
                    Style::default()
                        .fg(app.theme.popup_title)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.border_chat));

            let lines: Vec<Line> = vec![
                Line::from(Span::styled("Global", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))),
                Line::from("  ?: show/close help"),
                Line::from("  Ctrl+T: hide/show left sidebar"),
                Line::from("  Ctrl+P: hide/show right statistics panel"),
                Line::from("  Ctrl+W: toggle web search"),
                Line::from("  : command mode (:attach PATH, :web on)"),
                Line::from("  Ctrl+Y: hide/show thinking blocks"),
                Line::from("  Ctrl+X: terminate generation"),
                Line::from("  q / Ctrl+C: quit"),
                Line::from(""),
                Line::from(Span::styled("Sidebar", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))),
                Line::from("  j/k: select chat, Enter: load, l: to chat, n: new, d: delete"),
                Line::from(""),
                Line::from(Span::styled("Chat", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))),
                Line::from("  h/l: move focus, i: insert, v: visual, p: paste clipboard"),
                Line::from("  r: refresh screen"),
                Line::from("  Sidebar focus: / search chats, Enter apply, Esc clear"),
                Line::from("  ↑/k: scroll up, ↓/j: scroll down, g: top, G: bottom"),
                Line::from("  :attach /path/to/image then Enter to attach image"),
                Line::from("  :web on then Enter to enable web search"),
                Line::from("  Ctrl+S: send in INSERT mode, Esc: normal mode"),
            ];
            frame.render_widget(Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false }), area);
        }
        Popup::None => {}
    }
}

fn draw_sidebar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let block = section_block(
        Line::from(Span::styled(
            " Conversations ",
            Style::default()
                .fg(app.theme.sidebar_title)
                .add_modifier(Modifier::BOLD),
        )),
        app.theme.panel_bg,
        app.theme.border_sidebar,
        Borders::RIGHT,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut items: Vec<ListItem> = Vec::new();
    let search_label = if app.sidebar_search_query.is_empty() {
        "Search chats…".to_string()
    } else {
        format!("Search: {}", app.sidebar_search_query)
    };
    let search_suffix = if app.sidebar_search_active {
        "[typing]"
    } else if app.sidebar_search_query.is_empty() {
        "[/]"
    } else {
        "[Esc clears]"
    };
    let search_bg = if app.sidebar_search_active {
        app.theme.sidebar_selected_bg
    } else {
        app.theme.panel_alt_bg
    };
    items.push(
        ListItem::new(Line::from(vec![
            Span::styled(search_label, Style::default().fg(app.theme.status_hint)),
            Span::raw(" "),
            Span::styled(
                search_suffix,
                Style::default()
                    .fg(app.theme.mode_insert)
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(search_bg)),
    );
    items.push(ListItem::new(Line::from(Span::styled(
        "─".repeat(inner.width.saturating_sub(2) as usize),
        Style::default().fg(app.theme.message_rule),
    ))));

    for (i, c) in app.chats.iter().enumerate() {
        let selected = app.focus == Focus::Sidebar && i == app.sidebar_idx;
        let ts = format_timestamp(c.updated_ts);
        let style = if selected {
            Style::default().bg(app.theme.sidebar_selected_bg)
        } else {
            Style::default().bg(app.theme.sidebar_item_bg)
        };
        let mut lines = vec![
            Line::from(Span::styled(c.title.clone(), Style::default().fg(app.theme.sidebar_item).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(ts, Style::default().fg(app.theme.sidebar_timestamp))),
        ];
        if let Some(excerpt) = &c.search_excerpt {
            lines.push(Line::from(Span::styled(excerpt.clone(), Style::default().fg(app.theme.status_hint))));
        }
        lines.push(Line::from(""));
        items.push(ListItem::new(lines).style(style));
    }

    let list = List::new(items).highlight_style(Style::default().bg(app.theme.sidebar_selected_bg));
    frame.render_widget(list, inner);
}

fn draw_stats_panel(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let total_chars: usize = app.messages.iter().map(|m| m.content.chars().count()).sum::<usize>()
        + app.pending_assistant.chars().count()
        + app.input.chars().count()
        + app.system_prompt.as_ref().map(|s| s.chars().count()).unwrap_or(0);
    let total_tokens = approx_token_count(&"x".repeat(total_chars));
    let context_budget = context_window_tokens(app)
        .map(|num_ctx| format_token_budget(total_tokens, num_ctx))
        .unwrap_or_else(|| "unknown".to_string());
    let status_value = if app.sending { "streaming" } else { "idle" };
    let mode_value = match app.mode {
        Mode::Normal => "normal",
        Mode::Insert => "insert",
        Mode::Command => "command",
        Mode::Visual => "visual",
    };

    let mut cards = Vec::new();
    cards.push(("web", if app.web_search { "on" } else { "off" }, app.theme.stats_accent));
    cards.push((
        "show reasoning",
        if app.show_thinking { "on" } else { "off" },
        if app.show_thinking { app.theme.stats_accent } else { app.theme.status_hint },
    ));
    cards.push(("stream", status_value, if app.sending { app.theme.assistant_prefix } else { app.theme.status_hint }));
    cards.push(("mode", mode_value, app.theme.stats_value));

    let outer = panel_block(app.theme.panel_bg, app.theme.border_chat, Borders::LEFT);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(10),
        ])
        .split(inner);

    let mut top_lines = vec![];
    top_lines.push(Line::from(Span::styled(
        "Statistics",
        Style::default()
            .fg(app.theme.title_chat)
            .add_modifier(Modifier::BOLD),
    )));
    for (label, value, color) in cards {
        top_lines.push(Line::from(vec![
            Span::styled(format!("{}: ", label), Style::default().fg(app.theme.stats_label)),
            Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
        ]));
    }
    top_lines.push(Line::from(""));
    top_lines.push(Line::from(vec![
        Span::styled("model: ", Style::default().fg(app.theme.stats_label)),
        Span::styled(app.model.clone(), Style::default().fg(app.theme.stats_value)),
    ]));
    frame.render_widget(
        Paragraph::new(Text::from(top_lines)).block(panel_block(
            app.theme.panel_alt_bg,
            app.theme.message_rule,
            Borders::BOTTOM,
        )),
        rows[0],
    );

    let last_activity = app
        .messages
        .last()
        .map(|m| message_timestamp(m, app.current_created_ts))
        .unwrap_or_else(|| format_timestamp(app.current_created_ts.unwrap_or_default()));
    let active_stream = app
        .current_chat_id
        .as_ref()
        .and_then(|chat_id| app.active_streams.get(chat_id));
    let live_tps = active_stream.map(|stream| {
        let elapsed = stream.started_at.elapsed().as_secs_f64().max(0.001);
        (stream.generated_tokens as f64) / elapsed
    });
    let tps_value = if let Some(tps) = live_tps {
        format!("{tps:.1} tok/s (live)")
    } else if let Some(tps) = app.last_stream_tps {
        format!("{tps:.1} tok/s (last)")
    } else {
        "n/a".to_string()
    };
    let metrics = vec![
        Line::from(vec![
            Span::styled("messages: ", Style::default().fg(app.theme.stats_label)),
            Span::styled(app.messages.len().to_string(), Style::default().fg(app.theme.stats_value).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("approx tokens: ", Style::default().fg(app.theme.stats_label)),
            Span::styled(total_tokens.to_string(), Style::default().fg(app.theme.stats_value).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("context: ", Style::default().fg(app.theme.stats_label)),
            Span::styled(context_budget, Style::default().fg(app.theme.stats_value).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("last activity: ", Style::default().fg(app.theme.stats_label)),
        ]),
        Line::from(vec![
            Span::styled("  ", Style::default().fg(app.theme.stats_label)),
            Span::styled(last_activity, Style::default().fg(app.theme.stats_value)),
        ]),
        Line::from(vec![
            Span::styled("streams live: ", Style::default().fg(app.theme.stats_label)),
            Span::styled(
                app.active_streams.len().to_string(),
                Style::default()
                    .fg(app.theme.stats_value)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("throughput: ", Style::default().fg(app.theme.stats_label)),
            Span::styled(tps_value, Style::default().fg(app.theme.stats_value).add_modifier(Modifier::BOLD)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(metrics)).block(section_block(
            Line::from(Span::styled(
                " Session ",
                Style::default()
                    .fg(app.theme.assistant_prefix)
                    .add_modifier(Modifier::BOLD),
            )),
            app.theme.panel_bg,
            app.theme.message_rule,
            Borders::BOTTOM,
        )),
        rows[1],
    );

    let trace = vec![
        Line::from(Span::styled("Trace log", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled("sidebar", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(if app.show_sidebar { "visible" } else { "hidden" }, Style::default().fg(app.theme.stats_value)),
        ]),
        Line::from(vec![
            Span::styled("stats panel", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(if app.show_stats_panel { "visible" } else { "hidden" }, Style::default().fg(app.theme.stats_value)),
        ]),
        Line::from(vec![
            Span::styled("focus", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(match app.focus { Focus::Sidebar => "sidebar", Focus::Chat => "chat" }, Style::default().fg(app.theme.stats_value)),
        ]),
        Line::from(vec![
            Span::styled("chat id", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(app.current_chat_id.clone().unwrap_or_else(|| "new chat".to_string()), Style::default().fg(app.theme.stats_value)),
        ]),
        Line::from(vec![
            Span::styled("request", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                app.pending_request_id
                    .clone()
                    .or_else(|| active_stream.map(|s| s.request_id.clone()))
                    .unwrap_or_else(|| "none".to_string()),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("status", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                app.status_message
                    .clone()
                    .or_else(|| active_stream.and_then(|s| s.status.clone()))
                    .unwrap_or_else(|| "waiting".to_string()),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("pending", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                format!(
                    "{} chars answer / {} chars thinking",
                    app.pending_assistant.chars().count(),
                    app.pending_thinking.chars().count()
                ),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("token run", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                format!("{} tokens in {} ms", app.last_stream_tokens, app.last_stream_ms),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("viewport", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                format!(
                    "scroll {} • inner {}x{}",
                    app.chat_scroll, app.chat_inner_width, app.chat_inner_height
                ),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("selection", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                format!(
                    "focus {:?} • selected {:?}",
                    app.focus, app.selected_msg
                ),
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(vec![
            Span::styled("input", Style::default().fg(app.theme.stats_label)),
            Span::raw(": "),
            Span::styled(
                if app.mode == Mode::Command {
                    format!(
                        "command {} chars • col {}",
                        app.command_input.chars().count(),
                        app.command_cursor_col + 1
                    )
                } else {
                    format!(
                        "{} chars • line {} col {}",
                        app.input.chars().count(),
                        app.input_cursor_line + 1,
                        app.input_cursor_col + 1
                    )
                },
                Style::default().fg(app.theme.stats_value),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled("Sources", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))),
    ];
    let latest_sources = active_stream
        .map(|stream| stream.sources.as_slice())
        .filter(|sources| !sources.is_empty())
        .or_else(|| app.messages.iter().rev().find(|m| !m.sources.is_empty()).map(|m| m.sources.as_slice()));
    let mut trace = trace;
    if let Some(sources) = latest_sources {
        for (idx, source) in sources.iter().take(4).enumerate() {
            trace.push(Line::from(vec![
                Span::styled(format!("{}.", idx + 1), Style::default().fg(app.theme.stats_label)),
                Span::raw(" "),
                Span::styled(source.title.clone(), Style::default().fg(app.theme.stats_value)),
            ]));
            trace.push(Line::from(Span::styled(source.url.clone(), Style::default().fg(app.theme.status_hint))));
        }
    } else {
        trace.push(Line::from(Span::styled("No sources yet", Style::default().fg(app.theme.status_hint))));
    }
    trace.push(Line::from(""));
    trace.push(Line::from(Span::styled("Ctrl+P toggles this panel.", Style::default().fg(app.theme.status_hint))));
    frame.render_widget(
        Paragraph::new(Text::from(trace))
            .wrap(Wrap { trim: false })
            .block(section_block(
                Line::from(Span::styled(
                    " Debug ",
                    Style::default()
                        .fg(app.theme.user_prefix)
                        .add_modifier(Modifier::BOLD),
                )),
                app.theme.panel_alt_bg,
                app.theme.message_rule,
                Borders::NONE,
            )),
        rows[2],
    );
}

fn render_message_block(text: &mut Text<'static>, app: &mut App, idx: usize, m: &Message, inner_w: u16, selected: bool) {
    let timestamp = message_timestamp(m, app.current_created_ts);
    let (label, color) = match m.role {
        Role::User => ("You", app.theme.user_prefix),
        Role::Assistant => (app.model.as_str(), app.theme.assistant_prefix),
        Role::System => ("System", app.theme.system_prefix),
    };
    let label_text = format!("{}:", label);
    let label_width = UnicodeWidthStr::width(label_text.as_str());
    let timestamp_width = UnicodeWidthStr::width(timestamp.as_str());
    let spacer_width = 2usize;
    let rule_len = (inner_w as usize)
        .saturating_sub(label_width + timestamp_width + spacer_width)
        .max(1);
    let rule = "─".repeat(rule_len);
    let mut header_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
    header_style = apply_selection(header_style, selected, app.bold_selection);
    let meta_style = apply_selection(Style::default().fg(app.theme.message_meta), selected, app.bold_selection);
    let rule_style = apply_selection(Style::default().fg(app.theme.message_rule), selected, app.bold_selection);
    text.push_line(Line::from(vec![
        Span::styled(label_text, header_style),
        Span::raw(" "),
        Span::styled(rule, rule_style),
        Span::raw(" "),
        Span::styled(timestamp, meta_style),
    ]));

    if app.show_thinking {
        if let Some(thinking) = m.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
            let thinking_style = Style::default().fg(app.theme.status_hint);
            text.push_line(Line::styled("Thinking", thinking_style.add_modifier(Modifier::ITALIC)));
            let rendered_thinking = render_markdown_to_text(
                thinking,
                &app.theme,
                inner_w.saturating_sub(3),
                &app.syn_ss,
                &app.syn_theme,
                app.syntax_enabled,
                app.render_emojis,
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

    if !m.attachments.is_empty() {
        for attachment in &m.attachments {
            text.push_line(Line::from(vec![
                Span::styled("Attachment: ", Style::default().fg(app.theme.stats_label).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} ({})", attachment.path, attachment.mime_type), Style::default().fg(app.theme.status_hint)),
            ]));
        }
        text.push_line(Line::from(""));
    }

    let msg_h = message_hash(m);
    let key = (idx, inner_w);
    let body = if let Some((h, cached)) = app.render_cache.get(&key) {
        if *h == msg_h {
            cached.clone()
        } else {
            let t = render_markdown_to_text(&m.content, &app.theme, inner_w, &app.syn_ss, &app.syn_theme, app.syntax_enabled, app.render_emojis);
            app.render_cache.insert(key, (msg_h, t.clone()));
            t
        }
    } else {
        let t = render_markdown_to_text(&m.content, &app.theme, inner_w, &app.syn_ss, &app.syn_theme, app.syntax_enabled, app.render_emojis);
        app.render_cache.insert(key, (msg_h, t.clone()));
        t
    };
    let mut md = body;
    if selected {
        md = clone_with_modifier(md, if app.bold_selection { Modifier::BOLD } else { Modifier::REVERSED });
    }
    for line in md.lines {
        text.push_line(line);
    }
    if !m.sources.is_empty() {
        text.push_line(Line::from(""));
        text.push_line(Line::from(Span::styled("Sources", Style::default().fg(app.theme.heading2).add_modifier(Modifier::BOLD))));
        for (idx, source) in m.sources.iter().enumerate() {
            let title = if source.title.is_empty() { source.url.clone() } else { source.title.clone() };
            text.push_line(Line::from(vec![
                Span::styled(format!("[{}] ", idx + 1), Style::default().fg(app.theme.stats_label)),
                Span::styled(title, Style::default().fg(app.theme.stats_value)),
            ]));
            text.push_line(Line::from(Span::styled(source.url.clone(), Style::default().fg(app.theme.status_hint))));
            if !source.snippet.is_empty() {
                text.push_line(Line::from(Span::styled(source.snippet.clone(), Style::default().fg(app.theme.message_meta))));
            }
        }
    }
    text.push_line(Line::from(""));
}

fn draw_chat(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let input_multiline = app.input.chars().filter(|&c| c == '\n').count();
    let input_height: u16 = if app.mode == Mode::Command {
        1
    } else {
        std::cmp::min(input_multiline as u16 + 1, 5)
    };

    let cols = if app.show_stats_panel {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Length(36)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Length(0)])
            .split(area)
    };

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8), Constraint::Length(input_height + 2)])
        .split(cols[0]);

    let current_title = app
        .chats
        .iter()
        .find(|c| Some(c.id.as_str()) == app.current_chat_id.as_deref())
        .map(|c| c.title.clone())
        .or_else(|| app.messages.iter().find(|m| matches!(m.role, Role::User)).map(|m| m.content.lines().next().unwrap_or("Untitled chat").to_string()))
        .unwrap_or_else(|| "Untitled chat".to_string());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            current_title,
            Style::default()
                .fg(app.theme.title_chat)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" • {}", app.model),
            Style::default().fg(app.theme.message_meta),
        ),
    ]))
    .block(section_block(
        Line::default(),
        app.theme.title_bar_bg,
        app.theme.border_chat,
        Borders::BOTTOM,
    ));
    frame.render_widget(header, left[0]);

    app.chat_inner_height = left[1].height.saturating_sub(2);
    app.chat_inner_width = left[1].width.saturating_sub(2);
    let inner_w = app.chat_inner_width;

    let mut text = Text::default();
    let sel = app.selected_msg.unwrap_or_else(|| app.messages.len().saturating_sub(1));
    for i in 0..app.messages.len() {
        let selected = app.focus == Focus::Chat && app.mode == Mode::Visual && i == sel;
        let m = app.messages[i].clone();
        render_message_block(&mut text, app, i, &m, inner_w, selected);
    }

    let has_pending_stream = app.sending
        || !app.pending_assistant.trim().is_empty()
        || !app.pending_thinking.trim().is_empty()
        || !app.pending_sources.is_empty();
    if has_pending_stream {
        let selected = app.focus == Focus::Chat && app.mode == Mode::Visual && app.messages.len() == sel;
        let pending = Message {
            role: Role::Assistant,
            content: if app.pending_assistant.is_empty() { "…".to_string() } else { app.pending_assistant.clone() },
            created_ts: now_sec(),
            thinking: (!app.pending_thinking.trim().is_empty()).then_some(app.pending_thinking.clone()),
            attachments: Vec::new(),
            sources: app.pending_sources.clone(),
        };
        render_message_block(&mut text, app, app.messages.len(), &pending, inner_w, selected);
    }

    let wrapped_line_count = wrapped_text_height(&text, inner_w.max(1));
    let max_chat_scroll = wrapped_line_count.saturating_sub(app.chat_inner_height);
    if app.scroll_to_bottom_on_draw {
        app.chat_scroll = max_chat_scroll;
        app.scroll_to_bottom_on_draw = false;
    } else {
        app.chat_scroll = app.chat_scroll.min(max_chat_scroll);
    }
    app.chat_at_bottom = app.chat_scroll >= max_chat_scroll;

    frame.render_widget(
        Paragraph::new(text)
            .block(section_block(
                Line::from(Span::styled(
                    " ",
                    Style::default()
                        .fg(app.theme.title_input)
                        .add_modifier(Modifier::BOLD),
                )),
                app.theme.app_bg,
                app.theme.border_chat,
                Borders::BOTTOM,
            ))
            .wrap(Wrap { trim: false })
            .scroll((app.chat_scroll, 0)),
        left[1],
    );

    let help_hint = Span::styled(" • ? help", Style::default().fg(app.theme.status_hint));
    let base = match app.mode {
        Mode::Insert => "Compose",
        Mode::Command => "Command Palette",
        Mode::Normal | Mode::Visual => "Command",
    };
    let mode_name = match app.mode { Mode::Insert => "INSERT", Mode::Command => "COMMAND", Mode::Normal => "NORMAL", Mode::Visual => "VISUAL" };
    let mode_color = match app.mode { Mode::Insert => app.theme.mode_insert, Mode::Command => app.theme.mode_insert, Mode::Normal => app.theme.mode_normal, Mode::Visual => app.theme.mode_visual };
    let attachment_hint = if app.pending_attachments.is_empty() {
        Span::raw("")
    } else {
        Span::styled(
            format!(" • {} image(s) attached", app.pending_attachments.len()),
            Style::default().fg(app.theme.status_hint),
        )
    };
    let input_title_line = Line::from(vec![
        Span::styled(format!(" {} ", base), Style::default().fg(app.theme.title_input).add_modifier(Modifier::BOLD)),
        Span::styled(format!("[{}]", mode_name), Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
        help_hint,
        attachment_hint,
    ]);

    let input_block = section_block(
        input_title_line,
        app.theme.input_bg,
        app.theme.border_input,
        Borders::ALL,
    );
    let input_inner = input_block.inner(left[2]);
    let input_inner_w = input_inner.width as usize;
    let command_display = format!(":{}", app.command_input);
    let lines: Vec<&str> = if app.mode == Mode::Command {
        vec![command_display.as_str()]
    } else {
        app.input.split('\n').collect()
    };
    let total_lines = lines.len();
    let active_cursor_line = if app.mode == Mode::Command {
        0
    } else {
        if app.input_cursor_line >= total_lines { app.input_cursor_line = total_lines.saturating_sub(1); }
        app.input_cursor_line
    };
    let cur_line_str = lines.get(active_cursor_line).copied().unwrap_or("");
    let cur_line_grs: Vec<&str> = UnicodeSegmentation::graphemes(cur_line_str, true).collect();
    let active_cursor_col = if app.mode == Mode::Command {
        (app.command_cursor_col + 1).min(cur_line_grs.len())
    } else {
        if app.input_cursor_col > cur_line_grs.len() { app.input_cursor_col = cur_line_grs.len(); }
        app.input_cursor_col
    };

    let view_h = input_height as usize;
    let headroom = if view_h > 1 { view_h - 1 } else { 0 };
    let max_top_allowed = total_lines.saturating_sub(view_h);
    let min_top_for_cursor = active_cursor_line.saturating_sub(headroom);
    let max_top_for_cursor = active_cursor_line.min(max_top_allowed);
    if app.input_top_line < min_top_for_cursor { app.input_top_line = min_top_for_cursor; }
    if app.input_top_line > max_top_for_cursor { app.input_top_line = max_top_for_cursor; }
    if app.input_top_line > max_top_allowed { app.input_top_line = max_top_allowed; }

    let mut input_text = Text::default();
    let mut cursor_x_in_view: usize = 0;
    for i in 0..view_h {
        let li = app.input_top_line + i;
        let s = if li < total_lines { lines[li] } else { "" };
        if li == active_cursor_line {
            let (visible, cx) = line_view(s, active_cursor_col, input_inner_w);
            cursor_x_in_view = cx;
            input_text.lines.push(Line::from(visible));
        } else {
            let mut visible = String::new();
            for g in UnicodeSegmentation::graphemes(s, true) {
                let w = display_width(g);
                if UnicodeWidthStr::width(visible.as_str()) + w > input_inner_w {
                    break;
                }
                visible.push_str(display_grapheme(g));
            }
            input_text.lines.push(Line::from(visible));
        }
    }

    frame.render_widget(Paragraph::new(input_text).block(input_block), left[2]);

    if app.mode == Mode::Insert && app.focus == Focus::Chat {
        let cursor_row = (app.input_cursor_line.saturating_sub(app.input_top_line)) as u16;
        frame.set_cursor(
            input_inner.x + cursor_x_in_view as u16,
            input_inner.y + cursor_row,
        );
    } else if app.mode == Mode::Command {
        frame.set_cursor(
            input_inner.x + (app.command_cursor_col + 1).min(input_inner_w) as u16,
            input_inner.y,
        );
    }

    if app.show_stats_panel {
        draw_stats_panel(frame, cols[1], app);
    }
}

fn rendered_message_height(app: &App, inner_w: u16, m: &Message) -> u16 {
    let mut y: u16 = 0;
    y = y.saturating_add(1);
    if app.show_thinking {
        if let Some(thinking) = m.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
            y = y.saturating_add(1);
            let rendered_thinking = render_markdown_to_text(thinking, &app.theme, inner_w.saturating_sub(3), &app.syn_ss, &app.syn_theme, app.syntax_enabled, app.render_emojis);
            y = y.saturating_add(rendered_thinking.lines.len() as u16);
            y = y.saturating_add(1);
        }
    }
    if !m.attachments.is_empty() {
        y = y.saturating_add(m.attachments.len() as u16 + 1);
    }
    let rendered_body = render_markdown_to_text(&m.content, &app.theme, inner_w, &app.syn_ss, &app.syn_theme, app.syntax_enabled, app.render_emojis);
    y = y.saturating_add(rendered_body.lines.len() as u16);
    if !m.sources.is_empty() {
        let source_lines = m
            .sources
            .iter()
            .map(|source| if source.snippet.is_empty() { 2 } else { 3 })
            .sum::<u16>();
        y = y.saturating_add(2 + source_lines);
    }
    y.saturating_add(1)
}

fn offset_for_message(app: &App, inner_w: u16, idx: usize) -> u16 {
    let mut y: u16 = 0;
    for m in &app.messages[..idx.min(app.messages.len())] {
        y = y.saturating_add(rendered_message_height(app, inner_w, m));
    }
    y
}
