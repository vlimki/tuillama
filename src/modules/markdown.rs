use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

fn syn_to_color(c: SynColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

#[derive(Clone, Copy, Default)]
struct InlineAttrs {
    emphasis: bool,
    strong: bool,
    strike: bool,
    link: bool,
}

#[derive(Clone)]
enum Inline {
    Text { content: String, attrs: InlineAttrs },
    Code(String),
    Math { content: String, block: bool },
    Footnote(String),
    Image { label: String, url: String },
    LinkUrl(String),
}

#[derive(Clone)]
enum MdBlock {
    Paragraph(Vec<Inline>),
    Heading { level: u8, content: Vec<Inline> },
    CodeBlock { lang: Option<String>, code: String },
    List { ordered: bool, items: Vec<Vec<MdBlock>>, open: bool },
    Table { alignments: Vec<Alignment>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
    BlockQuote(Vec<MdBlock>),
    FootnoteDefinition { label: String, content: Vec<MdBlock> },
}

#[derive(Clone, Copy)]
struct ListContext {
    next_number: Option<u64>,
}

#[derive(Default)]
struct TableBuildState {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Inline>>>,
    current_row: Vec<Vec<Inline>>,
    current_cell: Vec<Inline>,
    in_cell: bool,
}

struct MarkdownDocumentBuilder {
    blocks: Vec<MdBlock>,
    inline_buf: Vec<Inline>,
    attrs_stack: Vec<InlineAttrs>,
    current_heading: Option<u8>,
    code_block: Option<(Option<String>, String)>,
    link_url: Option<String>,
    footnote_label: Option<String>,
    footnote_blocks: Vec<MdBlock>,
    quote_depth: usize,
    table: Option<TableBuildState>,
    math_placeholders: Vec<(bool, String)>,
}

impl MarkdownDocumentBuilder {
    fn new(math_placeholders: Vec<(bool, String)>) -> Self {
        Self {
            blocks: Vec::new(),
            inline_buf: Vec::new(),
            attrs_stack: vec![InlineAttrs::default()],
            current_heading: None,
            code_block: None,
            link_url: None,
            footnote_label: None,
            footnote_blocks: Vec::new(),
            quote_depth: 0,
            table: None,
            math_placeholders,
        }
    }

    fn current_attrs(&self) -> InlineAttrs {
        *self.attrs_stack.last().unwrap_or(&InlineAttrs::default())
    }

    fn push_attrs(&mut self, patch: impl FnOnce(&mut InlineAttrs)) {
        let mut attrs = self.current_attrs();
        patch(&mut attrs);
        self.attrs_stack.push(attrs);
    }

    fn pop_attrs(&mut self) {
        if self.attrs_stack.len() > 1 {
            self.attrs_stack.pop();
        }
    }

    fn emit_block(&mut self, block: MdBlock) {
        if self.footnote_label.is_some() {
            self.footnote_blocks.push(block);
        } else if self.quote_depth > 0 {
            self.blocks.push(MdBlock::BlockQuote(vec![block]));
        } else {
            self.blocks.push(block);
        }
    }

    fn push_inline(&mut self, inline: Inline) {
        if let Some(table) = self.table.as_mut() {
            if table.in_cell {
                table.current_cell.push(inline);
                return;
            }
        }
        self.inline_buf.push(inline);
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let attrs = self.current_attrs();
        for part in split_math_placeholders(text, &self.math_placeholders) {
            match part {
                MathSplit::Text(s) if !s.is_empty() => self.push_inline(Inline::Text { content: s, attrs }),
                MathSplit::Math { block, content } => self.push_inline(Inline::Math { content, block }),
                MathSplit::Text(_) => {}
            }
        }
    }

    fn finish_inlines_as(&mut self, paragraph: bool) {
        if self.inline_buf.is_empty() {
            return;
        }
        if let Some(level) = self.current_heading.take() {
            let content = std::mem::take(&mut self.inline_buf);
            self.emit_block(MdBlock::Heading { level, content });
        } else if paragraph {
            let content = std::mem::take(&mut self.inline_buf);
            self.emit_block(MdBlock::Paragraph(content));
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.current_heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
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
                self.finish_inlines_as(true);
                self.emit_block(MdBlock::List { ordered: first.is_some(), items: Vec::new(), open: true });
            }
            Tag::Item => {}
            Tag::FootnoteDefinition(label) => {
                self.footnote_label = Some(label.to_string());
                self.footnote_blocks.clear();
            }
            Tag::Table(alignments) => self.table = Some(TableBuildState { alignments, ..TableBuildState::default() }),
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
            Tag::Emphasis => self.push_attrs(|attrs| attrs.emphasis = true),
            Tag::Strong => self.push_attrs(|attrs| attrs.strong = true),
            Tag::Strikethrough => self.push_attrs(|attrs| attrs.strike = true),
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.push_attrs(|attrs| attrs.link = true);
            }
            Tag::Image { dest_url, title, .. } => {
                let label = if title.is_empty() { "image".to_string() } else { title.to_string() };
                self.push_inline(Inline::Image { label, url: dest_url.to_string() });
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.finish_inlines_as(true),
            TagEnd::Heading(_) => self.finish_inlines_as(false),
            TagEnd::BlockQuote => self.quote_depth = self.quote_depth.saturating_sub(1),
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code_block.take() {
                    self.emit_block(MdBlock::CodeBlock { lang, code });
                }
            }
            TagEnd::List(_) => {
                for block in self.blocks.iter_mut().rev() {
                    if let MdBlock::List { open, .. } = block {
                        if *open {
                            *open = false;
                            break;
                        }
                    }
                }
            }
            TagEnd::Item => {
                self.finish_inlines_as(true);
                let mut item = Vec::new();
                while let Some(block) = self.blocks.pop() {
                    match block {
                        MdBlock::List { ordered, mut items, open } if open => {
                            items.push(item.into_iter().rev().collect());
                            self.blocks.push(MdBlock::List { ordered, items, open });
                            break;
                        }
                        other => item.push(other),
                    }
                }
            }
            TagEnd::FootnoteDefinition => {
                self.finish_inlines_as(true);
                if let Some(label) = self.footnote_label.take() {
                    let content = std::mem::take(&mut self.footnote_blocks);
                    self.blocks.push(MdBlock::FootnoteDefinition { label, content });
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.emit_block(MdBlock::Table { alignments: table.alignments, rows: table.rows });
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
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_attrs(),
            TagEnd::Link => {
                self.pop_attrs();
                if let Some(url) = self.link_url.take() {
                    self.push_inline(Inline::LinkUrl(url));
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
            Event::Code(code) => self.push_inline(Inline::Code(code.to_string())),
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(label) => self.push_inline(Inline::Footnote(label.to_string())),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.finish_inlines_as(true),
            Event::Rule => self.emit_block(MdBlock::Rule),
            Event::TaskListMarker(checked) => self.push_text(if checked { "☑ " } else { "☐ " }),
        }
    }

    fn finish(mut self) -> Vec<MdBlock> {
        self.finish_inlines_as(true);
        self.blocks
    }
}

enum MathSplit {
    Text(String),
    Math { block: bool, content: String },
}

const MATH_PLACEHOLDER_PREFIX: &str = "TUILLAMAMATHPLACEHOLDER";
const MATH_PLACEHOLDER_SUFFIX: &str = "END";

fn split_math_placeholders(text: &str, placeholders: &[(bool, String)]) -> Vec<MathSplit> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(MATH_PLACEHOLDER_PREFIX) {
        if start > 0 {
            parts.push(MathSplit::Text(rest[..start].to_string()));
        }
        let after_start = &rest[start + MATH_PLACEHOLDER_PREFIX.len()..];
        if let Some(end) = after_start.find(MATH_PLACEHOLDER_SUFFIX) {
            let idx_text = &after_start[..end];
            if let Ok(idx) = idx_text.parse::<usize>() {
                if let Some((block, content)) = placeholders.get(idx).cloned() {
                    parts.push(MathSplit::Math { block, content });
                }
            }
            rest = &after_start[end + MATH_PLACEHOLDER_SUFFIX.len()..];
        } else {
            parts.push(MathSplit::Text(rest[start..].to_string()));
            rest = "";
        }
    }
    if !rest.is_empty() {
        parts.push(MathSplit::Text(rest.to_string()));
    }
    parts
}

fn protect_math(input: &str) -> (String, Vec<(bool, String)>) {
    let mut out = String::new();
    let mut placeholders = Vec::new();
    let mut i = 0usize;
    while i < input.len() {
        let rest = &input[i..];
        let spec = if rest.starts_with("$$") {
            Some(("$$", true, 2usize))
        } else if rest.starts_with("\\[") {
            Some(("\\]", true, 2usize))
        } else if rest.starts_with("\\(") {
            Some(("\\)", false, 2usize))
        } else if let Some((open, close)) = latex_environment_delimiters(rest) {
            Some((close, true, open.len()))
        } else if rest.starts_with('$') && !rest.starts_with("$$") {
            Some(("$", false, 1usize))
        } else {
            None
        };

        if let Some((close, block, open_len)) = spec {
            if let Some(end_rel) = find_math_close(&input[i + open_len..], close) {
                let content_start = i + open_len;
                let content_end = content_start + end_rel;
                let idx = placeholders.len();
                placeholders.push((block, input[content_start..content_end].to_string()));
                out.push_str(&format!(
                    "{}{}{}",
                    MATH_PLACEHOLDER_PREFIX, idx, MATH_PLACEHOLDER_SUFFIX
                ));
                i = content_end + close.len();
                continue;
            }
        }

        if let Some(ch) = rest.chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    (out, placeholders)
}

fn latex_environment_delimiters(input: &str) -> Option<(&'static str, &'static str)> {
    const ENVS: &[&str] = &[
        "equation",
        "equation*",
        "align",
        "align*",
        "gather",
        "gather*",
        "multline",
        "multline*",
        "split",
    ];
    for env in ENVS {
        let open = match *env {
            "equation" => "\\begin{equation}",
            "equation*" => "\\begin{equation*}",
            "align" => "\\begin{align}",
            "align*" => "\\begin{align*}",
            "gather" => "\\begin{gather}",
            "gather*" => "\\begin{gather*}",
            "multline" => "\\begin{multline}",
            "multline*" => "\\begin{multline*}",
            "split" => "\\begin{split}",
            _ => unreachable!(),
        };
        if input.starts_with(open) {
            let close = match *env {
                "equation" => "\\end{equation}",
                "equation*" => "\\end{equation*}",
                "align" => "\\end{align}",
                "align*" => "\\end{align*}",
                "gather" => "\\end{gather}",
                "gather*" => "\\end{gather*}",
                "multline" => "\\end{multline}",
                "multline*" => "\\end{multline*}",
                "split" => "\\end{split}",
                _ => unreachable!(),
            };
            return Some((open, close));
        }
    }
    None
}

fn find_math_close(haystack: &str, close: &str) -> Option<usize> {
    let mut search_from = 0usize;
    while let Some(rel) = haystack[search_from..].find(close) {
        let pos = search_from + rel;
        let escaped = pos > 0 && haystack[..pos].chars().rev().take_while(|c| *c == '\\').count() % 2 == 1;
        if !escaped {
            return Some(pos);
        }
        search_from = pos + close.len();
    }
    None
}

struct MarkdownRenderer<'a> {
    text: Text<'static>,
    current: Vec<Span<'static>>,
    list_stack: Vec<ListContext>,
    pending_list_marker: Option<Vec<Span<'static>>>,
    quote_depth: usize,
    theme: &'a Theme,
    inner_width: u16,
    ss: &'a SyntaxSet,
    syn_theme: &'a SynTheme,
    syntax_enabled: bool,
    render_emojis: bool,
    selected_code_block: Option<usize>,
    code_block_index: usize,
}

impl<'a> MarkdownRenderer<'a> {
    fn new(
        theme: &'a Theme,
        inner_width: u16,
        ss: &'a SyntaxSet,
        syn_theme: &'a SynTheme,
        syntax_enabled: bool,
        render_emojis: bool,
        selected_code_block: Option<usize>,
    ) -> Self {
        Self {
            text: Text::default(),
            current: Vec::new(),
            list_stack: Vec::new(),
            pending_list_marker: None,
            quote_depth: 0,
            theme,
            inner_width,
            ss,
            syn_theme,
            syntax_enabled,
            render_emojis,
            selected_code_block,
            code_block_index: 0,
        }
    }

    fn finish(mut self) -> Text<'static> {
        self.flush_line();
        self.text
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        for _ in 0..self.quote_depth {
            self.current.push(Span::styled("▏ ", Style::default().fg(self.theme.blockquote_bar)));
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
        self.ensure_prefix();
        self.current.push(Span::styled(content, style));
    }

    fn flush_line(&mut self) {
        if !self.current.is_empty() {
            self.text.push_line(Line::from(std::mem::take(&mut self.current)));
        }
    }

    fn add_modifier_to_last_line(&mut self, modifier: Modifier) {
        if let Some(line) = self.text.lines.last_mut() {
            for span in &mut line.spans {
                span.style = span.style.add_modifier(modifier);
            }
        }
    }

    fn push_blank_line(&mut self) {
        self.flush_line();
        let already_blank = self.text.lines.last().is_some_and(|line| {
            line.spans
                .iter()
                .all(|span| span.content.as_ref().trim().is_empty())
        });
        if !self.text.lines.is_empty() && !already_blank {
            self.text.push_line(Line::default());
        }
    }

    fn inline_style(&self, attrs: InlineAttrs) -> Style {
        let mut style = Style::default();
        if attrs.link {
            style = style.fg(self.theme.heading3).add_modifier(Modifier::UNDERLINED);
        }
        if attrs.emphasis {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if attrs.strong {
            style = style.add_modifier(Modifier::BOLD);
        }
        if attrs.strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn render_inlines(&mut self, inlines: &[Inline], base_style: Style) {
        for inline in inlines {
            match inline {
                Inline::Text { content, attrs } => self.push_span(content.clone(), base_style.patch(self.inline_style(*attrs))),
                Inline::Code(code) => self.push_span(code.clone(), base_style.fg(self.theme.inline_code).bg(self.theme.inline_code_bg)),
                Inline::Math { content, block } => {
                    let rendered = if *block {
                        content.trim().to_string()
                    } else {
                        content.clone()
                    };
                    self.push_span(
                        rendered,
                        base_style.fg(self.theme.heading2).add_modifier(Modifier::ITALIC),
                    );
                }
                Inline::Footnote(label) => self.push_span(format!("[{}]", label), base_style.fg(self.theme.ordered_number)),
                Inline::Image { label, url } => self.push_span(format!("[{}: {}]", label, url), base_style.fg(self.theme.status_hint)),
                Inline::LinkUrl(url) => self.push_span(format!(" ({})", url), base_style.fg(self.theme.status_hint)),
            }
        }
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

    fn render_code_block(&mut self, language: Option<String>, contents: String) {
        self.flush_line();
        let selected = self.selected_code_block == Some(self.code_block_index);
        self.code_block_index += 1;
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
            if selected {
                self.add_modifier_to_last_line(Modifier::REVERSED);
            }
        }
    }

    fn push_code_block_line(&mut self, raw: &str, high: Option<&mut HighlightLines>) {
        let style_bg = Style::default().bg(self.theme.code_block_bg);
        let w = self.inner_width.max(1) as usize;
        const NBSP: char = '\u{00A0}';
        if raw.is_empty() {
            let fill: String = std::iter::repeat(NBSP).take(w).collect();
            self.text.push_line(Line::styled(fill, style_bg.fg(self.theme.code_block_fg)));
            return;
        }
        if let Some(h) = high {
            let ranges = h.highlight_line(raw, self.ss).unwrap_or_default();
            let mut cur_line: Vec<Span<'static>> = Vec::new();
            let mut cur_w = 0usize;
            for (sty, seg) in ranges {
                if seg.is_empty() { continue; }
                let st = style_bg.fg(syn_to_color(sty.foreground));
                for g in UnicodeSegmentation::graphemes(seg, true) {
                    let gw = UnicodeWidthStr::width(g);
                    if cur_w + gw > w && cur_w > 0 {
                        let pad_len = w.saturating_sub(cur_w);
                        if pad_len > 0 { cur_line.push(Span::styled(std::iter::repeat(NBSP).take(pad_len).collect::<String>(), style_bg.fg(self.theme.code_block_fg))); }
                        self.text.push_line(Line::from(std::mem::take(&mut cur_line)));
                        cur_w = 0;
                    }
                    cur_line.push(Span::styled(g.to_string(), st));
                    cur_w += gw;
                }
            }
            let pad_len = w.saturating_sub(cur_w);
            if pad_len > 0 { cur_line.push(Span::styled(std::iter::repeat(NBSP).take(pad_len).collect::<String>(), style_bg.fg(self.theme.code_block_fg))); }
            self.text.push_line(Line::from(cur_line));
        } else {
            let mut line = raw.to_string();
            let cur_w = UnicodeWidthStr::width(line.as_str());
            if cur_w < w { line.push_str(&std::iter::repeat(NBSP).take(w - cur_w).collect::<String>()); }
            self.text.push_line(Line::styled(line, style_bg.fg(self.theme.code_block_fg)));
        }
    }

    fn render_table(&mut self, alignments: &[Alignment], rows: &[Vec<Vec<Inline>>]) {
        self.flush_line();
        if rows.is_empty() { return; }
        let cols = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        if cols == 0 { return; }
        let natural = table_widths(rows, cols);
        let natural_total = table_total_width(&natural);
        let available = self.inner_width.max(1) as usize;
        if natural_total <= available {
            self.render_table_normal(alignments, rows, &natural);
        } else if cols <= 6 && available >= cols * 6 + 1 {
            let widths = wrapped_widths(&natural, available, cols);
            self.render_table_wrapped(alignments, rows, &widths);
        } else {
            self.render_table_fallback(rows);
        }
    }

    fn render_table_normal(&mut self, alignments: &[Alignment], rows: &[Vec<Vec<Inline>>], widths: &[usize]) {
        let cols = widths.len();
        let separator = table_separator(widths);
        for (idx, row) in rows.iter().enumerate() {
            self.text.push_line(Line::from(table_row_spans(self, alignments, row, widths, cols)));
            if idx == 0 && rows.len() > 1 {
                self.text.push_line(Line::styled(separator.clone(), Style::default().fg(self.theme.hr)));
            }
        }
    }

    fn render_table_wrapped(&mut self, alignments: &[Alignment], rows: &[Vec<Vec<Inline>>], widths: &[usize]) {
        let cols = widths.len();
        let separator = table_separator(widths);
        for (idx, row) in rows.iter().enumerate() {
            let wrapped: Vec<Vec<Vec<Span<'static>>>> = (0..cols)
                .map(|col| wrap_spans(inlines_to_spans(self, row.get(col).map(Vec::as_slice).unwrap_or(&[]), Style::default()), widths[col]))
                .collect();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            for line_idx in 0..height {
                let mut line = vec![Span::styled("│ ", Style::default().fg(self.theme.hr))];
                for col in 0..cols {
                    let mut cell = wrapped[col].get(line_idx).cloned().unwrap_or_default();
                    pad_cell(&mut cell, widths[col], alignments.get(col).copied().unwrap_or(Alignment::None));
                    line.extend(cell);
                    line.push(Span::styled(" │ ", Style::default().fg(self.theme.hr)));
                }
                self.text.push_line(Line::from(line));
            }
            if idx == 0 && rows.len() > 1 {
                self.text.push_line(Line::styled(separator.clone(), Style::default().fg(self.theme.hr)));
            }
        }
    }

    fn render_table_fallback(&mut self, rows: &[Vec<Vec<Inline>>]) {
        let headers = rows.first().cloned().unwrap_or_default();
        for (row_idx, row) in rows.iter().enumerate().skip(1) {
            self.text.push_line(Line::styled(format!("Row {}", row_idx), Style::default().fg(self.theme.hr).add_modifier(Modifier::BOLD)));
            for (col, cell) in row.iter().enumerate() {
                let key = headers.get(col).map(|cell| inlines_plain(cell)).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Column {}", col + 1));
                let value = inlines_plain(cell);
                self.text.push_line(Line::from(vec![
                    Span::styled(format!("  {}: ", key), Style::default().fg(self.theme.ordered_number)),
                    Span::raw(value),
                ]));
            }
        }
        if rows.len() <= 1 {
            for (col, cell) in headers.iter().enumerate() {
                self.text.push_line(Line::from(vec![Span::styled(format!("Column {}: ", col + 1), Style::default().fg(self.theme.ordered_number)), Span::raw(inlines_plain(cell))]));
            }
        }
    }

    fn render_block(&mut self, block: &MdBlock) {
        match block {
            MdBlock::Paragraph(inlines) => { self.render_inlines(inlines, Style::default()); self.flush_line(); }
            MdBlock::Heading { level, content } => {
                self.push_blank_line();
                let style = match level {
                    1 => Style::default().fg(self.theme.heading1).add_modifier(Modifier::BOLD),
                    2 => Style::default().fg(self.theme.heading2).add_modifier(Modifier::BOLD),
                    3 => Style::default().fg(self.theme.heading3).add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(self.theme.heading4).add_modifier(Modifier::BOLD),
                };
                self.render_inlines(content, style);
                self.flush_line();
                self.push_blank_line();
            }
            MdBlock::CodeBlock { lang, code } => self.render_code_block(lang.clone(), code.clone()),
            MdBlock::List { ordered, items, .. } => {
                self.list_stack.push(ListContext { next_number: ordered.then_some(1) });
                for item in items {
                    self.start_list_item();
                    for block in item { self.render_block(block); }
                    self.pending_list_marker = None;
                    self.flush_line();
                }
                self.list_stack.pop();
            }
            MdBlock::Table { alignments, rows } => self.render_table(alignments, rows),
            MdBlock::Rule => { self.flush_line(); self.text.push_line(Line::styled("─".repeat(self.inner_width.max(1) as usize), Style::default().fg(self.theme.hr))); }
            MdBlock::BlockQuote(blocks) => { self.quote_depth += 1; for block in blocks { self.render_block(block); } self.quote_depth = self.quote_depth.saturating_sub(1); }
            MdBlock::FootnoteDefinition { label, content } => {
                self.push_span(format!("[{}]: ", label), Style::default().fg(self.theme.ordered_number));
                for block in content { self.render_block(block); }
                self.flush_line();
            }
        }
    }

    fn render_document(mut self, blocks: &[MdBlock]) -> Text<'static> {
        for block in blocks { self.render_block(block); }
        self.finish()
    }
}

fn inlines_to_spans(renderer: &MarkdownRenderer<'_>, inlines: &[Inline], base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text { content, attrs } => spans.push(Span::styled(filter_emojis(content.clone(), renderer.render_emojis), base_style.patch(renderer.inline_style(*attrs)))),
            Inline::Code(code) => spans.push(Span::styled(filter_emojis(code.clone(), renderer.render_emojis), base_style.fg(renderer.theme.inline_code).bg(renderer.theme.inline_code_bg))),
            Inline::Math { content, block } => spans.push(Span::styled(
                if *block {
                    content.trim().to_string()
                } else {
                    content.clone()
                },
                base_style.fg(renderer.theme.heading2).add_modifier(Modifier::ITALIC),
            )),
            Inline::Footnote(label) => spans.push(Span::styled(format!("[{}]", label), base_style.fg(renderer.theme.ordered_number))),
            Inline::Image { label, url } => spans.push(Span::styled(format!("[{}: {}]", label, url), base_style.fg(renderer.theme.status_hint))),
            Inline::LinkUrl(url) => spans.push(Span::styled(format!(" ({})", url), base_style.fg(renderer.theme.status_hint))),
        }
    }
    spans
}

fn inlines_plain(inlines: &[Inline]) -> String {
    let mut s = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { content, .. } | Inline::Code(content) => s.push_str(content),
            Inline::Math { content, block } => {
                if *block {
                    s.push_str(content.trim());
                } else {
                    s.push_str(content);
                }
            }
            Inline::Footnote(label) => s.push_str(&format!("[{}]", label)),
            Inline::Image { label, url } => s.push_str(&format!("[{}: {}]", label, url)),
            Inline::LinkUrl(url) => s.push_str(&format!(" ({})", url)),
        }
    }
    s
}

