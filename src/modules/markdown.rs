use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

fn syn_to_color(c: SynColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

#[derive(Clone, Copy)]
struct ListContext {
    next_number: Option<u64>,
}

#[derive(Default)]
struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    current_row: Vec<Vec<Span<'static>>>,
    current_cell: Vec<Span<'static>>,
    in_cell: bool,
}

struct MarkdownRenderer<'a> {
    text: Text<'static>,
    current: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    list_stack: Vec<ListContext>,
    pending_list_marker: Option<Vec<Span<'static>>>,
    quote_depth: usize,
    link_url: Option<String>,
    code_block: Option<(Option<String>, String)>,
    table: Option<TableState>,
    theme: &'a Theme,
    inner_width: u16,
    ss: &'a SyntaxSet,
    syn_theme: &'a SynTheme,
    syntax_enabled: bool,
    render_emojis: bool,
}

impl<'a> MarkdownRenderer<'a> {
    fn new(
        theme: &'a Theme,
        inner_width: u16,
        ss: &'a SyntaxSet,
        syn_theme: &'a SynTheme,
        syntax_enabled: bool,
        render_emojis: bool,
    ) -> Self {
        Self {
            text: Text::default(),
            current: Vec::new(),
            style_stack: vec![Style::default()],
            list_stack: Vec::new(),
            pending_list_marker: None,
            quote_depth: 0,
            link_url: None,
            code_block: None,
            table: None,
            theme,
            inner_width,
            ss,
            syn_theme,
            syntax_enabled,
            render_emojis,
        }
    }

