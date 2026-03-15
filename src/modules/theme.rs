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