fn table_widths(rows: &[Vec<Vec<Inline>>], cols: usize) -> Vec<usize> {
    let mut widths = vec![1usize; cols];
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(UnicodeWidthStr::width(inlines_plain(cell).as_str()));
        }
    }
    widths
}

fn table_total_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + (widths.len() * 3) + 2
}

fn wrapped_widths(natural: &[usize], available: usize, cols: usize) -> Vec<usize> {
    let borders = cols * 3 + 2;
    let content = available.saturating_sub(borders).max(cols);
    let mut widths = natural.to_vec();
    while widths.iter().sum::<usize>() > content {
        if let Some((idx, width)) = widths.iter().enumerate().max_by_key(|(_, width)| *width).map(|(idx, width)| (idx, *width)) {
            if width <= 4 { break; }
            widths[idx] -= 1;
        } else { break; }
    }
    widths.into_iter().map(|w| w.max(4)).collect()
}

fn table_separator(widths: &[usize]) -> String {
    let mut s = String::from("├");
    for width in widths {
        s.push_str(&"─".repeat(*width + 2));
        s.push('┼');
    }
    s.pop();
    s.push('┤');
    s
}

fn table_row_spans(renderer: &MarkdownRenderer<'_>, alignments: &[Alignment], row: &[Vec<Inline>], widths: &[usize], cols: usize) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("│ ", Style::default().fg(renderer.theme.hr))];
    for col in 0..cols {
        let mut cell = inlines_to_spans(renderer, row.get(col).map(Vec::as_slice).unwrap_or(&[]), Style::default());
        pad_cell(&mut cell, widths[col], alignments.get(col).copied().unwrap_or(Alignment::None));
        spans.extend(cell);
        spans.push(Span::styled(" │ ", Style::default().fg(renderer.theme.hr)));
    }
    spans
}