    fn finish(mut self) -> Text<'static> {
        self.flush_line();
        self.text
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&Style::default())
    }

    fn push_style(&mut self, patch: Style) {
        self.style_stack.push(self.current_style().patch(patch));
    }

    fn pop_style(&mut self) {
        if self.style_stack.len() > 1 {
            self.style_stack.pop();
        }
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }

        for _ in 0..self.quote_depth {
            self.current.push(Span::styled(
                "▏ ",
                Style::default().fg(self.theme.blockquote_bar),
            ));
        }

        if let Some(marker) = self.pending_list_marker.take() {
            self.current.extend(marker);
        }
    }

    fn push_span(&mut self, content: impl Into<String>, style: Style) {
        let content = filter_emojis(content.into(), self.render_emojis);
        if content.is_empty() {
            return;
        }

        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                table.current_cell.push(Span::styled(content, style));
                return;
            }
        }

        self.ensure_prefix();
        self.current.push(Span::styled(content, style));
    }

    fn push_text(&mut self, content: &str) {
        if content.is_empty() {
            return;
        }
        self.push_span(content.to_string(), self.current_style());
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.text.push_line(Line::from(std::mem::take(&mut self.current)));
    }

    fn start_list_item(&mut self) {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = if let Some(ctx) = self.list_stack.last_mut() {
            if let Some(n) = ctx.next_number.as_mut() {
                let marker = format!("{}{}. ", indent, *n);
                *n += 1;
                Span::styled(marker, Style::default().fg(self.theme.ordered_number))
            } else {
                Span::styled(format!("{}• ", indent), Style::default().fg(self.theme.list_bullet))
            }
        } else {
            Span::styled("• ", Style::default().fg(self.theme.list_bullet))
        };
        self.pending_list_marker = Some(vec![marker]);
    }

    fn end_list_item(&mut self) {
        self.flush_line();
        self.pending_list_marker = None;
    }

    fn push_task_marker(&mut self, checked: bool) {
        let marker = if checked { "☑ " } else { "☐ " };
        if let Some(prefix) = self.pending_list_marker.as_mut() {
            prefix.push(Span::styled(marker, Style::default().fg(self.theme.list_bullet)));
        } else {
            self.push_span(marker, Style::default().fg(self.theme.list_bullet));
        }
    }

    fn push_code_block_line(&mut self, raw: &str, high: Option<&mut HighlightLines>) {
        let style_bg = Style::default().bg(self.theme.code_block_bg);
        let w = self.inner_width.max(1) as usize;
        const NBSP: char = '\u{00A0}';

        if raw.is_empty() {
            let fill: String = std::iter::repeat(NBSP).take(w).collect();
            self.text
                .push_line(Line::styled(fill, style_bg.fg(self.theme.code_block_fg)));
            return;
        }

        if let Some(h) = high {
            let ranges = h.highlight_line(raw, self.ss).unwrap_or_default();
            let mut cur_line: Vec<Span<'static>> = Vec::new();
            let mut cur_w = 0usize;

            for (sty, seg) in ranges {
                if seg.is_empty() {
                    continue;
                }
                let st = style_bg.fg(syn_to_color(sty.foreground));
                for g in UnicodeSegmentation::graphemes(seg, true) {
                    let gw = UnicodeWidthStr::width(g);

                    if gw > w {
                        if !cur_line.is_empty() || cur_w > 0 {
                            let pad_len = w.saturating_sub(cur_w);
                            if pad_len > 0 {
                                let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                                cur_line.push(Span::styled(pad, style_bg.fg(self.theme.code_block_fg)));
                            }
                            self.text.push_line(Line::from(std::mem::take(&mut cur_line)));
                            cur_w = 0;
                        }
                        let mut spans = vec![Span::styled(g.to_string(), st)];
                        let pad_len = w.saturating_sub(gw.min(w));
                        if pad_len > 0 {
                            let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                            spans.push(Span::styled(pad, style_bg.fg(self.theme.code_block_fg)));
                        }
                        self.text.push_line(Line::from(spans));
                        continue;
                    }

                    if cur_w + gw > w {
                        let pad_len = w.saturating_sub(cur_w);
                        if pad_len > 0 {
                            let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                            cur_line.push(Span::styled(pad, style_bg.fg(self.theme.code_block_fg)));
                        }
                        self.text.push_line(Line::from(std::mem::take(&mut cur_line)));
                        cur_w = 0;
                    }

                    cur_line.push(Span::styled(g.to_string(), st));
                    cur_w += gw;
                }
            }

            let pad_len = w.saturating_sub(cur_w);
            if pad_len > 0 {
                let pad: String = std::iter::repeat(NBSP).take(pad_len).collect();
                cur_line.push(Span::styled(pad, style_bg.fg(self.theme.code_block_fg)));
            }
            self.text.push_line(Line::from(cur_line));
        } else {
            let mut line = raw.to_string();
            let cur_w = UnicodeWidthStr::width(line.as_str());
            if cur_w < w {
                let pad: String = std::iter::repeat(NBSP).take(w - cur_w).collect();
                line.push_str(&pad);
            }
            self.text
                .push_line(Line::styled(line, style_bg.fg(self.theme.code_block_fg)));
        }
    }

    fn render_code_block(&mut self, language: Option<String>, contents: String) {
        self.flush_line();
        let mut high = if self.syntax_enabled {
            let syn = language
                .as_deref()
                .filter(|lang| !lang.is_empty())
                .and_then(|lang| self.ss.find_syntax_by_token(lang))
                .unwrap_or_else(|| self.ss.find_syntax_plain_text());
            Some(HighlightLines::new(syn, self.syn_theme))
        } else {
            None
        };

        for raw in contents.trim_end_matches('\n').split('\n') {
            let raw = filter_emojis(raw.to_string(), self.render_emojis);
            self.push_code_block_line(&raw, high.as_mut());
        }
    }

    fn render_table(&mut self, table: TableState) {
        self.flush_line();
        if table.rows.is_empty() {
            return;
        }

        let cols = table.rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if cols == 0 {
            return;
        }

        let mut widths = vec![1usize; cols];
        for row in &table.rows {
            for (idx, cell) in row.iter().enumerate() {
                let width = cell.iter().map(|span| UnicodeWidthStr::width(span.content.as_ref())).sum::<usize>();
                widths[idx] = widths[idx].max(width);
            }
        }

        let render_row = |text: &mut Text<'static>, row: &[Vec<Span<'static>>]| {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(self.theme.hr))];
            for col in 0..cols {
                let mut cell = row.get(col).cloned().unwrap_or_default();
                let width = cell.iter().map(|span| UnicodeWidthStr::width(span.content.as_ref())).sum::<usize>();
                let pad = widths[col].saturating_sub(width);
                match table.alignments.get(col).copied().unwrap_or(Alignment::None) {
                    Alignment::Right => {
                        if pad > 0 {
                            cell.insert(0, Span::raw(" ".repeat(pad)));
                        }
                    }
                    Alignment::Center => {
                        let left = pad / 2;
                        let right = pad - left;
                        if left > 0 {
                            cell.insert(0, Span::raw(" ".repeat(left)));
                        }
                        if right > 0 {
                            cell.push(Span::raw(" ".repeat(right)));
                        }
                    }
                    _ => {
                        if pad > 0 {
                            cell.push(Span::raw(" ".repeat(pad)));
                        }
                    }
                }
                spans.extend(cell);
                spans.push(Span::styled(" │ ", Style::default().fg(self.theme.hr)));
            }
            text.push_line(Line::from(spans));
        };

        let separator = {
            let mut s = String::from("├");
            for width in &widths {
                s.push_str(&"─".repeat(*width + 2));
                s.push('┼');
            }
            s.pop();
            s.push('┤');
            s
        };

        for (idx, row) in table.rows.iter().enumerate() {
            render_row(&mut self.text, row);
            if idx == 0 && table.rows.len() > 1 {
                self.text.push_line(Line::styled(separator.clone(), Style::default().fg(self.theme.hr)));
            }
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                let style = match level {
                    HeadingLevel::H1 => Style::default().fg(self.theme.heading1).add_modifier(Modifier::BOLD),
                    HeadingLevel::H2 => Style::default().fg(self.theme.heading2).add_modifier(Modifier::BOLD),
                    HeadingLevel::H3 => Style::default().fg(self.theme.heading3).add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(self.theme.heading4).add_modifier(Modifier::BOLD),
                };
                self.push_style(style);
            }
            Tag::BlockQuote => self.quote_depth += 1,
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_string),
                    CodeBlockKind::Indented => None,
                };
                self.code_block = Some((language, String::new()));
            }
            Tag::List(first) => {
                if !self.list_stack.is_empty() {
                    self.flush_line();
                }
                self.list_stack.push(ListContext { next_number: first });
            }
            Tag::Item => self.start_list_item(),
            Tag::FootnoteDefinition(label) => {
                self.push_span(format!("[{}]: ", label), Style::default().fg(self.theme.ordered_number));
            }
            Tag::Table(alignments) => self.table = Some(TableState { alignments, ..TableState::default() }),
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.current_cell.clear();
                    table.in_cell = true;
                }
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.push_style(Style::default().fg(self.theme.heading3).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, title, .. } => {
                let label = if title.is_empty() { "image".to_string() } else { title.to_string() };
                self.push_span(format!("[{}: {}]", label, dest_url), Style::default().fg(self.theme.status_hint));
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_line(),
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush_line();
            }
            TagEnd::BlockQuote => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((language, contents)) = self.code_block.take() {
                    self.render_code_block(language, contents);
                }
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.flush_line();
            }
            TagEnd::Item => self.end_list_item(),
            TagEnd::FootnoteDefinition => {
                self.flush_line();
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.render_table(table);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(std::mem::take(&mut table.current_row));
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.in_cell = false;
                    table.current_row.push(std::mem::take(&mut table.current_cell));
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(url) = self.link_url.take() {
                    self.push_span(format!(" ({})", url), Style::default().fg(self.theme.status_hint));
                }
            }
            TagEnd::Image => {}
            _ => {}
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        if let Some((_, contents)) = self.code_block.as_mut() {
            match event {
                Event::End(TagEnd::CodeBlock) => self.handle_end(TagEnd::CodeBlock),
                Event::Text(text) => contents.push_str(&text),
                Event::SoftBreak | Event::HardBreak => contents.push('\n'),
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => self.push_span(
                code.to_string(),
                self.current_style()
                    .fg(self.theme.inline_code)
                    .bg(self.theme.inline_code_bg),
            ),
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(label) => self.push_span(
                format!("[{}]", label),
                self.current_style().fg(self.theme.ordered_number),
            ),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                let hr = "─".repeat(self.inner_width.max(1) as usize);
                self.text.push_line(Line::styled(hr, Style::default().fg(self.theme.hr)));
            }
            Event::TaskListMarker(checked) => self.push_task_marker(checked),
        }
    }
}

