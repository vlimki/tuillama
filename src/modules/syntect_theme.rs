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
        font_style: Some(FontStyle::empty())
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

