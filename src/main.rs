use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use directories::ProjectDirs;
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Terminal,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use chrono::NaiveDateTime;
use regex::{Captures, Regex};
use tokio::process::Command;
use std::process::Stdio;
use rand::random;

// ---------- Data models ----------
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    role: Role,
    content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Chat {
    id: String,
    title: String,
    created_ts: i64,
    updated_ts: i64,
    messages: Vec<Message>,
}

#[derive(Clone, Debug)]
struct ChatMeta {
    id: String,
    title: String,
    updated_ts: i64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode { Normal, Insert }

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus { Sidebar, Chat }

// ---------- Config ----------
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AppConfig {
    model: Option<String>,
    api_url: Option<String>,
    options: Option<toml::value::Table>,
}

fn config_path() -> Result<PathBuf> {
    let proj = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve config dir"))?;
    Ok(proj.config_dir().join("config.toml"))
}

fn load_config(path: &PathBuf) -> Result<AppConfig> {
    if !path.exists() { return Ok(AppConfig::default()); }
    let s = fs::read_to_string(path).with_context(|| "read config".to_string())?;
    let cfg: AppConfig = toml::from_str(&s).with_context(|| "parse TOML".to_string())?;
    Ok(cfg)
}

// ---------- Chats storage ----------
fn data_root() -> Result<PathBuf> {
    let proj = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve data dir"))?;
    Ok(proj.data_dir().to_path_buf())
}

fn chats_dir_path() -> Result<PathBuf> {
    let dir = data_root()?.join("chats");
    fs::create_dir_all(&dir).with_context(|| "create chats dir".to_string())?;
    Ok(dir)
}

fn chat_file_path(id: &str) -> Result<PathBuf> { Ok(chats_dir_path()?.join(format!("{}.json", id))) }

fn list_chats() -> Result<Vec<ChatMeta>> {
    let mut metas = Vec::new();
    for entry in fs::read_dir(chats_dir_path()?)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        if let Some(ext) = entry.path().extension() { if ext != "json" { continue; } }
        let s = fs::read_to_string(entry.path());
        if let Ok(s) = s {
            if let Ok(chat) = serde_json::from_str::<Chat>(&s) {
                metas.push(ChatMeta { id: chat.id, title: chat.title, updated_ts: chat.updated_ts });
            }
        }
    }
    metas.sort_by(|a,b| b.updated_ts.cmp(&a.updated_ts));
    Ok(metas)
}

fn load_chat(id: &str) -> Result<Chat> {
    let p = chat_file_path(id)?;
    let s = fs::read_to_string(&p).with_context(|| "read chat".to_string())?;
    let chat: Chat = serde_json::from_str(&s).with_context(|| "parse chat json".to_string())?;
    Ok(chat)
}

fn save_chat(chat: &Chat) -> Result<()> {
    let p = chat_file_path(&chat.id)?;
    let mut f = File::create(&p).with_context(|| "create chat file".to_string())?;
    let s = serde_json::to_string_pretty(chat)?;
    f.write_all(s.as_bytes())?;
    Ok(())
}

fn gen_chat_id() -> String { format!("{}-{:08x}", now_sec(), random::<u32>()) }

fn derive_title(messages: &[Message]) -> String {
    let first = messages.iter().find(|m| matches!(m.role, Role::User)).or(messages.first());
    let raw = first.map(|m| m.content.trim()).unwrap_or("");
    let oneline = raw.lines().next().unwrap_or("").trim();
    let mut title = oneline.chars().take(60).collect::<String>();
    if title.is_empty() { title = "Untitled chat".to_string(); }
    title
}

// ---------- Ollama API payloads ----------
#[derive(Debug, Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<&'a JsonValue>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatStreamChunk { done: bool, #[serde(default)] error: Option<String>, #[serde(default)] message: Option<OllamaChatMessage> }
#[derive(Debug, Deserialize)]
struct OllamaChatMessage { role: String, content: String }

// ---------- App state ----------
struct App {
    // current chat
    current_chat_id: Option<String>,
    current_created_ts: Option<i64>,
    messages: Vec<Message>,
    pending_assistant: String,

    // sidebar
    chats: Vec<ChatMeta>,
    sidebar_idx: usize,

