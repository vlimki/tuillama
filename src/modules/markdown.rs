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

    let push_buf = |spans: &mut Vec<Span>, buf: &mut String, st: InlineState| {
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

    // Fix a GPT-ism
    let input = &input.replace("‑", "-");

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