fn filter_emojis(input: String, render_emojis: bool) -> String {
    if render_emojis {
        return input;
    }

    UnicodeSegmentation::graphemes(input.as_str(), true)
        .filter(|g| !is_emoji_grapheme(g))
        .collect()
}

fn is_emoji_grapheme(g: &str) -> bool {
    g.chars().any(|c| {
        matches!(
            c as u32,
            0x1F000..=0x1FAFF | 0xE0020..=0xE007F
        ) || c == '\u{FE0F}'
    })
}

fn render_markdown_to_text(
    input: &str,
    theme: &Theme,
    inner_width: u16,
    ss: &SyntaxSet,
    syn_theme: &SynTheme,
    syntax_enabled: bool,
    render_emojis: bool,
) -> Text<'static> {
    let input = input.replace("‑", "-").replace('\t', "    ");
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let parser = Parser::new_ext(&input, options);
    let mut renderer = MarkdownRenderer::new(theme, inner_width, ss, syn_theme, syntax_enabled, render_emojis);
    for event in parser {
        renderer.handle_event(event);
    }
    renderer.finish()
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    fn render_plain(input: &str) -> Vec<String> {
        render_plain_with_emoji_config(input, true)
    }

    fn render_plain_with_emoji_config(input: &str, render_emojis: bool) -> Vec<String> {
        let ss = SyntaxSet::load_defaults_newlines();
        let syn_theme = SynTheme::default();
        render_markdown_to_text(
            input,
            &Theme::default(),
            80,
            &ss,
            &syn_theme,
            false,
            render_emojis,
        )
        .lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect()
    }

    #[test]
    fn renders_commonmark_links_tasks_footnotes_and_tables() {
        let lines = render_plain(
            "- [x] Ship [docs](https://example.com) with footnote[^1]\n\n[^1]: Citation text\n\n| Name | Done |\n| :--- | ---: |\n| API | yes |",
        );

        assert!(lines.iter().any(|line| line
            == "• ☑ Ship docs (https://example.com) with footnote[1]"));
        assert!(lines.iter().any(|line| line == "[1]: Citation text"));
        assert!(lines.iter().any(|line| line.contains("│ Name │ Done │")));
        assert!(lines.iter().any(|line| line.contains("│ API  │  yes │")));
    }

    #[test]
    fn parser_handles_nested_lists_and_escaped_delimiters() {
        let lines = render_plain("1. Parent with \\*literal stars\\*\n   - Child with **bold** text");

        assert_eq!(lines[0], "1. Parent with *literal stars*");
        assert_eq!(lines[1], "  • Child with bold text");
    }

    #[test]
    fn can_suppress_emoji_graphemes() {
        let lines = render_plain_with_emoji_config("Status 😀 is good", false);

        assert_eq!(lines[0], "Status  is good");
    }
}