    // UI/input
    input: String,
    chat_scroll: u16,
    chat_inner_height: u16,
    sending: bool,
    model: String,
    api_url: String,
    options: Option<JsonValue>,
    quit: bool,
    mode: Mode,
    focus: Focus,
    selected_msg: Option<usize>,
}

impl App {
    fn new(model: String, api_url: String, options: Option<JsonValue>) -> Self {
        let chats = list_chats().unwrap_or_default();
        Self {
            current_chat_id: None,
            current_created_ts: None,
            messages: Vec::new(),
            pending_assistant: String::new(),
            chats,
            sidebar_idx: 0,
            input: String::new(),
            chat_scroll: 0,
            chat_inner_height: 0,
            sending: false,
            model,
            api_url,
            options,
            quit: false,
            mode: Mode::Normal,
            focus: Focus::Chat,
            selected_msg: None,
        }
    }
}

// ---------- Events ----------
#[derive(Debug)]
enum AppEvent { Tick, Input(KeyEvent), OllamaChunk(String), OllamaDone, OllamaError(String) }

// ---------- Ollama streaming ----------
async fn stream_ollama(
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    messages: Vec<Message>,
    tx: UnboundedSender<AppEvent>,
) {
    let client = reqwest::Client::new();
    let req = OllamaChatRequest { model: &model, messages: &messages, stream: true, options: options.as_ref() };

    let resp = match client.post(api_url).json(&req).send().await {
        Ok(r) => r,
        Err(e) => { let _ = tx.send(AppEvent::OllamaError(e.to_string())); return; }
    };

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_else(|_| "unknown error".to_string());
        let _ = tx.send(AppEvent::OllamaError(format!("HTTP error: {}", text)));
        return;
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                buf.extend_from_slice(&chunk);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.drain(..=pos).collect::<Vec<u8>>();
                    let line = &line[..line.len().saturating_sub(1)]; // drop '\n'
                    if line.is_empty() { continue; }
                    match serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                        Ok(obj) => {
                            if let Some(err) = obj.error { let _ = tx.send(AppEvent::OllamaError(err)); }
                            if let Some(msg) = obj.message { let _ = tx.send(AppEvent::OllamaChunk(msg.content)); }
                            if obj.done { let _ = tx.send(AppEvent::OllamaDone); }
                        }
                        Err(_) => { /* ignore malformed lines */ }
                    }
                }
            }
            Err(e) => { let _ = tx.send(AppEvent::OllamaError(e.to_string())); break; }
        }
    }
}

// ---------- Sanitization + Preview ----------
fn sanitize_for_html(input: &str) -> String {
    let mut out = input.replace('\u{2011}', "-");
    out = out.replace('<', "").replace('>', "");
    let re_display = Regex::new(r"(?s)\\\[\s*(.*?)\s*\\\]").unwrap();
    out = re_display.replace_all(&out, |caps: &Captures| format!("$$\n{}\n$$", &caps[1])).to_string();
    let re_trim_inline = Regex::new(r"\$\s+([^$]*?\S)\s+\$").unwrap();
    out = re_trim_inline.replace_all(&out, |caps: &Captures| format!("${}$", &caps[1])).to_string();
    let re_inline = Regex::new(r"\\\((.*?)\\\)").unwrap();
    out = re_inline.replace_all(&out, |caps: &Captures| format!("${}$", caps[1].trim())).to_string();
    out
}

async fn preview_to_html(content: String) -> Result<()> {
    let sanitized = sanitize_for_html(&content);
    let base = std::env::temp_dir();
    let stamp = now_sec();
    let in_path = base.join(format!("tuillama_{}_input.md", stamp));
    let out_path = base.join(format!("tuillama_{}_output.html", stamp));
    tokio::fs::write(&in_path, sanitized).await?;
    let status = Command::new("pandoc")
        .arg(&in_path).arg("-s").arg("--from").arg("markdown").arg("--to").arg("html").arg("--mathjax").arg("-o").arg(&out_path)
        .status().await?;
    if !status.success() { return Err(anyhow!("pandoc failed")); }
    let _ = Command::new("xdg-open").arg(&out_path).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    Ok(())
}

// ---------- UI ----------
fn draw_ui(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(frame.size());
    draw_sidebar(frame, chunks[0], app);
    draw_chat(frame, chunks[1], app);
}

