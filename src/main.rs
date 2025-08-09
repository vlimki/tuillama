use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
    str::FromStr,
};

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDateTime;
use crossterm::{
    cursor::SetCursorStyle,
    event::{self, Event as CEvent, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use directories::ProjectDirs;
use futures_util::StreamExt;
use rand::random;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Terminal,
};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

// syntect for code highlighting
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SynColor, FontStyle, ScopeSelectors, Style as SynStyle, StyleModifier, Theme as SynTheme, ThemeItem, ThemeSet
    },
    parsing::SyntaxSet,
};

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
enum Mode {
    Normal,
    Insert,
    Visual,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Chat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Popup {
    None,
    ConfirmDelete { id: String, title: String },
    Help,
}

// ---------- Theme ----------
#[derive(Clone, Debug)]
struct Theme {
    // sidebar
    sidebar_title: Color,
    sidebar_timestamp: Color,
    sidebar_item: Color,

    // titles
    title_chat: Color,
    title_input: Color,

    // prefixes
    user_prefix: Color,
    assistant_prefix: Color,
    system_prefix: Color,

    // markdown
    heading1: Color,
    heading2: Color,
    heading3: Color,
    heading4: Color,
    blockquote_bar: Color,
    list_bullet: Color,
    ordered_number: Color,
    inline_code: Color,
    inline_code_bg: Color,
    code_block_fg: Color,
    code_block_bg: Color,
    hr: Color,

    // status / mode
    status_hint: Color,
    mode_insert: Color,
    mode_normal: Color,
    mode_visual: Color,
    mode_focus: Color,

    // borders
    border_sidebar: Color,
    border_chat: Color,
    border_input: Color,

    // popup
    popup_title: Color,
    popup_accent: Color,
    popup_text: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            sidebar_title: Color::Cyan,
            sidebar_timestamp: Color::DarkGray,
            sidebar_item: Color::White,

            title_chat: Color::White,
            title_input: Color::White,

            user_prefix: Color::Cyan,
            assistant_prefix: Color::Magenta,
            system_prefix: Color::Yellow,

            heading1: Color::Yellow,
            heading2: Color::LightYellow,
            heading3: Color::Cyan,
            heading4: Color::LightCyan,
            blockquote_bar: Color::DarkGray,
            list_bullet: Color::Green,
            ordered_number: Color::Green,
            inline_code: Color::Yellow,
            inline_code_bg: Color::DarkGray,
            code_block_fg: Color::Gray,
            code_block_bg: Color::Black,
            hr: Color::DarkGray,

            status_hint: Color::Gray,
            mode_insert: Color::Green,
            mode_normal: Color::Yellow,
            mode_visual: Color::LightMagenta,
            mode_focus: Color::Blue,

            border_sidebar: Color::Gray,
            border_chat: Color::Gray,
            border_input: Color::Gray,

            popup_title: Color::Red,
            popup_accent: Color::Yellow,
            popup_text: Color::Gray,
        }
    }
}

fn parse_color_name(s: &str) -> Option<Color> {
    let t = s.trim().to_lowercase().replace('-', "_");
    let c = match t.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark_gray" | "dark_grey" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => {
            if let Some(hex) = t.strip_prefix('#') {
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Some(Color::Rgb(r, g, b));
                    }
                }
            }
            return None;
        }
    };
    Some(c)
}

fn color_from_cfg(tbl: &toml::value::Table, key: &str, default: Color) -> Color {
    tbl.get(key)
        .and_then(|v| v.as_str())
        .and_then(parse_color_name)
        .unwrap_or(default)
}

impl Theme {
    fn from_config(tbl: Option<&toml::value::Table>) -> Self {
        let mut t = Theme::default();
        if let Some(map) = tbl {
            t.sidebar_title = color_from_cfg(map, "sidebar_title", t.sidebar_title);
            t.sidebar_timestamp = color_from_cfg(map, "sidebar_timestamp", t.sidebar_timestamp);
            t.sidebar_item = color_from_cfg(map, "sidebar_item", t.sidebar_item);

            t.title_chat = color_from_cfg(map, "title_chat", t.title_chat);
            t.title_input = color_from_cfg(map, "title_input", t.title_input);

            t.user_prefix = color_from_cfg(map, "user_prefix", t.user_prefix);
            t.assistant_prefix = color_from_cfg(map, "assistant_prefix", t.assistant_prefix);
            t.system_prefix = color_from_cfg(map, "system_prefix", t.system_prefix);

            t.heading1 = color_from_cfg(map, "heading1", t.heading1);
            t.heading2 = color_from_cfg(map, "heading2", t.heading2);
            t.heading3 = color_from_cfg(map, "heading3", t.heading3);
            t.heading4 = color_from_cfg(map, "heading4", t.heading4);
            t.blockquote_bar = color_from_cfg(map, "blockquote_bar", t.blockquote_bar);
            t.list_bullet = color_from_cfg(map, "list_bullet", t.list_bullet);
            t.ordered_number = color_from_cfg(map, "ordered_number", t.ordered_number);
            t.inline_code = color_from_cfg(map, "inline_code", t.inline_code);
            t.inline_code_bg = color_from_cfg(map, "inline_code_bg", t.inline_code_bg);
            t.code_block_fg = color_from_cfg(map, "code_block_fg", t.code_block_fg);
            t.code_block_bg = color_from_cfg(map, "code_block_bg", t.code_block_bg);
            t.hr = color_from_cfg(map, "hr", t.hr);

            t.status_hint = color_from_cfg(map, "status_hint", t.status_hint);
            t.mode_insert = color_from_cfg(map, "mode_insert", t.mode_insert);
            t.mode_normal = color_from_cfg(map, "mode_normal", t.mode_normal);
            t.mode_visual = color_from_cfg(map, "mode_visual", t.mode_visual);
            t.mode_focus = color_from_cfg(map, "mode_focus", t.mode_focus);

            t.border_sidebar = color_from_cfg(map, "border_sidebar", t.border_sidebar);
            t.border_chat = color_from_cfg(map, "border_chat", t.border_chat);
            t.border_input = color_from_cfg(map, "border_input", t.border_input);

            t.popup_title = color_from_cfg(map, "popup_title", t.popup_title);
            t.popup_accent = color_from_cfg(map, "popup_accent", t.popup_accent);
            t.popup_text = color_from_cfg(map, "popup_text", t.popup_text);
        }
        t
    }
}

