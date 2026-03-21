
#[derive(Clone, Debug)]
struct ActiveStream {
    request_id: String,
    buffer: String,
    thinking: String,
    status: Option<String>,
}

struct App {
    // current chat
    current_chat_id: Option<String>,
    current_created_ts: Option<i64>,
    messages: Vec<Message>,
    pending_assistant: String,
    pending_thinking: String,
    status_message: Option<String>,
    show_thinking: bool,
    pending_request_id: Option<String>,
    active_streams: HashMap<String, ActiveStream>,

    // sidebar
    chats: Vec<ChatMeta>,
    sidebar_idx: usize,
    show_sidebar: bool,

    // UI/input
    input: String,
    input_cursor_line: usize, // line index in input
    input_cursor_col: usize,  // grapheme index in current line
    input_top_line: usize,    // first visible line in input viewport
    chat_scroll: u16,
    chat_inner_height: u16,
    chat_inner_width: u16,
    sending: bool,
    model: String,
    api_url: String,
    options: Option<JsonValue>,
    ollama_api_key: Option<String>,
    web_search: bool,
    system_prompt: Option<String>,
    bold_selection: bool,
    theme: Theme,
    quit: bool,
    mode: Mode,
    focus: Focus,
    selected_msg: Option<usize>,
    popup: Popup,

    // preview cfg
    preview_fmt: String,    // "html" | "pdf"
    preview_open: String,   // program to open

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
        ollama_api_key: Option<String>,
        web_search: bool,
        system_prompt: Option<String>,
        bold_selection: bool,
        theme: Theme,
        syntax_enabled: bool,
        syntax_theme_name: String,
        syntax_custom: Option<toml::value::Table>,
        stream_throttle: Duration,
        preview_fmt: String,
        preview_open: String,
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
            pending_thinking: String::new(),
            status_message: None,
            show_thinking: true,
            pending_request_id: None,
            active_streams: HashMap::new(),
            chats,
            sidebar_idx: 0,
            show_sidebar: true,
            input: String::new(),
            input_cursor_line: 0,
            input_cursor_col: 0,
            input_top_line: 0,
            chat_scroll: 0,
            chat_inner_height: 0,
            chat_inner_width: 0,
            sending: false,
            model,
            api_url,
            options,
            ollama_api_key,
            web_search,
            system_prompt,
            bold_selection,
            theme,
            quit: false,
            mode: Mode::Normal,
            focus: Focus::Chat,
            selected_msg: None,
            popup: Popup::None,
            preview_fmt,
            preview_open,
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