fn span_widths(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| UnicodeWidthStr::width(span.content.as_ref())).sum()
}

fn pad_cell(cell: &mut Vec<Span<'static>>, width: usize, alignment: Alignment) {
    let pad = width.saturating_sub(span_widths(cell));
    match alignment {
        Alignment::Right => if pad > 0 { cell.insert(0, Span::raw(" ".repeat(pad))); },
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            if left > 0 { cell.insert(0, Span::raw(" ".repeat(left))); }
            if right > 0 { cell.push(Span::raw(" ".repeat(right))); }
        }
        _ => if pad > 0 { cell.push(Span::raw(" ".repeat(pad))); },
    }
}

fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    for span in spans {
        let style = span.style;
        for g in UnicodeSegmentation::graphemes(span.content.as_ref(), true) {
            let gw = UnicodeWidthStr::width(g);
            if cur_w + gw > width && cur_w > 0 {
                lines.push(Vec::new());
                cur_w = 0;
            }
            lines.last_mut().unwrap().push(Span::styled(g.to_string(), style));
            cur_w += gw;
        }
    }
    lines
}

fn parse_markdown_blocks(input: &str, options: Options) -> Vec<MdBlock> {
    let (protected, math_placeholders) = protect_math(input);
    let parser = Parser::new_ext(&protected, options);
    let mut builder = MarkdownDocumentBuilder::new(math_placeholders);
    for event in parser {
        builder.handle_event(event);
    }
    builder.finish()
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
    render_markdown_to_text_with_selected_code_block(
        input,
        theme,
        inner_width,
        ss,
        syn_theme,
        syntax_enabled,
        render_emojis,
        None,
    )
}