fn apply_selection(style: Style, selected: bool, bold_selection: bool) -> Style {
    if selected {
        if bold_selection {
            style.add_modifier(Modifier::BOLD)
        } else {
            style.add_modifier(Modifier::REVERSED)
        }
    } else {
        style
    }
}

// ---------- Config ----------
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct SyntaxSection {
    enabled: Option<bool>,
    theme_name: Option<String>,
    custom: Option<toml::value::Table>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Performance {
    stream_fps: Option<u64>,      // throttle redraws while streaming (default 30)
    input_poll_ms: Option<u64>,   // terminal input poll period (default 250)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AppConfig {
    model: Option<String>,
    api_url: Option<String>,
    options: Option<toml::value::Table>,
    system_prompt: Option<String>,
    bold_selection: Option<bool>,
    colors: Option<toml::value::Table>,
    syntax: Option<SyntaxSection>,
    syntax_theme: Option<String>, // legacy
    performance: Option<Performance>,
}

fn config_path() -> Result<PathBuf> {
    let proj = ProjectDirs::from("dev", "example", "tuillama")
        .ok_or_else(|| anyhow!("unable to resolve config dir"))?;
    Ok(proj.config_dir().join("config.toml"))
}

fn load_config(path: &PathBuf) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
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

fn chat_file_path(id: &str) -> Result<PathBuf> {
    Ok(chats_dir_path()?.join(format!("{}.json", id)))
}

fn list_chats() -> Result<Vec<ChatMeta>> {
    let mut metas = Vec::new();
    for entry in fs::read_dir(chats_dir_path()?)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(ext) = entry.path().extension() {
            if ext != "json" {
                continue;
            }
        }
        let s = fs::read_to_string(entry.path());
        if let Ok(s) = s {
            if let Ok(chat) = serde_json::from_str::<Chat>(&s) {
                metas.push(ChatMeta {
                    id: chat.id,
                    title: chat.title,
                    updated_ts: chat.updated_ts,
                });
            }
        }
    }
    metas.sort_by(|a, b| b.updated_ts.cmp(&a.updated_ts));
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

fn delete_chat_file(id: &str) -> Result<()> {
    let p = chat_file_path(id)?;
    if p.exists() {
        fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
    }
    Ok(())
}

fn gen_chat_id() -> String {
    format!("{}-{:08x}", now_sec(), random::<u32>())
}

fn derive_title(messages: &[Message]) -> String {
    let first = messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .or(messages.first());
    let raw = first.map(|m| m.content.trim()).unwrap_or("");
    let oneline = raw.lines().next().unwrap_or("").trim();
    let mut title = oneline.chars().take(60).collect::<String>();
    if title.is_empty() {
        title = "Untitled chat".to_string();
    }
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
struct OllamaChatStreamChunk {
    done: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<OllamaChatMessage>,
}
#[derive(Debug, Deserialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

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
    input_cursor: usize, // grapheme index
    chat_scroll: u16,
    chat_inner_height: u16,
    sending: bool,
    model: String,
    api_url: String,
    options: Option<JsonValue>,
    system_prompt: Option<String>,
    bold_selection: bool,
    theme: Theme,
    quit: bool,
    mode: Mode,
    focus: Focus,
    selected_msg: Option<usize>,
    popup: Popup,

    // syntect
    syntax_enabled: bool,
    syn_ss: SyntaxSet,
    syn_theme: SynTheme,

    // render cache
    render_cache: HashMap<(usize, u16), (u64, Text<'static>)>, // (msg_idx, width) -> (hash, rendered)
    pending_cache: Option<(u16, u64, Text<'static>)>,          // (width, hash, rendered)

    // redraw throttle
    last_draw: Instant,
    stream_throttle: Duration,
}

impl App {
    fn new(
        model: String,
        api_url: String,
        options: Option<JsonValue>,
        system_prompt: Option<String>,
        bold_selection: bool,
        theme: Theme,
        syntax_enabled: bool,
        syntax_theme_name: String,
        syntax_custom: Option<toml::value::Table>,
        stream_throttle: Duration,
    ) -> Self {
        let chats = list_chats().unwrap_or_default();

        // syntect assets
        let syn_ss = SyntaxSet::load_defaults_newlines();
        let syn_theme = build_syntect_theme(&syntax_theme_name, syntax_custom.as_ref());

        Self {
            current_chat_id: None,
            current_created_ts: None,
            messages: Vec::new(),
            pending_assistant: String::new(),
            chats,
            sidebar_idx: 0,
            input: String::new(),
            input_cursor: 0,
            chat_scroll: 0,
            chat_inner_height: 0,
            sending: false,
            model,
            api_url,
            options,
            system_prompt,
            bold_selection,
            theme,
            quit: false,
            mode: Mode::Normal,
            focus: Focus::Chat,
            selected_msg: None,
            popup: Popup::None,
            syntax_enabled,
            syn_ss,
            syn_theme,
            render_cache: HashMap::new(),
            pending_cache: None,
            last_draw: Instant::now(),
            stream_throttle,
        }
    }
}

// ---------- Events ----------
#[derive(Debug)]
enum AppEvent {
    Input(KeyEvent),
    OllamaChunk(String),
    OllamaDone,
    OllamaError(String),
}

// ---------- Ollama streaming ----------
async fn stream_ollama(
    api_url: String,
    model: String,
    options: Option<JsonValue>,
    mut messages: Vec<Message>,
    tx: UnboundedSender<AppEvent>,
) {
    let req = OllamaChatRequest {
        model: &model,
        messages: &messages,
        stream: true,
        options: options.as_ref(),
    };
    let client = reqwest::Client::new();

    let resp = match client.post(api_url).json(&req).send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(AppEvent::OllamaError(e.to_string()));
            return;
        }
    };

    if !resp.status().is_success() {
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_string());
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
                    let line = &line[..line.len().saturating_sub(1)];
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_slice::<OllamaChatStreamChunk>(line) {
                        Ok(obj) => {
                            if let Some(err) = obj.error {
                                let _ = tx.send(AppEvent::OllamaError(err));
                            }
                            if let Some(msg) = obj.message {
                                let _ = tx.send(AppEvent::OllamaChunk(msg.content));
                            }
                            if obj.done {
                                let _ = tx.send(AppEvent::OllamaDone);
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(AppEvent::OllamaError(e.to_string()));
                break;
            }
        }
    }
}

// ---------- Sanitization + Preview ----------
fn sanitize_for_html(input: &str) -> String {
    let mut out = input.replace('\u{2011}', "-"); // non-breaking hyphen -> hyphen
    out = out.replace('<', "").replace('>', "");
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

async fn preview_to_html(content: String) -> Result<()> {
    let sanitized = sanitize_for_html(&content);
    let base = std::env::temp_dir();
    let stamp = now_sec();
    let in_path = base.join(format!("tuillama_{}_input.md", stamp));
    let out_path = base.join(format!("tuillama_{}_output.html", stamp));
    tokio::fs::write(&in_path, sanitized).await?;
    let status = Command::new("pandoc")
        .arg(&in_path)
        .arg("-s")
        .arg("--from")
        .arg("markdown")
        .arg("--to")
        .arg("html")
        .arg("--mathjax")
        .arg("-o")
        .arg(&out_path)
        .status()
        .await?;
    if !status.success() {
        return Err(anyhow!("pandoc failed"));
    }
    let _ = Command::new("xdg-open")
        .arg(&out_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    Ok(())
}

// ---------- Clipboard helpers (xclip) ----------
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

// ---------- Markdown -> Text formatting ----------
#[derive(Clone, Copy, Default)]
struct InlineState {
    bold: bool,
    italic: bool,
    code: bool,
}

fn style_from_state(s: InlineState, theme: &Theme) -> Style {
    let mut st = Style::default();
    if s.bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.italic {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.code {
        st = st.fg(theme.inline_code).bg(theme.inline_code_bg);
    }
    st
}

fn stylize_inline(input: &str, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = input.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut st = InlineState::default();
    let mut i = 0usize;

    let mut push_buf = |spans: &mut Vec<Span>, buf: &mut String, st: InlineState| {
        if !buf.is_empty() {
            spans.push(Span::styled(buf.clone(), style_from_state(st, theme)));
            buf.clear();
        }
    };

    while i < chars.len() {
        if chars[i] == '`' {
            push_buf(&mut spans, &mut buf, st);
            st.code = !st.code;
            i += 1;
            continue;
        }
        if !st.code {
            if i + 1 < chars.len()
                && ((chars[i] == '*' && chars[i + 1] == '*')
                    || (chars[i] == '_' && chars[i + 1] == '_'))
            {
                push_buf(&mut spans, &mut buf, st);
                st.bold = !st.bold;
                i += 2;
                continue;
            }
            if chars[i] == '*' || chars[i] == '_' {
                push_buf(&mut spans, &mut buf, st);
                st.italic = !st.italic;
                i += 1;
                continue;
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    push_buf(&mut spans, &mut buf, st);
    spans
}

fn syn_to_color(c: SynColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn render_markdown_to_text(
    input: &str,
    theme: &Theme,
    inner_width: u16,
    ss: &SyntaxSet,
    syn_theme: &SynTheme,
    syntax_enabled: bool,
) -> Text<'static> {
    let mut text = Text::default();
    let mut in_code_block = false;
    let mut high: Option<HighlightLines> = None;
    let hr_line_len = inner_width.max(1) as usize;
    const NBSP: char = '\u{00A0}';

    for raw in input.lines() {
        let trimmed = raw.trim_start();

        // Fenced code start/end with optional language tag
        if trimmed.starts_with("```") {
            if in_code_block {
                in_code_block = false;
                high = None;
            } else {
                in_code_block = true;
                let lang = trimmed.trim_start_matches("```").trim();
                if syntax_enabled {
                    let syn = if lang.is_empty() {
                        ss.find_syntax_plain_text()
                    } else {
                        ss.find_syntax_by_token(lang).unwrap_or_else(|| ss.find_syntax_plain_text())
                    };
                    high = Some(HighlightLines::new(syn, syn_theme));
                } else {
                    high = None;
                }
            }
            continue;
        }

        if in_code_block {
            let style_bg = Style::default().bg(theme.code_block_bg);
            let w = inner_width.max(1) as usize;

            // Empty line -> full-width padding to preserve background
            if raw.is_empty() {
                let fill: String = std::iter::repeat(NBSP).take(w).collect();
                text.push_line(Line::styled(fill, style_bg.fg(theme.code_block_fg)));
                continue;
            }

            if let Some(h) = high.as_mut() {
                let ranges = h.highlight_line(raw, ss).unwrap_or_default();

                // Collect styled segments as owned strings
                let mut segs: Vec<(Style, String)> = Vec::with_capacity(ranges.len());
                for (sty, seg) in ranges {
                    if seg.is_empty() {
                        continue;
                    }
                    let st = style_bg.fg(syn_to_color(sty.foreground));
                    segs.push((st, seg.to_string()));
                }

                // Hard-wrap by visible width, preserving styles
                let mut cur_line: Vec<Span<'static>> = Vec::new();
                let mut cur_w = 0usize;

                for (st, seg) in segs {
                    for g in UnicodeSegmentation::graphemes(seg.as_str(), true) {
                        let gw = UnicodeWidthStr::width(g);

                        if gw > w {
                            if !cur_line.is_empty() || cur_w > 0 {
                                let pad_len = w.saturating_sub(cur_w);
                                if pad_len > 0 {
                                    let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                                    cur_line.push(Span::styled(pad, style_bg.fg(theme.code_block_fg)));
                                }
                                text.push_line(Line::from(std::mem::take(&mut cur_line)));
                                cur_w = 0;
                            }
                            let mut spans: Vec<Span<'static>> = Vec::new();
                            spans.push(Span::styled(g.to_string(), st));
                            let pad_len = w.saturating_sub(gw.min(w));
                            if pad_len > 0 {
                                let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                                spans.push(Span::styled(pad, style_bg.fg(theme.code_block_fg)));
                            }
                            text.push_line(Line::from(spans));
                            continue;
                        }

                        if cur_w + gw > w {
                            let pad_len = w.saturating_sub(cur_w);
                            if pad_len > 0 {
                                let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                                cur_line.push(Span::styled(pad, style_bg.fg(theme.code_block_fg)));
                            }
                            text.push_line(Line::from(std::mem::take(&mut cur_line)));
                            cur_w = 0;
                        }

                        cur_line.push(Span::styled(g.to_string(), st));
                        cur_w += gw;
                    }
                }

                // flush last visual line
                let pad_len = w.saturating_sub(cur_w);
                if pad_len > 0 {
                    let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                    cur_line.push(Span::styled(pad, style_bg.fg(theme.code_block_fg)));
                }
                text.push_line(Line::from(cur_line));
            } else {
                // fallback (plain)
                let mut line = raw.replace('\t', "    ");
                let cur_w = UnicodeWidthStr::width(line.as_str());
                if cur_w < w {
                    let pad: String = std::iter::repeat(NBSP).take(w - cur_w).collect();
                    line.push_str(&pad);
                }
                text.push_line(Line::styled(line, style_bg.fg(theme.code_block_fg)));
            }
            continue;
        }

        // Non-code markdown enhancements
        if !trimmed.is_empty() && trimmed.chars().all(|c| c == '-') && trimmed.len() >= 3 {
            let hr = "─".repeat(hr_line_len);
            text.push_line(Line::styled(hr, Style::default().fg(theme.hr)));
            continue;
        }

        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if hashes > 0 && trimmed.chars().nth(hashes) == Some(' ') {
            let content = trimmed[hashes + 1..].to_string();
            let style = match hashes {
                1 => Style::default()
                    .fg(theme.heading1)
                    .add_modifier(Modifier::BOLD),
                2 => Style::default()
                    .fg(theme.heading2)
                    .add_modifier(Modifier::BOLD),
                3 => Style::default()
                    .fg(theme.heading3)
                    .add_modifier(Modifier::BOLD),
                4 => Style::default()
                    .fg(theme.heading4)
                    .add_modifier(Modifier::BOLD),
                _ => Style::default()
                    .fg(theme.heading4)
                    .add_modifier(Modifier::BOLD),
            };
            text.push_line(Line::styled(content, style));
            continue;
        }

        if trimmed.starts_with("> ") {
            let inner = &trimmed[2..];
            let mut spans = vec![Span::styled(
                "▏ ",
                Style::default().fg(theme.blockquote_bar),
            )];
            spans.extend(stylize_inline(inner, theme));
            text.push_line(Line::from(spans));
            continue;
        }

        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let inner = &trimmed[2..];
            let mut spans = vec![Span::styled(
                "• ",
                Style::default().fg(theme.list_bullet),
            )];
            spans.extend(stylize_inline(inner, theme));
            text.push_line(Line::from(spans));
            continue;
        }

        if let Some(pos) = trimmed.find(". ") {
            if trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                let num = &trimmed[..pos + 1];
                let rest = &trimmed[pos + 2..];
                let mut spans = vec![Span::styled(
                    format!("{} ", num),
                    Style::default().fg(theme.ordered_number),
                )];
                spans.extend(stylize_inline(rest, theme));
                text.push_line(Line::from(spans));
                continue;
            }
        }

        text.push_line(Line::from(stylize_inline(raw, theme)));
    }

    text
}

// ---------- Custom syntect theme builder ----------
fn syn_color_from_str(s: &str) -> Option<SynColor> {
    let t = s.trim().to_lowercase().replace('-', "_");
    let c = match t.as_str() {
        "black" => SynColor { r: 0, g: 0, b: 0, a: 0xFF },
        "red" => SynColor { r: 0xFF, g: 0, b: 0, a: 0xFF },
        "green" => SynColor { r: 0, g: 0x80, b: 0, a: 0xFF },
        "yellow" => SynColor { r: 0xFF, g: 0xFF, b: 0, a: 0xFF },
        "blue" => SynColor { r: 0, g: 0, b: 0xFF, a: 0xFF },
        "magenta" => SynColor { r: 0xFF, g: 0, b: 0xFF, a: 0xFF },
        "cyan" => SynColor { r: 0, g: 0xFF, b: 0xFF, a: 0xFF },
        "white" => SynColor { r: 0xFF, g: 0xFF, b: 0xFF, a: 0xFF },
        "gray" | "grey" => SynColor { r: 0x80, g: 0x80, b: 0x80, a: 0xFF },
        "dark_gray" | "dark_grey" => SynColor { r: 0x40, g: 0x40, b: 0x40, a: 0xFF },
        "light_red" => SynColor { r: 0xFF, g: 0x66, b: 0x66, a: 0xFF },
        "light_green" => SynColor { r: 0x66, g: 0xFF, b: 0x66, a: 0xFF },
        "light_yellow" => SynColor { r: 0xFF, g: 0xFF, b: 0x99, a: 0xFF },
        "light_blue" => SynColor { r: 0x66, g: 0x99, b: 0xFF, a: 0xFF },
        "light_magenta" => SynColor { r: 0xFF, g: 0x66, b: 0xFF, a: 0xFF },
        "light_cyan" => SynColor { r: 0x99, g: 0xFF, b: 0xFF, a: 0xFF },
        _ => {
            if let Some(hex) = t.strip_prefix('#') {
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Some(SynColor { r, g, b, a: 0xFF });
                    }
                }
            }
            return None;
        }
    };
    Some(c)
}

fn theme_item_for(scope_sel: &str, color: SynColor) -> Option<ThemeItem> {
    let sels = ScopeSelectors::from_str(scope_sel).ok()?;
    let style = StyleModifier {
        foreground: Some(color),
        background: Some(SynColor { r: 0, g: 0, b: 0, a: 0 }),
        font_style: Some(FontStyle::empty()),
    };
    Some(ThemeItem { scope: sels, style })
}

fn build_custom_syn_theme(tbl: &toml::value::Table) -> Option<SynTheme> {
    let mut items: Vec<ThemeItem> = Vec::new();

    let map = |key: &str, scopes: &str, items: &mut Vec<ThemeItem>| {
        if let Some(v) = tbl.get(key).and_then(|v| v.as_str()).and_then(syn_color_from_str) {
            if let Some(item) = theme_item_for(scopes, v) {
                items.push(item);
            }
        }
    };

    map("keyword", "keyword, storage, keyword.operator", &mut items);
    map("string", "string", &mut items);
    map("comment", "comment", &mut items);
    map("type", "storage.type, support.type, entity.name.type", &mut items);
    map("function", "entity.name.function, support.function", &mut items);
    map("number", "constant.numeric", &mut items);
    map("operator", "keyword.operator", &mut items);
    map("punct", "punctuation", &mut items);
    map("text", "text, source", &mut items);

    if items.is_empty() {
        return None;
    }

    let mut th = SynTheme::default();
    th.scopes = items;
    Some(th)
}

fn build_syntect_theme(theme_name: &str, custom: Option<&toml::value::Table>) -> SynTheme {
    if let Some(tbl) = custom {
        if let Some(t) = build_custom_syn_theme(tbl) {
            return t;
        }
    }
    let ts = ThemeSet::load_defaults();
    ts.themes
        .get(theme_name)
        .or_else(|| ts.themes.get("base16-ocean.dark"))
        .cloned()
        .unwrap_or_else(|| ts.themes.values().next().cloned().unwrap_or_default())
}

// ---------- Cache helpers ----------
fn role_tag(r: &Role) -> u8 {
    match r {
        Role::User => 1,
        Role::Assistant => 2,
        Role::System => 3,
    }
}

fn message_hash(m: &Message) -> u64 {
    let mut h = DefaultHasher::new();
    role_tag(&m.role).hash(&mut h);
    m.content.hash(&mut h);
    h.finish()
}

fn str_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn clone_with_modifier(mut t: Text<'static>, m: Modifier) -> Text<'static> {
    for line in &mut t.lines {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(m);
        }
    }
    t
}

// ---------- UI helpers ----------
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

fn draw_ui(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(frame.size());
    draw_sidebar(frame, chunks[0], app);
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
                "  Type, Enter: send to Ollama, Esc: back to NORMAL",
            ));
            lines.push(Line::from(
                "  ←/→: move cursor   Backspace: delete previous grapheme",
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
            let ts = NaiveDateTime::from_timestamp_opt(c.updated_ts, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| c.updated_ts.to_string());
            let selected = app.focus == Focus::Sidebar && i == app.sidebar_idx;
            let mut spans = vec![
                Span::styled(ts, Style::default().fg(app.theme.sidebar_timestamp)),
                Span::raw("  "),
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
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
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

    if !app.pending_assistant.is_empty() {
        let selected = app.focus == Focus::Chat && app.mode == Mode::Visual && app.messages.len() == sel;
        let mut hdr_style = Style::default()
            .fg(app.theme.assistant_prefix)
            .add_modifier(Modifier::BOLD);
        hdr_style = apply_selection(hdr_style, selected, app.bold_selection);
        text.push_line(Line::styled(format!("{}:", app.model), hdr_style));

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
    let chat_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border_chat))
        .title(Line::from(title_spans));

    let messages = Paragraph::new(text)
        .block(chat_block)
        .wrap(Wrap { trim: false })
        .scroll((app.chat_scroll, 0));
    frame.render_widget(messages, v_chunks[0]);

    // Status/input bar
    let input_title_line: Line = match app.mode {
        Mode::Insert => Line::from(vec![
            Span::styled(
                "Message ",
                Style::default()
                    .fg(app.theme.title_input)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "[INSERT]",
                Style::default()
                    .fg(app.theme.mode_insert)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Mode::Normal => {
            let base = if app.focus == Focus::Sidebar {
                "Sidebar "
            } else {
                "Navigation "
            };
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
            ])
        }
        Mode::Visual => {
            let base = if app.focus == Focus::Sidebar {
                "Sidebar "
            } else {
                "Navigation "
            };
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
            ])
        }
    };

    // Always show current input in the box across modes
    let input = Paragraph::new(app.input.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.border_input))
            .title(input_title_line),
    );
    frame.render_widget(input, v_chunks[1]);

    // Cursor (as thin bar) only in INSERT mode within chat focus
    if app.mode == Mode::Insert && app.focus == Focus::Chat {
        let visual_x = input_visual_x(&app.input, app.input_cursor) as u16;
        frame.set_cursor(v_chunks[1].x + 1 + visual_x, v_chunks[1].y + 1);
    }
}