fn draw_sidebar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let items: Vec<ListItem> = app
        .chats
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let ts = NaiveDateTime::from_timestamp_opt(c.updated_ts, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| c.updated_ts.to_string());
            let style = if app.focus == Focus::Sidebar && i == app.sidebar_idx {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(ts, Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(&c.title, Style::default().fg(Color::White)),
            ]))
            .style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                "Chats",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(list, area);
}

fn draw_chat(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    // track inner viewport height for minimal selection scrolling
    app.chat_inner_height = v_chunks[0].height.saturating_sub(2);

    let mut text = Text::default();
    let sel = app.selected_msg.unwrap_or_else(|| app.messages.len().saturating_sub(1));
    for (i, m) in app.messages.iter().enumerate() {
        let active = app.focus == Focus::Chat && app.mode == Mode::Normal && i == sel;
        let (prefix_text, mut pstyle) = match m.role {
            Role::User => ("You:".to_string(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Role::Assistant => (format!("{}:", app.model), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Role::System => ("System:".to_string(), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        };
        if active { pstyle = pstyle.add_modifier(Modifier::REVERSED); }
        text.push_line(Line::styled(prefix_text, pstyle));
        let content_style = if active { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
        for line in m.content.lines() {
            text.push_line(Line::styled(line.to_string(), content_style));
        }
        text.push_line(Line::from(""));
    }
    if !app.pending_assistant.is_empty() {
        let active = app.focus == Focus::Chat && app.mode == Mode::Normal && app.messages.len() == sel;
        let mut hdr_style = Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD);
        if active { hdr_style = hdr_style.add_modifier(Modifier::REVERSED); }
        text.push_line(Line::styled(format!("{}:", app.model), hdr_style));
        let mut line = app.pending_assistant.clone();
        line.push('▌');
        let content_style = if active { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
        text.push_line(Line::styled(line, content_style));
        text.push_line(Line::from(""));
    }

    let mode_span = match app.mode {
        Mode::Insert => Span::styled("[INSERT]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Mode::Normal => Span::styled("[NORMAL]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    };
    let mut title_spans = vec![Span::raw("Chat "), mode_span];
    if matches!(app.mode, Mode::Normal) && matches!(app.focus, Focus::Chat) {
        title_spans.push(Span::styled(" [FOCUS]", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)));
    }
    let chat_block = Block::default().borders(Borders::ALL).title(Line::from(title_spans));

    let scroll_y = app.chat_scroll;

    let messages = Paragraph::new(text)
        .block(chat_block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));
    frame.render_widget(messages, v_chunks[0]);

    let input_title_line: Line = match app.mode {
        Mode::Insert => Line::from(vec![
            Span::raw("Message "),
            Span::styled("[INSERT]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ]),
        Mode::Normal => {
            let base = if app.focus == Focus::Sidebar { "Sidebar " } else { "Navigation " };
            let hint = if app.focus == Focus::Sidebar {
                "(j/k nav, Enter load, l->chat, n=new)"
            } else {
                "(h->sidebar, j/k select, Enter preview, n=new)"
            };
            Line::from(vec![
                Span::raw(base),
                Span::styled("[NORMAL] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(hint),
            ])
        }
    };
    let bottom = match app.mode {
        Mode::Insert => app.input.as_str(),
        Mode::Normal => "h=sidebar  l=chat  j/k=move  n=new  Enter=load/preview  ↑/↓=scroll  q=quit",
    };
    let input = Paragraph::new(bottom)
        .block(Block::default().borders(Borders::ALL).title(input_title_line));
    frame.render_widget(input, v_chunks[1]);

    if app.mode == Mode::Insert && app.focus == Focus::Chat {
        frame.set_cursor(v_chunks[1].x + 1 + app.input.len() as u16, v_chunks[1].y + 1);
    }
}

// ---------- Main ----------
#[tokio::main]
async fn main() -> Result<()> {
    let cfg_path = config_path()?; let cfg = load_config(&cfg_path).unwrap_or_default();
    let model = std::env::var("OLLAMA_MODEL").ok().or_else(|| cfg.model.clone()).unwrap_or_else(|| "llama3".to_string());
    let api_url = std::env::var("OLLAMA_CHAT_URL").ok().or_else(|| cfg.api_url.clone()).unwrap_or_else(|| "http://localhost:11434/api/chat".to_string());
    let options: Option<JsonValue> = cfg.options.as_ref().and_then(|t| serde_json::to_value(t).ok());

    let mut app = App::new(model, api_url, options);

    enable_raw_mode()?; let mut stdout = std::io::stdout(); execute!(stdout, EnterAlternateScreen)?; let backend = CrosstermBackend::new(stdout); let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx): (UnboundedSender<AppEvent>, UnboundedReceiver<AppEvent>) = unbounded_channel();

    let tx_input = tx.clone();
    std::thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(250)).unwrap_or(false) {
                if let Ok(CEvent::Key(key)) = event::read() {
                    let _ = tx_input.send(AppEvent::Input(key));
                }
            }
        }
    });

    let tx_tick = tx.clone();
    tokio::spawn(async move { let mut interval = tokio::time::interval(Duration::from_millis(33)); loop { interval.tick().await; let _ = tx_tick.send(AppEvent::Tick); } });

    loop {
        terminal.draw(|f| draw_ui(f, &mut app))?;
        if let Some(ev) = rx.recv().await {
            match ev {
                AppEvent::Tick => {}
                AppEvent::Input(key) => handle_key(key, &mut app, &tx).await?,
                AppEvent::OllamaChunk(delta) => { app.pending_assistant.push_str(&delta); },
                AppEvent::OllamaDone => {
                    let content = std::mem::take(&mut app.pending_assistant);
                    app.messages.push(Message { role: Role::Assistant, content: content.clone() });
                    persist_current_chat(&mut app)?;
                    app.sending = false;
                    if app.mode == Mode::Normal && app.selected_msg.is_none() { app.selected_msg = Some(app.messages.len().saturating_sub(1)); }
                }
                AppEvent::OllamaError(e) => {
                    app.pending_assistant.clear(); app.sending = false; app.messages.push(Message { role: Role::System, content: format!("Error: {}", e) });
                }
            }
        }
        if app.quit { break; }
    }

    disable_raw_mode()?; execute!(terminal.backend_mut(), LeaveAlternateScreen)?; terminal.show_cursor()?; Ok(())
}

fn now_sec() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
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
    let chat = Chat { id: id.clone(), title, created_ts: created, updated_ts: now, messages: app.messages.clone() };
    save_chat(&chat)?;
    app.chats = list_chats().unwrap_or_default();
    if let Some(idx) = app.chats.iter().position(|c| c.id == id) { app.sidebar_idx = idx; }
    Ok(())
}

fn start_new_chat(app: &mut App) -> Result<()> {
    let now = now_sec();
    let id = gen_chat_id();
    app.current_chat_id = Some(id.clone());
    app.current_created_ts = Some(now);
    app.messages.clear();
    app.pending_assistant.clear();
    app.selected_msg = None;
    app.chat_scroll = 0;
    app.chat_inner_height = 0;
    let chat = Chat { id: id.clone(), title: "Untitled chat".to_string(), created_ts: now, updated_ts: now, messages: Vec::new() };
    save_chat(&chat)?;
    app.chats = list_chats().unwrap_or_default();
    if let Some(idx) = app.chats.iter().position(|c| c.id == id) { app.sidebar_idx = idx; }
    Ok(())
}

fn offset_for_message(messages: &[Message], idx: usize) -> u16 {
    let mut y: u16 = 0;
    for m in &messages[..idx.min(messages.len())] {
        y = y.saturating_add(1); // header
        y = y.saturating_add(m.content.lines().count() as u16); // content
        y = y.saturating_add(1); // spacer
    }
    y
}

async fn handle_key(key: KeyEvent, app: &mut App, tx: &UnboundedSender<AppEvent>) -> Result<()> {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { app.quit = true; return Ok(()); }
    if key.code == KeyCode::Char('q') && app.mode == Mode::Normal { app.quit = true; return Ok(()); }

    match app.mode {
        Mode::Insert => match key.code {
            KeyCode::Esc => { app.mode = Mode::Normal; }
            KeyCode::Enter => {
                if app.sending { return Ok(()); }
                let input = app.input.trim().to_string(); if input.is_empty() { return Ok(()); }
                app.input.clear();
                app.messages.push(Message { role: Role::User, content: input.clone() });
                persist_current_chat(app)?;
                app.sending = true;
                let mut convo = app.messages.clone();
                convo.retain(|m| !matches!(m.role, Role::System) || !m.content.starts_with("Error:"));
                let api_url = app.api_url.clone(); let model = app.model.clone(); let options = app.options.clone(); let tx2 = tx.clone();
                tokio::spawn(async move { stream_ollama(api_url, model, options, convo, tx2).await; });
            }
            KeyCode::Backspace => { app.input.pop(); }
            KeyCode::Char(c) => { app.input.push(c); }
            KeyCode::Tab => { app.input.push('\t'); }
            KeyCode::Up => { app.chat_scroll = app.chat_scroll.saturating_add(1); }
            KeyCode::Down => { app.chat_scroll = app.chat_scroll.saturating_sub(1); }
            _ => {}
        },
        Mode::Normal => {
            match key.code {
                KeyCode::Char('h') => { app.focus = Focus::Sidebar; }
                KeyCode::Char('l') => { app.focus = Focus::Chat; }
                KeyCode::Char('i') => { if app.focus == Focus::Chat { app.mode = Mode::Insert; } }
                KeyCode::Char('n') => { start_new_chat(app)?; }
                KeyCode::Char('j') => {
                    match app.focus {
                        Focus::Sidebar => { if !app.chats.is_empty() { app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len()-1); } }
                        Focus::Chat => {
                            if app.messages.is_empty() { return Ok(()); }
                            let cur = app.selected_msg.unwrap_or_else(|| app.messages.len().saturating_sub(1));
                            let next = (cur + 1).min(app.messages.len().saturating_sub(1));
                            app.selected_msg = Some(next);
                            // minimal selection scrolling: align to start if off-screen
                            let target_y = offset_for_message(&app.messages, next);
                            let top = app.chat_scroll;
                            let bottom = top.saturating_add(app.chat_inner_height.max(1));
                            if target_y < top || target_y >= bottom {
                                app.chat_scroll = target_y;
                            }
                        }
                    }
                }
                KeyCode::Char('k') => {
                    match app.focus {
                        Focus::Sidebar => { if !app.chats.is_empty() { app.sidebar_idx = app.sidebar_idx.saturating_sub(1); } }
                        Focus::Chat => {
                            if app.messages.is_empty() { return Ok(()); }
                            let cur = app.selected_msg.unwrap_or_else(|| app.messages.len().saturating_sub(1));
                            let next = cur.saturating_sub(1);
                            app.selected_msg = Some(next);
                            // minimal selection scrolling: align to start if off-screen
                            let target_y = offset_for_message(&app.messages, next);
                            let top = app.chat_scroll;
                            let bottom = top.saturating_add(app.chat_inner_height.max(1));
                            if target_y < top || target_y >= bottom {
                                app.chat_scroll = target_y;
                            }
                        }
                    }
                }
                KeyCode::Enter => {
                    match app.focus {
                        Focus::Sidebar => {
                            if app.chats.is_empty() { return Ok(()); }
                            let meta = &app.chats[app.sidebar_idx];
                            if let Ok(chat) = load_chat(&meta.id) {
                                app.current_chat_id = Some(chat.id.clone());
                                app.current_created_ts = Some(chat.created_ts);
                                app.messages = chat.messages;
                                app.selected_msg = Some(app.messages.len().saturating_sub(1));
                                app.chat_scroll = offset_for_message(&app.messages, app.selected_msg.unwrap_or(0));
                                app.focus = Focus::Chat;
                            }
                        }
                        Focus::Chat => {
                            if let Some(i) = app.selected_msg { if let Some(m) = app.messages.get(i) {
                                let tx2 = tx.clone(); let content = m.content.clone();
                                tokio::spawn(async move { if let Err(e) = preview_to_html(content).await { let _ = tx2.send(AppEvent::OllamaError(format!("Preview error: {}", e))); } });
                            }}
                        }
                    }
                }
                KeyCode::Up => { app.chat_scroll = app.chat_scroll.saturating_add(1); }
                KeyCode::Down => { app.chat_scroll = app.chat_scroll.saturating_sub(1); }
                _ => {}
            }
        }
    }
    Ok(())
}