fn render_markdown_to_text_with_selected_code_block(
    input: &str,
    theme: &Theme,
    inner_width: u16,
    ss: &SyntaxSet,
    syn_theme: &SynTheme,
    syntax_enabled: bool,
    render_emojis: bool,
    selected_code_block: Option<usize>,
) -> Text<'static> {
    let input = input.replace("‑", "-").replace('\t', "    ");
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let blocks = parse_markdown_blocks(&input, options);
    MarkdownRenderer::new(
        theme,
        inner_width,
        ss,
        syn_theme,
        syntax_enabled,
        render_emojis,
        selected_code_block,
    )
    .render_document(&blocks)
}

fn markdown_code_blocks(input: &str) -> Vec<String> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for event in Parser::new_ext(input, options) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => current = Some(String::new()),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = current.take() {
                    blocks.push(code);
                }
            }
            Event::Text(text) => {
                if let Some(code) = current.as_mut() {
                    code.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(code) = current.as_mut() {
                    code.push('\n');
                }
            }
            _ => {}
        }
    }
    blocks
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    fn render_plain(input: &str) -> Vec<String> {
        render_plain_with_width_and_emoji_config(input, 80, true)
    }

    fn render_plain_with_width(input: &str, width: u16) -> Vec<String> {
        render_plain_with_width_and_emoji_config(input, width, true)
    }

    fn render_plain_with_emoji_config(input: &str, render_emojis: bool) -> Vec<String> {
        render_plain_with_width_and_emoji_config(input, 80, render_emojis)
    }

    fn render_plain_with_width_and_emoji_config(input: &str, width: u16, render_emojis: bool) -> Vec<String> {
        let ss = SyntaxSet::load_defaults_newlines();
        let syn_theme = SynTheme::default();
        render_markdown_to_text(
            input,
            &Theme::default(),
            width,
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
    fn parser_preserves_multiple_list_items() {
        let lines = render_plain("1. First\n2. Second\n   - Child");

        assert_eq!(lines[0], "1. First");
        assert_eq!(lines[1], "2. Second");
        assert_eq!(lines[2], "  • Child");
    }

    #[test]
    fn can_suppress_emoji_graphemes() {
        let lines = render_plain_with_emoji_config("Status 😀 is good", false);

        assert_eq!(lines[0], "Status  is good");
    }

    #[test]
    fn wraps_wide_tables_and_falls_back_for_tiny_widths() {
        let markdown = "| Name | Description |\n| --- | --- |\n| API | This is a very long description for a narrow viewport |";
        let wrapped = render_plain_with_width(markdown, 32);
        assert!(wrapped.iter().any(|line| line.contains("│ API")));
        assert!(wrapped.iter().all(|line| UnicodeWidthStr::width(line.as_str()) <= 32));

        let fallback = render_plain_with_width(markdown, 12);
        assert!(fallback.iter().any(|line| line == "Row 1"));
        assert!(fallback.iter().any(|line| line.contains("Name: API")));
    }

    #[test]
    fn math_is_rendered_as_atomic_styled_inline_and_block_spans() {
        let lines = render_plain("Inline $a_b * c$ and \\(x^2\\)\n\n$$E = mc^2$$\n\nNot *emphasis*.");

        assert!(lines.iter().any(|line| line.contains("a_b * c")));
        assert!(lines.iter().any(|line| line.contains("x^2")));
        assert!(lines.iter().any(|line| line.contains("E = mc^2")));
        assert!(lines.iter().all(|line| !line.contains("MATH")));
        assert!(lines.iter().any(|line| line == "Not emphasis."));
    }

    #[test]
    fn latex_environments_render_as_latex_without_placeholders() {
        let lines = render_plain(
            "Before\n\n\\begin{equation}\na_b = c_d\n\\end{equation}\n\nAfter",
        );

        assert!(lines.iter().any(|line| line == "a_b = c_d"));
        assert!(lines.iter().all(|line| !line.contains("MATH")));
        assert!(lines.iter().all(|line| !line.contains("PLACEHOLDER")));
    }

    #[test]
    fn headings_have_breathing_room_without_leading_blank_space() {
        let lines = render_plain("# Title\n\nBody\n\n## Next\nMore");

        assert_eq!(lines, vec!["Title", "", "Body", "", "Next", "", "More"]);
    }

    #[test]
    fn extracts_fenced_code_blocks_for_visual_yank() {
        let blocks = markdown_code_blocks("Text\n\n```rust\nfn main() {}\n```\n\n```\nplain\n```");

        assert_eq!(blocks, vec!["fn main() {}\n", "plain\n"]);
    }
}