// visual width of input up to grapheme index
fn input_visual_x(s: &str, gidx: usize) -> usize {
    let mut w = 0usize;
    for (i, g) in UnicodeSegmentation::graphemes(s, true).enumerate() {
        if i >= gidx {
            break;
        }
        w += UnicodeWidthStr::width(g);
    }
    w
}

// byte index of grapheme boundary
fn byte_index_from_grapheme(s: &str, gidx: usize) -> usize {
    let mut it = s.grapheme_indices(true).map(|(b, _)| b).collect::<Vec<_>>();
    it.push(s.len());
    it.get(gidx).copied().unwrap_or(s.len())
}

// ---------- Main ----------
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

    let mut app = App::new(
        model,
        api_url,
        options,
        system_prompt,
        bold_selection,
        theme,
        syntax_enabled,
        syntax_theme_name,
        syntax_custom,
        stream_throttle,
    );

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, SetCursorStyle::SteadyBar)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx): (UnboundedSender<AppEvent>, UnboundedReceiver<AppEvent>) = unbounded_channel();

    // Blocking input reader thread with configurable poll period
    let tx_input = tx.clone();
    std::thread::spawn(move || loop {
        if event::poll(Duration::from_millis(input_poll_ms)).unwrap_or(false) {
            if let Ok(CEvent::Key(key)) = event::read() {
                let _ = tx_input.send(AppEvent::Input(key));
            }
        }
    });

    // Initial draw once (no idle redraw loop)
    terminal.draw(|f| draw_ui(f, &mut app))?;
    app.last_draw = Instant::now();

    // Event-driven UI loop with streaming redraw throttle
    while let Some(ev) = rx.recv().await {
        let mut should_draw = true;

        match ev {
            AppEvent::Input(key) => handle_key(key, &mut app, &tx).await?,
            AppEvent::OllamaChunk(delta) => {
                app.pending_assistant.push_str(&delta);
                app.pending_cache = None;
                // throttle frequent redraws while streaming
                if app.last_draw.elapsed() < app.stream_throttle {
                    should_draw = false;
                }
            }
            AppEvent::OllamaDone => {
                let content = std::mem::take(&mut app.pending_assistant);
                app.pending_cache = None;
                app.messages.push(Message {
                    role: Role::Assistant,
                    content: content.clone(),
                });
                persist_current_chat(&mut app)?;
                app.sending = false;
                if matches!(app.mode, Mode::Visual) && app.selected_msg.is_none() {
                    app.selected_msg = Some(app.messages.len().saturating_sub(1));
                }
            }
            AppEvent::OllamaError(e) => {
                app.pending_assistant.clear();
                app.pending_cache = None;
                app.sending = false;
                app.messages.push(Message {
                    role: Role::System,
                    content: format!("Error: {}", e),
                });
            }
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

fn now_sec() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// These estimations are used only for G/top/bottom and selection auto-adjust; they don't account for markdown wrapping.
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
    Ok(())
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
    Ok(())
}

async fn handle_key(key: KeyEvent, app: &mut App, tx: &UnboundedSender<AppEvent>) -> Result<()> {
    match &app.popup {
        Popup::ConfirmDelete { id, .. } => match key.code {
            KeyCode::Char('y') => {
                let del_id = id.clone();
                delete_chat_file(&del_id)?;
                if app.current_chat_id.as_deref() == Some(&del_id) {
                    app.current_chat_id = None;
                    app.current_created_ts = None;
                    app.messages.clear();
                    app.pending_assistant.clear();
                    app.selected_msg = None;
                    app.chat_scroll = 0;
                    app.render_cache.clear();
                    app.pending_cache = None;
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

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return Ok(());
    }
    if key.code == KeyCode::Char('q') && app.mode == Mode::Normal {
        app.quit = true;
        return Ok(());
    }

    match app.mode {
        Mode::Insert => match key.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                if app.sending {
                    return Ok(());
                }
                let input = app.input.trim().to_string();
                if input.is_empty() {
                    return Ok(());
                }
                app.input.clear();
                app.input_cursor = 0;
                app.messages.push(Message {
                    role: Role::User,
                    content: input.clone(),
                });
                app.render_cache.remove(&(app.messages.len() - 1, app.chat_inner_height));
                persist_current_chat(app)?;
                app.sending = true;
                let mut convo = app.messages.clone();
                convo.retain(|m| !matches!(m.role, Role::System) || !m.content.starts_with("Error:"));
                if !convo.iter().any(|m| matches!(m.role, Role::System)) {
                    if let Some(sp) = app.system_prompt.clone() {
                        convo.insert(
                            0,
                            Message {
                                role: Role::System,
                                content: sp,
                            },
                        );
                    }
                }
                let api_url = app.api_url.clone();
                let model = app.model.clone();
                let options = app.options.clone();
                let tx2 = tx.clone();
                tokio::spawn(async move { stream_ollama(api_url, model, options, convo, tx2).await; });
            }
            KeyCode::Backspace => {
                if app.input_cursor > 0 {
                    let start = byte_index_from_grapheme(&app.input, app.input_cursor - 1);
                    let end = byte_index_from_grapheme(&app.input, app.input_cursor);
                    app.input.replace_range(start..end, "");
                    app.input_cursor -= 1;
                }
            }
            KeyCode::Left => {
                if app.input_cursor > 0 {
                    app.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                let gcount = app.input.graphemes(true).count();
                if app.input_cursor < gcount {
                    app.input_cursor += 1;
                }
            }
            KeyCode::Char(c) => {
                let pos = byte_index_from_grapheme(&app.input, app.input_cursor);
                app.input.insert(pos, c);
                app.input_cursor += 1;
            }
            KeyCode::Tab => {
                let pos = byte_index_from_grapheme(&app.input, app.input_cursor);
                app.input.insert(pos, '\t');
                app.input_cursor += 1;
            }
            KeyCode::Up => {
                app.chat_scroll = app.chat_scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                app.chat_scroll = app.chat_scroll.saturating_add(1);
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
                    app.focus = Focus::Sidebar;
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
                        app.input_cursor = app.input.graphemes(true).count();
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
                KeyCode::Char('p') => {
                    if app.focus == Focus::Chat {
                        match read_clipboard_text().await {
                            Ok(clip) => {
                                app.input.push_str(&clip);
                                app.input_cursor = app.input.graphemes(true).count();
                            }
                            Err(e) => {
                                app.messages.push(Message {
                                    role: Role::System,
                                    content: format!("Clipboard paste failed: {}", e),
                                });
                                app.render_cache.clear();
                            }
                        }
                    }
                }
                KeyCode::Char('j') => match app.focus {
                    Focus::Sidebar => {
                        if !app.chats.is_empty() {
                            app.sidebar_idx = (app.sidebar_idx + 1).min(app.chats.len() - 1);
                        }
                    }
                    Focus::Chat => {
                        app.chat_scroll = app.chat_scroll.saturating_add(1);
                    }
                },
                KeyCode::Char('k') => match app.focus {
                    Focus::Sidebar => {
                        if !app.chats.is_empty() {
                            app.sidebar_idx = app.sidebar_idx.saturating_sub(1);
                        }
                    }
                    Focus::Chat => {
                        app.chat_scroll = app.chat_scroll.saturating_sub(1);
                    }
                },
                KeyCode::Char('g') => {
                    if app.focus == Focus::Chat {
                        app.chat_scroll = 0;
                    }
                }
                KeyCode::Char('G') => {
                    if app.focus == Focus::Chat {
                        let total = content_total_height(
                            &app.messages,
                            if app.pending_assistant.is_empty() {
                                None
                            } else {
                                Some(app.pending_assistant.as_str())
                            },
                        );
                        app.chat_scroll = total.saturating_sub(app.chat_inner_height);
                    }
                }
                KeyCode::Enter => match app.focus {
                    Focus::Sidebar => {
                        if app.chats.is_empty() {
                            return Ok(());
                        }
                        let meta = &app.chats[app.sidebar_idx];
                        if let Ok(chat) = load_chat(&meta.id) {
                            app.current_chat_id = Some(chat.id.clone());
                            app.current_created_ts = Some(chat.created_ts);
                            app.messages = chat.messages;
                            app.render_cache.clear();
                            app.pending_cache = None;
                            let total = content_total_height(&app.messages, None);
                            app.chat_scroll = total.saturating_sub(app.chat_inner_height);
                            app.focus = Focus::Chat;
                        }
                    }
                    Focus::Chat => {}
                },
                KeyCode::Up => {
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
                app.focus = Focus::Sidebar;
            }
            KeyCode::Char('l') => {
                app.focus = Focus::Chat;
            }
            KeyCode::Char('i') => {
                if app.focus == Focus::Chat {
                    app.mode = Mode::Insert;
                    app.input_cursor = app.input.graphemes(true).count();
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
                                });
                                app.render_cache.clear();
                            }
                        }
                    }
                }
            }
            KeyCode::Char('j') => match app.focus {
                Focus::Sidebar => {
                    if !app.chats.is_empty() {
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
                    let target_y = offset_for_message(&app.messages, next);
                    let top = app.chat_scroll;
                    let bottom = top.saturating_add(app.chat_inner_height.max(1));
                    if target_y < top || target_y >= bottom {
                        app.chat_scroll = target_y;
                    }
                }
            },
            KeyCode::Char('k') => match app.focus {
                Focus::Sidebar => {
                    if !app.chats.is_empty() {
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
                    let target_y = offset_for_message(&app.messages, next);
                    let top = app.chat_scroll;
                    let bottom = top.saturating_add(app.chat_inner_height.max(1));
                    if target_y < top || target_y >= bottom {
                        app.chat_scroll = target_y;
                    }
                }
            },
            KeyCode::Enter => match app.focus {
                Focus::Sidebar => {
                    if app.chats.is_empty() {
                        return Ok(());
                    }
                    let meta = &app.chats[app.sidebar_idx];
                    if let Ok(chat) = load_chat(&meta.id) {
                        app.current_chat_id = Some(chat.id.clone());
                        app.current_created_ts = Some(chat.created_ts);
                        app.messages = chat.messages;
                        app.render_cache.clear();
                        app.pending_cache = None;
                        app.selected_msg = Some(app.messages.len().saturating_sub(1));
                        app.chat_scroll =
                            offset_for_message(&app.messages, app.selected_msg.unwrap_or(0));
                        app.focus = Focus::Chat;
                    }
                }
                Focus::Chat => {
                    if let Some(i) = app.selected_msg {
                        if let Some(m) = app.messages.get(i) {
                            let tx2 = tx.clone();
                            let content = m.content.clone();
                            tokio::spawn(async move {
                                if let Err(e) = preview_to_html(content).await {
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
