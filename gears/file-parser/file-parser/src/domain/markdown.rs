use toolkit_macros::domain_model;

use crate::domain::ir::{ParsedBlock, ParsedDocument};

/// Markdown renderer that converts `ParsedDocument` to Markdown string
#[domain_model]
pub struct MarkdownRenderer;

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// How `render_styled_text` writes text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    Text,
    /// Link text, inside `[...]`.
    LinkText,
    /// Inside a code span: as is.
    Code,
}

/// Iterator over Markdown chunks from a `ParsedDocument`
/// This iterator owns the document to avoid lifetime issues with async streaming
#[domain_model]
pub struct MarkdownRenderIter {
    doc: ParsedDocument,
    header_emitted: bool,
    block_index: usize,
    lists: ListColumns,
}

/// Where the open list items put their content, by level. A nested item
/// starts exactly at its parent's content: further left it would not nest,
/// and four columns or more past it the line could not start an item and
/// would be read as text of the parent's paragraph, a code block in the
/// item as Markdown and HTML.
#[derive(Default)]
struct ListColumns(Vec<(u8, usize)>);

impl ListColumns {
    /// The indent for an item at `level` whose marker is `marker_width`
    /// wide; the item becomes the parent of deeper ones that follow.
    fn indent(&mut self, level: u8, marker_width: usize) -> usize {
        while self.0.last().is_some_and(|&(open, _)| open >= level) {
            self.0.pop();
        }
        let indent = self.0.last().map_or(0, |&(_, content)| content);
        self.0.push((level, indent + marker_width));
        indent
    }
}

impl Iterator for MarkdownRenderIter {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        // First emit header chunk if not yet emitted
        if !self.header_emitted {
            self.header_emitted = true;
            let header = Self::render_header(&self.doc);
            if !header.is_empty() {
                return Some(header);
            }
        }

        // Then emit one chunk per block
        if self.block_index < self.doc.blocks.len() {
            let block = &self.doc.blocks[self.block_index];
            self.block_index += 1;
            let mut chunk = String::new();
            MarkdownRenderer::render_block_in(block, &mut self.lists, &mut chunk);
            if MarkdownRenderer::ends_list(block, self.doc.blocks.get(self.block_index)) {
                chunk.push('\n');
            }
            Some(chunk)
        } else {
            None
        }
    }
}

impl MarkdownRenderIter {
    /// Render the header chunk (title + metadata)
    fn render_header(doc: &ParsedDocument) -> String {
        let mut header = String::new();

        // Render title if present
        if let Some(ref title) = doc.title {
            header.push_str("# ");
            push_text(&one_line(title), false, &mut header);
            escape_closing_hashes(&mut header, 2);
            header.push_str("\n\n");
        }

        // Render metadata section if we have useful info
        if doc.language.is_some()
            || doc.meta.original_filename.is_some()
            || doc.meta.content_type.is_some()
        {
            header.push_str("---\n");
            let fields = [
                ("language", doc.language.as_deref()),
                ("filename", doc.meta.original_filename.as_deref()),
                ("content-type", doc.meta.content_type.as_deref()),
            ];
            for (name, value) in fields {
                if let Some(value) = value {
                    header.push_str(name);
                    header.push_str(": ");
                    push_text(&one_line(value), false, &mut header);
                    header.push('\n');
                }
            }
            header.push_str("---\n\n");
        }

        header
    }
}

/// `text` with line breaks turned into spaces. Titles and file names come
/// from the upload; a line break in one would end the heading or the
/// metadata line and let the rest pose as Markdown or metadata. In link text
/// and image alt, a line break could end the paragraph or start a block
/// inside the brackets, and the destination and title would be read as
/// Markdown.
fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\r', '\n']) {
        text.replace(['\r', '\n'], " ").into()
    } else {
        text.into()
    }
}

/// Writes document text (text runs, link text, image alt, the title and
/// metadata) so that it cannot become a link, an image, raw HTML or code,
/// with backslash escapes, which read better raw than entities: `[` and
/// backticks always, `<` where it could start a tag, a comment or an
/// autolink (see `lt_could_open_html`), `]` in link text and image alt
/// (`in_brackets`), where it would close the brackets early, and `~` where it
/// could open a `~~~` fence (see `tilde_run_could_open_fence`). Outside
/// brackets a `]` has no `[` to close: every `[` from the text is escaped,
/// and the renderer's own are closed by their own `]`. A backslash is
/// doubled only where it would escape what follows (see
/// `backslash_escapes_next`), so `C:\Users` stays. `&` goes as `&amp;` when
/// it would start a character reference or ends the text, where the next run
/// could complete one (`AT&T` stays). Emphasis characters are left alone.
fn push_text(text: &str, in_brackets: bool, output: &mut String) {
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i + c.len_utf8()..];
        match c {
            '[' | '`' => {
                output.push('\\');
                output.push(c);
            }
            '<' if lt_could_open_html(rest) => output.push_str("\\<"),
            ']' if in_brackets => output.push_str("\\]"),
            // `[^` at the start of a line opens a footnote definition in
            // comrak, which ignores the escape in `\]:`.
            '^' if in_brackets && i == 0 => output.push_str("\\^"),
            '\\' if backslash_escapes_next(rest) => output.push_str("\\\\"),
            '~' => {
                // The whole run at once: scanning it again for each of its
                // tildes would be quadratic.
                let run = 1 + rest.bytes().take_while(|&b| b == b'~').count();
                let tilde = if tilde_run_could_open_fence(text, i, run) {
                    "\\~"
                } else {
                    "~"
                };
                for _ in 0..run {
                    output.push_str(tilde);
                }
                i += run;
                continue;
            }
            '&' if rest.is_empty() || starts_reference(rest) => output.push_str("&amp;"),
            c => output.push(c),
        }
        i += c.len_utf8();
    }
}

/// Whether a `<` written before `rest` could start raw HTML (a tag, a
/// comment, a processing instruction or a declaration) or an autolink, URI
/// or email. Those need an ASCII letter, `/`, `!`, `?` or an email character
/// next. At the end of the text the next run or the renderer's markup
/// follows, so it counts too.
fn lt_could_open_html(rest: &str) -> bool {
    rest.chars().next().is_none_or(|c| {
        c.is_ascii_alphanumeric() || (c.is_ascii_punctuation() && !"<>()[]\\,;:@\"".contains(c))
    })
}

/// Whether a backslash written before `rest` would not show as itself: it
/// escapes ASCII punctuation (another backslash included), and before a line
/// ending it is a hard line break. At the end of the text whatever the
/// renderer writes next follows it (`**`, `](`, a code span, `~~`), so it is
/// doubled there too.
fn backslash_escapes_next(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|c| c.is_ascii_punctuation() || matches!(c, '\n' | '\r'))
}

/// Whether the run of `len` tildes at byte `start` of `text` must be escaped
/// so that no line can start with three tildes: a run of three or more,
/// wherever it is (a line can start after a list or quote marker, which
/// text leaves alone), and a run at either end of the text, which the
/// renderer's own `~~` strikethrough markers or the next run could extend.
/// The whole run, not just its first `~`: pulldown-cmark reads `~~\~~x~~` as
/// no strikethrough at all.
fn tilde_run_could_open_fence(text: &str, start: usize, len: usize) -> bool {
    start == 0 || start + len == text.len() || len >= 3
}

/// Drops the spaces and tabs at the start of each line of the inline text
/// written from `start` on. Renderers do not show them, and four columns of
/// them would turn the line into an indented code block (after a blank line
/// or at the start of the block). Code spans have no line breaks, so this
/// never reaches into code.
fn drop_line_indents(output: &mut String, start: usize) {
    let text = &output[start..];
    let mut line_start = true;
    if !text
        .split(['\n', '\r'])
        .any(|line| line.starts_with([' ', '\t']))
    {
        return;
    }
    let mut kept = String::with_capacity(text.len());
    for c in text.chars() {
        if line_start && matches!(c, ' ' | '\t') {
            continue;
        }
        line_start = matches!(c, '\n' | '\r');
        kept.push(c);
    }
    output.truncate(start);
    output.push_str(&kept);
}

/// Escapes the first `#` of a run of them that ends the first line of a
/// heading written from `start` on, after a space or alone: Markdown takes
/// it for the optional closing sequence and drops it (`# Issue #`).
fn escape_closing_hashes(output: &mut String, start: usize) {
    let line_end = output[start..]
        .find(['\n', '\r'])
        .map_or(output.len(), |i| start + i);
    let line = output[start..line_end].trim_end_matches([' ', '\t']);
    let content = line.trim_end_matches('#');
    if content.len() < line.len() && (content.is_empty() || content.ends_with([' ', '\t'])) {
        output.insert(start + content.len(), '\\');
    }
}

/// The lines of `text`, split at every Markdown line ending: `\n`, `\r\n`
/// and a lone `\r`, which `str::lines` leaves inside a line. Like
/// `str::lines`, a line ending at the very end gives no empty last line.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .unwrap_or(text);
    (!text.is_empty())
        .then_some(body)
        .into_iter()
        .flat_map(|body| body.split('\n'))
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
}

/// Length of the longest run of backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// Writes `code` as a code span. The delimiter is one backtick longer than
/// any run in the code, so the code cannot close the span. Line breaks go
/// as spaces, which is how a code span shows them anyway, so no line of
/// the span can start a fence. A space is added inside the delimiters when
/// the code starts or ends with a backtick, or starts and ends with a
/// space; Markdown strips that one space again.
fn push_code_span(code: &str, output: &mut String) {
    if code.is_empty() {
        return;
    }
    let code = code.replace("\r\n", " ").replace(['\r', '\n'], " ");
    let fence = "`".repeat(longest_backtick_run(&code) + 1);
    let pad = code.starts_with('`')
        || code.ends_with('`')
        || (code.starts_with(' ') && code.ends_with(' ') && !code.trim_matches(' ').is_empty());
    output.push_str(&fence);
    if pad {
        output.push(' ');
    }
    output.push_str(&code);
    if pad {
        output.push(' ');
    }
    output.push_str(&fence);
}

/// Writes the pending `code` as a code span and clears it.
fn flush_code(code: &mut String, output: &mut String) {
    push_code_span(code, output);
    code.clear();
}

/// Writes `text` after the pending `code`, if there is any text.
fn push_after_code(text: &str, code: &mut String, output: &mut String) {
    if !text.is_empty() {
        flush_code(code, output);
        output.push_str(text);
    }
}

/// The code block language as a fence info string, or `None` when it has
/// characters outside `[A-Za-z0-9_+.#-]`. It comes from the document (an
/// HTML `class`), and a backtick in it would stop the line from opening
/// the fence.
fn fence_language(language: &str) -> Option<&str> {
    let valid = !language.is_empty()
        && language
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '.' | '#' | '-'));
    valid.then_some(language)
}

/// Whether `rest`, the text after a `&`, would make a character reference:
/// `name;`, `#digits;` or `#xhex;`.
fn starts_reference(rest: &str) -> bool {
    fn ends_with_semicolon(body: &str, allowed: fn(&u8) -> bool) -> bool {
        let len = body.bytes().take_while(allowed).count();
        len > 0 && body.as_bytes().get(len) == Some(&b';')
    }
    match rest.strip_prefix('#') {
        Some(num) => match num.strip_prefix(['x', 'X']) {
            Some(hex) => ends_with_semicolon(hex, u8::is_ascii_hexdigit),
            None => ends_with_semicolon(num, u8::is_ascii_digit),
        },
        None => ends_with_semicolon(rest, u8::is_ascii_alphanumeric),
    }
}

impl MarkdownRenderer {
    /// Create a new markdown renderer
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Render a document using this renderer instance
    #[must_use]
    pub fn render_doc(doc: &ParsedDocument) -> String {
        Self::render(doc)
    }

    /// Create a streaming iterator over Markdown chunks
    /// Takes ownership of the document to avoid lifetime issues with async streaming
    #[must_use]
    pub fn render_iter(doc: ParsedDocument) -> MarkdownRenderIter {
        MarkdownRenderIter {
            doc,
            header_emitted: false,
            block_index: 0,
            lists: ListColumns::default(),
        }
    }

    /// Create a streaming iterator over Markdown chunks from a borrowed document
    /// This is a convenience method for when you don't need to move the document
    #[must_use]
    pub fn render_iter_ref(doc: &ParsedDocument) -> MarkdownRenderIter {
        MarkdownRenderIter {
            doc: doc.clone(),
            header_emitted: false,
            block_index: 0,
            lists: ListColumns::default(),
        }
    }

    /// Render a parsed document to Markdown (static method)
    /// Collects all chunks from the streaming iterator
    #[must_use]
    pub fn render(doc: &ParsedDocument) -> String {
        let mut output = String::new();
        for chunk in Self::render_iter_ref(doc) {
            output.push_str(&chunk);
        }
        output
    }

    /// List items end with a single line break; the last one of a list needs
    /// a blank line, or the next block would continue the item.
    fn ends_list(block: &ParsedBlock, next: Option<&ParsedBlock>) -> bool {
        matches!(block, ParsedBlock::ListItem { .. })
            && next.is_some_and(|b| !matches!(b, ParsedBlock::ListItem { .. }))
    }

    fn render_block(block: &ParsedBlock, output: &mut String) {
        Self::render_block_in(block, &mut ListColumns::default(), output);
    }

    /// Renders `block` as one of a sequence of blocks; `lists` follows the
    /// list items of the sequence.
    fn render_block_in(block: &ParsedBlock, lists: &mut ListColumns, output: &mut String) {
        if !matches!(block, ParsedBlock::ListItem { .. }) {
            // The blank line after the block ends any list.
            lists.0.clear();
        }
        match block {
            ParsedBlock::Heading { level, inlines } => {
                let level = (*level).clamp(1, 6);
                output.push_str(&"#".repeat(level as usize));
                output.push(' ');
                let start = output.len();
                Self::render_inlines(inlines, output);
                escape_closing_hashes(output, start);
                output.push_str("\n\n");
            }
            ParsedBlock::Paragraph { inlines } => {
                Self::render_inlines(inlines, output);
                output.push_str("\n\n");
            }
            ParsedBlock::ListItem {
                level,
                ordered,
                blocks,
            } => Self::render_list_item(*level, *ordered, blocks, lists, output),
            ParsedBlock::CodeBlock { language, code } => {
                // Longer than any backtick run in the code, so no line of
                // the code can close the fence.
                let fence = "`".repeat(longest_backtick_run(code).max(2) + 1);
                output.push_str(&fence);
                if let Some(lang) = language.as_deref().and_then(fence_language) {
                    output.push_str(lang);
                }
                output.push('\n');
                output.push_str(code);
                if !code.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&fence);
                output.push_str("\n\n");
            }
            ParsedBlock::Table(table_block) => {
                Self::render_table(table_block, output);
                output.push_str("\n\n");
            }
            ParsedBlock::Quote { blocks } => {
                let mut quote_content = String::new();
                let mut quote_lists = ListColumns::default();
                for (idx, block) in blocks.iter().enumerate() {
                    Self::render_block_in(block, &mut quote_lists, &mut quote_content);
                    if Self::ends_list(block, blocks.get(idx + 1)) {
                        quote_content.push('\n');
                    }
                }

                // Every line gets "> ", including after a lone `\r`: a line
                // without it would end the quote and a code block in it.
                for line in split_lines(&quote_content) {
                    output.push_str("> ");
                    output.push_str(line);
                    output.push('\n');
                }
                output.push('\n');
            }
            ParsedBlock::HorizontalRule => {
                output.push_str("---\n\n");
            }
            ParsedBlock::Image { alt, title, src } => {
                output.push('!');
                output.push('[');
                if let Some(alt_text) = alt {
                    push_text(&one_line(alt_text), true, output);
                }
                output.push(']');
                output.push('(');
                match src.as_deref() {
                    Some(source) if !source.is_empty() => Self::push_destination(source, output),
                    // Without a destination the title would be read as one,
                    // and the rest of it as Markdown.
                    _ if title.is_some() => output.push_str("<>"),
                    _ => {}
                }
                if let Some(title_text) = title {
                    // Entities, as in destinations: `&` always goes as
                    // `&amp;`, and a backslash before it would escape it.
                    output.push_str(" \"");
                    for c in title_text.chars() {
                        match c {
                            '"' => output.push_str("&quot;"),
                            '\\' => output.push_str("&#92;"),
                            '&' => output.push_str("&amp;"),
                            c if c.is_control() => output.push(' '),
                            c => output.push(c),
                        }
                    }
                    output.push('"');
                }
                output.push(')');
                output.push_str("\n\n");
            }
            ParsedBlock::PageBreak => {
                output.push_str("\n\n---\n\n");
            }
        }
    }

    fn render_list_item(
        level: u8,
        ordered: bool,
        blocks: &[ParsedBlock],
        lists: &mut ListColumns,
        output: &mut String,
    ) {
        let marker = if ordered { "1. " } else { "- " };
        let indent = lists.indent(level, marker.len());
        output.push_str(&" ".repeat(indent));
        output.push_str(marker);
        // Every line after the first goes under the item's content. A line
        // left of it would end the item, and a code block's lines would
        // leave their fence and be read as Markdown and HTML.
        let continuation = " ".repeat(indent + marker.len());

        // Whether a line follows the marker yet: a blank line right after
        // it would leave the item empty and end it.
        let mut started = false;
        let mut blank_before = false;
        let mut previous: Option<&ParsedBlock> = None;
        for block in blocks {
            // Lines of two paragraphs in a row would join into one; the
            // blank line between them makes the list loose.
            if started && previous.is_some_and(Self::is_paragraph) && Self::is_paragraph(block) {
                blank_before = true;
            }
            previous = Some(block);
            let mut block_output = String::new();
            if matches!(block, ParsedBlock::HorizontalRule | ParsedBlock::PageBreak) {
                // `- ---` is a rule in place of the item, and `---` under a
                // paragraph line makes it a heading.
                block_output.push_str("***");
            } else {
                Self::render_block(block, &mut block_output);
            }
            for line in split_lines(block_output.trim_end()) {
                if started {
                    output.push('\n');
                    if blank_before {
                        output.push('\n');
                        blank_before = false;
                    }
                    if !line.is_empty() {
                        output.push_str(&continuation);
                    }
                } else if line.trim_matches([' ', '\t']).is_empty() {
                    continue;
                } else {
                    started = true;
                }
                output.push_str(line);
            }
            // A table takes the lines after it as rows, and a nested item
            // or quote takes a paragraph after it as its continuation, up
            // to a blank line.
            blank_before = started
                && (blank_before
                    || matches!(
                        block,
                        ParsedBlock::Table(_)
                            | ParsedBlock::ListItem { .. }
                            | ParsedBlock::Quote { .. }
                    ));
        }

        output.push('\n');
    }

    /// Blocks written as paragraph lines, which a following line continues.
    fn is_paragraph(block: &ParsedBlock) -> bool {
        matches!(
            block,
            ParsedBlock::Paragraph { .. } | ParsedBlock::Image { .. }
        )
    }

    fn render_inlines(inlines: &[crate::domain::ir::Inline], output: &mut String) {
        use crate::domain::ir::Inline;

        // Code waiting to be written. Code that follows code with nothing in
        // between joins it in one span: two spans side by side would touch
        // as one longer run of backticks, which closes neither of them.
        let mut code = String::new();
        let start = output.len();
        for inline in inlines {
            match inline {
                Inline::Text { text, style } => {
                    Self::render_styled_text(text, style, Escape::Text, &mut code, output);
                }
                Inline::Link {
                    text,
                    target,
                    style,
                } => {
                    flush_code(&mut code, output);
                    // Text never writes `\!`, so a `!` here is live and
                    // would turn the link into an image.
                    if output.len() > start && output.ends_with('!') {
                        output.pop();
                        output.push_str("\\!");
                    }
                    output.push('[');
                    let text = one_line(text);
                    // In a code span a `]` is not escaped, and `[`code]:` at
                    // the start of a line would be read as a link reference
                    // definition before the code span: such text goes as
                    // escaped text, without the code style.
                    let plain;
                    let style = if style.code
                        && (text.contains(']') || text.trim_start().starts_with('^'))
                    {
                        plain = crate::domain::ir::InlineStyle {
                            code: false,
                            ..style.clone()
                        };
                        &plain
                    } else {
                        style
                    };
                    let mut link_code = String::new();
                    Self::render_styled_text(
                        &text,
                        style,
                        Escape::LinkText,
                        &mut link_code,
                        output,
                    );
                    flush_code(&mut link_code, output);
                    output.push_str("](");
                    Self::push_destination(target, output);
                    output.push(')');
                }
                Inline::Code { text, style } => {
                    // Code inline takes precedence, then apply other styles
                    let mut styled = String::new();
                    Self::render_styled_text(
                        text,
                        style,
                        Escape::Code,
                        &mut String::new(),
                        &mut styled,
                    );
                    code.push_str(&styled);
                }
            }
        }
        flush_code(&mut code, output);
        drop_line_indents(output, start);
    }

    /// Writes a link or image destination. A plain one goes as is; one with
    /// characters that would end it early (spaces, parentheses, angle
    /// brackets, backslashes, control characters) goes in angle brackets with
    /// those escaped, so the destination cannot close the link and start a
    /// new one. `&` is written as `&amp;` in both: Markdown decodes entity
    /// references in destinations, and `javascript&#58;...` must not come
    /// back as the `javascript:` scheme that `link_target` turned away. A
    /// backslash is written as `&#92;`: before `&amp;` a backslash would
    /// escape the `&`.
    fn push_destination(target: &str, output: &mut String) {
        let plain = !target.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '(' | ')' | '<' | '>' | '\\')
        });
        if plain {
            for part in target.split_inclusive('&') {
                match part.strip_suffix('&') {
                    Some(head) => {
                        output.push_str(head);
                        output.push_str("&amp;");
                    }
                    None => output.push_str(part),
                }
            }
            return;
        }
        output.push('<');
        for c in target.chars() {
            match c {
                '&' => output.push_str("&amp;"),
                '<' => output.push_str("%3C"),
                '>' => output.push_str("%3E"),
                '\\' => output.push_str("&#92;"),
                c if c.is_control() => {
                    use std::fmt::Write;
                    let mut buf = [0; 4];
                    for byte in c.encode_utf8(&mut buf).bytes() {
                        _ = write!(output, "%{byte:02X}");
                    }
                }
                c => output.push(c),
            }
        }
        output.push('>');
    }

    /// `escape` says how `text` is written; inside a code span it is literal
    /// and written as is. Text in code style goes to `pending_code`, to be
    /// written as a code span by `flush_code`; anything else is written to
    /// `output`, after the pending code.
    fn render_styled_text(
        text: &str,
        style: &crate::domain::ir::InlineStyle,
        escape: Escape,
        pending_code: &mut String,
        output: &mut String,
    ) {
        // Emphasis markers must touch the text: leading and trailing
        // whitespace goes outside them ("**a **" is not bold in Markdown).
        let core = text.trim();
        if core.is_empty() {
            push_after_code(text, pending_code, output);
            return;
        }
        let start = text.len() - text.trim_start().len();
        let end = start + core.len();
        push_after_code(&text[..start], pending_code, output);
        let mut wrapped = String::with_capacity(core.len());
        let in_code = escape == Escape::Code;
        if in_code || style.code {
            wrapped.push_str(core);
        } else {
            push_text(core, escape == Escape::LinkText, &mut wrapped);
        }

        // Apply styles in order: code, bold, italic, underline, strike
        // Note: code is handled by Inline::Code variant, not here

        if style.strike {
            wrapped = format!("~~{wrapped}~~");
        }

        // Markdown has no underline (`__x__` is strong emphasis).
        if style.underline {
            // `></u>` alone on a line (after a quote marker) would start an
            // HTML block, where escapes are not read.
            if !in_code && !style.code && wrapped.ends_with('>') {
                wrapped.pop();
                wrapped.push_str("\\>");
            }
            wrapped = format!("<u>{wrapped}</u>");
        }

        if style.italic {
            wrapped = format!("*{wrapped}*");
        }

        if style.bold {
            wrapped = format!("**{wrapped}**");
        }

        if style.code && !in_code {
            pending_code.push_str(&wrapped);
        } else {
            push_after_code(&wrapped, pending_code, output);
        }
        push_after_code(&text[end..], pending_code, output);
    }

    fn render_table(table: &crate::domain::ir::TableBlock, output: &mut String) {
        if table.rows.is_empty() {
            return;
        }

        // The widest row decides the columns, so no cell is dropped.
        let num_cols = table.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);

        // A Markdown table needs a header row. Without one, an empty header
        // goes first, rather than the first row twice.
        let data_rows = if table.rows[0].is_header {
            Self::render_table_row(&table.rows[0], num_cols, output);
            &table.rows[1..]
        } else {
            output.push('|');
            for _ in 0..num_cols {
                output.push_str("  |");
            }
            &table.rows[..]
        };
        output.push('\n');

        // Render separator row
        output.push('|');
        for _ in 0..num_cols {
            output.push_str(" --- |");
        }
        output.push('\n');

        // Render data rows
        for row in data_rows {
            Self::render_table_row(row, num_cols, output);
            output.push('\n');
        }
    }

    fn render_table_row(row: &crate::domain::ir::TableRow, num_cols: usize, output: &mut String) {
        output.push('|');

        for i in 0..num_cols {
            output.push(' ');

            if let Some(cell) = row.cells.get(i) {
                let cell_content = Self::render_cell_content(&cell.blocks);
                let escaped = Self::escape_table_content(&cell_content);
                output.push_str(&escaped);
            }

            output.push_str(" |");
        }
    }

    fn render_cell_content(blocks: &[ParsedBlock]) -> String {
        let mut content = String::new();

        for (idx, block) in blocks.iter().enumerate() {
            if idx > 0 {
                content.push_str("<br/>");
            }

            let mut block_output = String::new();
            Self::render_block(block, &mut block_output);

            // Remove trailing whitespace and newlines for cell content
            let trimmed = block_output.trim();
            content.push_str(trimmed);
        }

        content
    }

    /// Escapes pipes as `\|`, and turns line breaks into `<br/>`: a raw
    /// newline would end the table row in the middle of the cell. GFM drops
    /// the backslash before an escaped pipe before it parses the cell,
    /// inside code too, and readers agree on that only after an odd run of
    /// backslashes: `\\|` is kept whole by some and loses a backslash in
    /// others. So a pipe after an even run (none, or escaped backslashes of
    /// text or code) gets one more backslash, and one after an odd run is
    /// left alone: it was escaped by a table nested in the cell. (So `\|` in
    /// code shows as `|`.)
    fn escape_table_content(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        let mut prev = None;
        let mut backslashes = 0_usize;
        for c in text.chars() {
            match c {
                '|' if backslashes.is_multiple_of(2) => escaped.push_str("\\|"),
                '\n' if prev == Some('\r') => {}
                '\r' | '\n' => escaped.push_str("<br/>"),
                c => escaped.push(c),
            }
            backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            prev = Some(c);
        }
        escaped
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::ir::{
        Inline, InlineStyle, ParsedMetadata, ParsedSource, TableBlock, TableCell, TableRow,
    };

    #[test]
    fn test_render_heading() {
        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![
                ParsedBlock::Heading {
                    level: 1,
                    inlines: vec![Inline::plain("Title")],
                },
                ParsedBlock::Heading {
                    level: 2,
                    inlines: vec![Inline::plain("Subtitle")],
                },
            ],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("# Title\n"));
        assert!(markdown.contains("## Subtitle\n"));
    }

    #[test]
    fn test_render_paragraph() {
        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Paragraph {
                inlines: vec![Inline::plain("Hello world")],
            }],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("Hello world\n"));
    }

    #[test]
    fn test_render_styled_text() {
        let style = InlineStyle {
            bold: true,
            italic: true,
            ..Default::default()
        };

        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Paragraph {
                inlines: vec![Inline::styled("Bold and italic", style)],
            }],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("**") && markdown.contains('*'));
    }

    #[test]
    fn test_render_list() {
        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![
                ParsedBlock::ListItem {
                    level: 0,
                    ordered: false,
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Item 1")],
                    }],
                },
                ParsedBlock::ListItem {
                    level: 1,
                    ordered: false,
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain("Nested item")],
                    }],
                },
            ],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("- Item 1\n"));
        assert!(markdown.contains("  - Nested item\n"));
    }

    #[test]
    fn test_render_code_block() {
        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::CodeBlock {
                language: Some("rust".to_owned()),
                code: "fn main() {\n    println!(\"Hello\");\n}".to_owned(),
            }],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("```rust\n"));
        assert!(markdown.contains("fn main()"));
    }

    #[test]
    fn test_render_simple_table() {
        let table = TableBlock {
            rows: vec![
                TableRow {
                    is_header: true,
                    cells: vec![
                        TableCell {
                            blocks: vec![ParsedBlock::Paragraph {
                                inlines: vec![Inline::plain("Name")],
                            }],
                        },
                        TableCell {
                            blocks: vec![ParsedBlock::Paragraph {
                                inlines: vec![Inline::plain("Age")],
                            }],
                        },
                    ],
                },
                TableRow {
                    is_header: false,
                    cells: vec![
                        TableCell {
                            blocks: vec![ParsedBlock::Paragraph {
                                inlines: vec![Inline::plain("Alice")],
                            }],
                        },
                        TableCell {
                            blocks: vec![ParsedBlock::Paragraph {
                                inlines: vec![Inline::plain("30")],
                            }],
                        },
                    ],
                },
            ],
        };

        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Table(table)],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.contains("| Name |"));
        assert!(markdown.contains("| Age |"));
        assert!(markdown.contains("| --- |"));
        assert!(markdown.contains("| Alice |"));
    }

    #[test]
    fn test_render_table_with_escaped_content() {
        let table = TableBlock {
            rows: vec![
                TableRow {
                    is_header: true,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Column")],
                        }],
                    }],
                },
                TableRow {
                    is_header: false,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Pipe|test")],
                        }],
                    }],
                },
                TableRow {
                    is_header: false,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Backslash\\test")],
                        }],
                    }],
                },
                TableRow {
                    is_header: false,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Line one\nLine two\r\nLine three")],
                        }],
                    }],
                },
            ],
        };

        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Table(table)],
        };

        let markdown = MarkdownRenderer::render(&doc);
        // Pipes are escaped; a backslash before a letter is no escape.
        assert!(markdown.contains("Pipe\\|test"));
        assert!(markdown.contains("| Backslash\\test |"));
        // A line break inside a cell must not end the table row
        assert!(markdown.contains("| Line one<br/>Line two<br/>Line three |"));
    }

    #[test]
    fn test_render_nested_table() {
        let inner_table = TableBlock {
            rows: vec![
                TableRow {
                    is_header: true,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Inner")],
                        }],
                    }],
                },
                TableRow {
                    is_header: false,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Data")],
                        }],
                    }],
                },
            ],
        };

        let outer_table = TableBlock {
            rows: vec![
                TableRow {
                    is_header: true,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("Outer")],
                        }],
                    }],
                },
                TableRow {
                    is_header: false,
                    cells: vec![TableCell {
                        blocks: vec![ParsedBlock::Table(inner_table)],
                    }],
                },
            ],
        };

        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Table(outer_table)],
        };

        let markdown = MarkdownRenderer::render(&doc);
        // Should contain both tables rendered
        assert!(markdown.contains("Outer"));
        assert!(markdown.contains("Inner"));
    }

    #[test]
    fn test_render_with_title() {
        let doc = ParsedDocument {
            id: None,
            title: Some("Document Title".to_owned()),
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Paragraph {
                inlines: vec![Inline::plain("Content")],
            }],
        };

        let markdown = MarkdownRenderer::render(&doc);
        assert!(markdown.starts_with("# Document Title\n"));
    }

    #[test]
    fn line_breaks_in_the_title_and_file_name_stay_on_their_line() {
        let doc = ParsedDocument {
            id: None,
            title: Some("a\n\n---\nlanguage: x".to_owned()),
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("x".to_owned()),
                original_filename: Some("f.txt\r\ncontent-type: text/html".to_owned()),
                content_type: Some("text/plain".to_owned()),
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![],
        };
        assert_eq!(
            MarkdownRenderer::render(&doc),
            "# a  --- language: x\n\n---\nfilename: f.txt  content-type: text/html\ncontent-type: text/plain\n---\n\n"
        );
    }

    #[test]
    fn test_render_iter_streaming() {
        let doc = ParsedDocument {
            id: None,
            title: Some("Test Title".to_owned()),
            language: Some("en".to_owned()),
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: Some("test.txt".to_owned()),
                content_type: Some("text/plain".to_owned()),
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![
                ParsedBlock::Heading {
                    level: 2,
                    inlines: vec![Inline::plain("Section 1")],
                },
                ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain("First paragraph")],
                },
                ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain("Second paragraph")],
                },
            ],
        };

        // Collect chunks from iterator using render_iter_ref
        let chunks: Vec<String> = MarkdownRenderer::render_iter_ref(&doc).collect();

        // Should have header + 3 blocks = 4 chunks
        assert_eq!(chunks.len(), 4);

        // First chunk is header with title and metadata
        assert!(chunks[0].contains("# Test Title"));
        assert!(chunks[0].contains("language: en"));
        assert!(chunks[0].contains("filename: test.txt"));
        assert!(chunks[0].contains("content-type: text/plain"));

        // Remaining chunks are blocks
        assert!(chunks[1].contains("## Section 1"));
        assert!(chunks[2].contains("First paragraph"));
        assert!(chunks[3].contains("Second paragraph"));

        // Streamed result should match non-streamed
        let streamed = chunks.join("");
        let non_streamed = MarkdownRenderer::render(&doc);
        assert_eq!(streamed, non_streamed);
    }

    #[test]
    fn test_render_iter_no_header() {
        let doc = ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("test.txt".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks: vec![ParsedBlock::Paragraph {
                inlines: vec![Inline::plain("Only content")],
            }],
        };

        let chunks: Vec<String> = MarkdownRenderer::render_iter_ref(&doc).collect();

        // Should have only 1 chunk (the paragraph, no header)
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("Only content"));
    }

    fn doc_of(blocks: Vec<ParsedBlock>) -> ParsedDocument {
        ParsedDocument {
            id: None,
            title: None,
            language: None,
            meta: ParsedMetadata {
                source: ParsedSource::LocalPath("t".to_owned()),
                original_filename: None,
                content_type: None,
                created_at: None,
                modified_at: None,
                is_stub: false,
            },
            blocks,
        }
    }

    fn para(inlines: Vec<Inline>) -> ParsedBlock {
        ParsedBlock::Paragraph { inlines }
    }

    #[test]
    fn a_paragraph_after_a_list_is_its_own_block() {
        let item = ParsedBlock::ListItem {
            level: 0,
            ordered: false,
            blocks: vec![para(vec![Inline::plain("item")])],
        };
        let markdown = MarkdownRenderer::render(&doc_of(vec![
            item.clone(),
            item,
            para(vec![Inline::plain("after")]),
        ]));
        assert_eq!(markdown, "- item\n- item\n\nafter\n\n");
    }

    #[test]
    fn emphasis_markers_touch_the_text() {
        let bold = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let underline = InlineStyle {
            underline: true,
            ..InlineStyle::default()
        };
        let markdown = MarkdownRenderer::render(&doc_of(vec![para(vec![
            Inline::styled("Text title ", bold.clone()),
            Inline::styled("spaced", underline),
            Inline::styled("  ", bold),
        ])]));
        assert_eq!(markdown, "**Text title** <u>spaced</u>  \n\n");
    }

    #[test]
    fn a_table_without_a_header_gets_an_empty_one() {
        let row = |text: &str| TableRow {
            is_header: false,
            cells: vec![TableCell {
                blocks: vec![para(vec![Inline::plain(text)])],
            }],
        };
        let markdown = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Table(TableBlock {
            rows: vec![row("a"), row("b")],
        })]));
        assert_eq!(markdown, "|  |\n| --- |\n| a |\n| b |\n\n\n");
    }

    fn table_row(is_header: bool, cells: &[&str]) -> TableRow {
        TableRow {
            is_header,
            cells: cells
                .iter()
                .map(|text| TableCell {
                    blocks: vec![para(vec![Inline::plain(*text)])],
                })
                .collect(),
        }
    }

    #[test]
    fn the_widest_row_decides_the_columns() {
        let with_header = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Table(TableBlock {
            rows: vec![table_row(true, &["h"]), table_row(false, &["a", "b", "c"])],
        })]));
        assert_eq!(
            with_header,
            "| h |  |  |\n| --- | --- | --- |\n| a | b | c |\n\n\n"
        );
        let without = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Table(TableBlock {
            rows: vec![table_row(false, &["a"]), table_row(false, &["b", "c"])],
        })]));
        assert_eq!(without, "|  |  |\n| --- | --- |\n| a |  |\n| b | c |\n\n\n");
    }

    #[test]
    fn a_list_inside_a_quote_ends_before_the_next_block() {
        let markdown = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Quote {
            blocks: vec![
                ParsedBlock::ListItem {
                    level: 0,
                    ordered: false,
                    blocks: vec![para(vec![Inline::plain("a")])],
                },
                para(vec![Inline::plain("b")]),
            ],
        }]));
        assert_eq!(markdown, "> - a\n> \n> b\n> \n\n");
    }

    #[test]
    fn destinations_cannot_close_the_link() {
        let link = |target: &str| {
            MarkdownRenderer::render(&doc_of(vec![para(vec![Inline::Link {
                text: "t".to_owned(),
                target: target.to_owned(),
                style: InlineStyle::default(),
            }])]))
        };
        assert_eq!(
            link("https://example.com/a"),
            "[t](https://example.com/a)\n\n"
        );
        assert_eq!(
            link("x) [y](javascript:alert(1)"),
            "[t](<x) [y](javascript:alert(1)>)\n\n"
        );
        assert_eq!(link("a<b>\\c"), "[t](<a%3Cb%3E&#92;c>)\n\n");
        // Control characters cannot end the destination or the line.
        assert_eq!(link("a\nb)"), "[t](<a%0Ab)>)\n\n");
        assert_eq!(link("a\u{7f}b"), "[t](<a%7Fb>)\n\n");

        let image = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Image {
            alt: Some("alt".to_owned()),
            title: Some("say \"hi\") [x](javascript:1)".to_owned()),
            src: Some("pic one.png".to_owned()),
        }]));
        assert_eq!(
            image,
            "![alt](<pic one.png> \"say &quot;hi&quot;) [x](javascript:1)\")\n\n"
        );
        let image = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Image {
            alt: None,
            title: Some("x\ny\u{0}z".to_owned()),
            src: Some("p.png".to_owned()),
        }]));
        assert_eq!(image, "![](p.png \"x y z\")\n\n");
    }

    #[test]
    fn escapes_survive_table_cells() {
        let cell = |block: ParsedBlock| TableCell {
            blocks: vec![block],
        };
        let table = ParsedBlock::Table(TableBlock {
            rows: vec![TableRow {
                is_header: true,
                cells: vec![
                    cell(ParsedBlock::Image {
                        alt: None,
                        title: Some("x\" [y](javascript:alert(1)) \"".to_owned()),
                        src: Some("a b\\c.png".to_owned()),
                    }),
                    cell(para(vec![Inline::plain("C:\\dir")])),
                ],
            }],
        });
        let markdown = MarkdownRenderer::render(&doc_of(vec![table]));
        assert!(
            markdown.contains("![](<a b&#92;c.png> \"x&quot; [y](javascript:alert(1)) &quot;\")"),
            "{markdown}"
        );
        assert!(markdown.contains("| C:\\dir |"), "{markdown}");
    }

    #[test]
    fn entity_references_in_destinations_stay_literal() {
        let link = |target: &str| {
            MarkdownRenderer::render(&doc_of(vec![para(vec![Inline::Link {
                text: "t".to_owned(),
                target: target.to_owned(),
                style: InlineStyle::default(),
            }])]))
        };
        // No `:`, so `link_target` keeps these as relative links; decoded
        // they would read `javascript:alert(1)`.
        assert_eq!(
            link("javascript&#58;alert(1)"),
            "[t](<javascript&amp;#58;alert(1)>)\n\n"
        );
        assert_eq!(
            link("javascript&colon;alert(1)"),
            "[t](<javascript&amp;colon;alert(1)>)\n\n"
        );
        assert_eq!(
            link("a b&amp;c"),
            "[t](<a b&amp;amp;c>)\n\n",
            "a literal `&amp;` stays literal"
        );
        assert_eq!(link("/q?a=1&b=2"), "[t](/q?a=1&amp;b=2)\n\n");
        assert_eq!(link("javascript&#58;x"), "[t](javascript&amp;#58;x)\n\n");
    }

    /// Renders `block` on its own and inside a table cell, and checks that
    /// the streaming renderer gives the same output as the eager one.
    fn plain_and_in_cell(block: ParsedBlock) -> (String, String) {
        let render = |doc: ParsedDocument| {
            let eager = MarkdownRenderer::render(&doc);
            let streamed: String = MarkdownRenderer::render_iter(doc).collect();
            assert_eq!(eager, streamed);
            eager
        };
        let plain = render(doc_of(vec![block.clone()]));
        let table = render(doc_of(vec![ParsedBlock::Table(TableBlock {
            rows: vec![TableRow {
                is_header: true,
                cells: vec![TableCell {
                    blocks: vec![block],
                }],
            }],
        })]));
        (plain, table)
    }

    #[track_caller]
    fn assert_renders(block: ParsedBlock, plain: &str, cell: &str) {
        let (got_plain, got_table) = plain_and_in_cell(block);
        assert_eq!(got_plain, format!("{plain}\n\n"));
        assert_eq!(got_table, format!("| {cell} |\n| --- |\n\n\n"));
    }

    #[test]
    fn link_shaped_text_stays_text() {
        let escaped = r"\[click](javascript:alert(1))";
        assert_renders(
            para(vec![Inline::plain("[click](javascript:alert(1))")]),
            escaped,
            escaped,
        );
    }

    #[test]
    fn link_text_cannot_close_the_link() {
        let link = Inline::Link {
            text: "x](javascript:alert(1)) [y".to_owned(),
            target: "https://ok".to_owned(),
            style: InlineStyle::default(),
        };
        let escaped = r"[x\](javascript:alert(1)) \[y](https://ok)";
        assert_renders(para(vec![link]), escaped, escaped);
    }

    #[test]
    fn a_bang_before_a_link_does_not_make_an_image() {
        let link = || Inline::link("t", "https://ok");
        assert_renders(
            para(vec![Inline::plain("see!"), link()]),
            r"see\![t](https://ok)",
            r"see\![t](https://ok)",
        );
        // A doubled backslash before the `!` leaves it live.
        assert_renders(
            para(vec![Inline::plain("a\\!"), link()]),
            r"a\\\![t](https://ok)",
            r"a\\\![t](https://ok)",
        );
        // Elsewhere a `!` stays as is.
        assert_renders(
            para(vec![Inline::plain("hi! "), link(), Inline::plain("!")]),
            "hi! [t](https://ok)!",
            "hi! [t](https://ok)!",
        );
    }

    #[test]
    fn image_alt_cannot_close_the_image() {
        let image = ParsedBlock::Image {
            alt: Some("a](javascript:1) ![b".to_owned()),
            title: None,
            src: Some("p.png".to_owned()),
        };
        let escaped = r"![a\](javascript:1) !\[b](p.png)";
        assert_renders(image, escaped, escaped);
    }

    #[test]
    fn raw_html_in_text_stays_text() {
        let escaped = r"\<script>alert(1)\</script> \<img src=x onerror=alert(1)>";
        assert_renders(
            para(vec![Inline::plain(
                "<script>alert(1)</script> <img src=x onerror=alert(1)>",
            )]),
            escaped,
            escaped,
        );
    }

    #[test]
    fn text_that_looks_like_references_stays_literal() {
        // Each `&amp;` decodes back to `&`, so a renderer shows the original.
        let escaped = "&amp;lt;b&amp;gt; &amp;#91; &amp;#x5B; &amp;amp;";
        assert_renders(
            para(vec![Inline::plain("&lt;b&gt; &#91; &#x5B; &amp;")]),
            escaped,
            escaped,
        );
    }

    #[test]
    fn ampersands_that_start_no_reference_stay() {
        let text = "AT&T a & b &; &#; &#x; &#xZ; &a-b;";
        assert_renders(para(vec![Inline::plain(text)]), text, text);
    }

    #[test]
    fn text_escapes_compose_with_table_escapes() {
        assert_renders(
            para(vec![Inline::plain("[a|b]\\")]),
            r"\[a|b]\\",
            r"\[a\|b]\\",
        );
        // An escaped backslash before a pipe: GFM drops one backslash
        // before the pipe in a cell, so the pipe gets one of its own.
        assert_renders(para(vec![Inline::plain(r"a\|b")]), r"a\\|b", r"a\\\|b");
        assert_renders(
            para(vec![Inline::Code {
                text: r"a\\|b".to_owned(),
                style: InlineStyle::default(),
            }]),
            r"`a\\|b`",
            r"`a\\\|b`",
        );
    }

    #[test]
    fn backslashes_in_text_cannot_escape_what_follows() {
        // Doubled before punctuation, which includes the escapes written
        // for `[` and `<` and the `&` of `&amp;`: `\[x` would show as `[x`.
        let escaped = r"\\\[x\\] \\\<b> \\&amp;lt; \\\ \\\` a\\";
        assert_renders(
            para(vec![Inline::plain(r"\[x\] \<b> \&lt; \\ \` a\")]),
            escaped,
            escaped,
        );
        // Before a letter, a space or other text a backslash is no escape.
        let path = "C:\\Users\\x \\ \u{e9}\\\u{201d}";
        assert_renders(para(vec![Inline::plain(path)]), path, path);
        // Before a line ending it would be a hard line break; at the end
        // of the text, the renderer's markers follow it.
        let bold = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        assert_renders(
            para(vec![Inline::plain("a\\\nb "), Inline::styled(r"c\", bold)]),
            "a\\\\\nb **c\\\\**",
            r"a\\<br/>b **c\\**",
        );
    }

    #[test]
    fn text_cannot_open_code() {
        assert_renders(
            para(vec![Inline::plain("a `b` ``c``")]),
            r"a \`b\` \`\`c\`\`",
            r"a \`b\` \`\`c\`\`",
        );
        // Fences inside one multi-line run.
        let lines = "x\n```\ny\n  ~~~\n~a ~b~~c";
        let escaped = "x\n\\`\\`\\`\ny\n\\~\\~\\~\n~a ~b~~c";
        assert_renders(
            para(vec![Inline::plain(lines)]),
            escaped,
            &escaped.replace('\n', "<br/>"),
        );
    }

    #[test]
    fn code_spans_outlast_backticks_in_the_code() {
        let code = |text: &str| Inline::Code {
            text: text.to_owned(),
            style: InlineStyle::default(),
        };
        let code_style = InlineStyle {
            code: true,
            ..InlineStyle::default()
        };
        let plain = "```a``b``` ``  ` `` `` `x `` ``x`y`` ```a ``b```";
        assert_renders(
            para(vec![
                code("a``b"),
                Inline::plain(" "),
                code(" `"),
                Inline::plain(" "),
                code("`x"),
                Inline::plain(" "),
                Inline::styled("x`y", code_style),
                Inline::plain(" "),
                code("a\n``b"),
            ]),
            plain,
            plain,
        );
    }

    #[test]
    fn code_style_link_text_cannot_close_the_link() {
        let link = |text: &str| Inline::Link {
            text: text.to_owned(),
            target: "https://ok".to_owned(),
            style: InlineStyle {
                code: true,
                ..InlineStyle::default()
            },
        };
        // A `]` in a code span would end the label of a link reference
        // definition (`[`a]: b`](...)` would vanish), so such text goes as
        // escaped text.
        let escaped = r"[a\` \](https://evil) \`b](https://ok)";
        assert_renders(para(vec![link("a` ](https://evil) `b")]), escaped, escaped);
        assert_renders(
            para(vec![link("a]: b")]),
            r"[a\]: b](https://ok)",
            r"[a\]: b](https://ok)",
        );
        assert_renders(
            para(vec![link("^a")]),
            r"[\^a](https://ok)",
            r"[\^a](https://ok)",
        );
        assert_renders(
            para(vec![link("a[b")]),
            "[`a[b`](https://ok)",
            "[`a[b`](https://ok)",
        );
    }

    #[test]
    fn code_blocks_outlast_backticks_in_the_code() {
        let block = |language: Option<&str>, code: &str| {
            MarkdownRenderer::render(&doc_of(vec![ParsedBlock::CodeBlock {
                language: language.map(str::to_owned),
                code: code.to_owned(),
            }]))
        };
        assert_eq!(
            block(Some("rust"), "a\n```\n<b>\n"),
            "````rust\na\n```\n<b>\n````\n\n"
        );
        assert_eq!(block(None, "`````"), "``````\n`````\n``````\n\n");
        assert_eq!(block(Some("c++"), "x"), "```c++\nx\n```\n\n");
        assert_eq!(block(Some("c#"), "x"), "```c#\nx\n```\n\n");
        for language in ["js`", "js x", "js\n<b>", "<b>", ""] {
            assert_eq!(
                block(Some(language), "x"),
                "```\nx\n```\n\n",
                "{language:?}"
            );
        }
    }

    #[test]
    fn title_and_metadata_are_escaped() {
        let mut doc = doc_of(vec![]);
        doc.title = Some("<img src=x onerror=alert(1)> [x](https://evil)".to_owned());
        doc.language = Some("`en`".to_owned());
        doc.meta.original_filename = Some("<img src=x onerror=1>.txt".to_owned());
        doc.meta.content_type = Some("text/html; x=[y]".to_owned());
        let eager = MarkdownRenderer::render(&doc);
        let streamed: String = MarkdownRenderer::render_iter(doc).collect();
        assert_eq!(eager, streamed);
        assert_eq!(
            eager,
            "# \\<img src=x onerror=alert(1)> \\[x](https://evil)\n\n---\n\
             language: \\`en\\`\nfilename: \\<img src=x onerror=1>.txt\n\
             content-type: text/html; x=\\[y]\n---\n\n"
        );
    }

    #[test]
    fn a_block_after_a_table_in_a_list_item_is_not_a_row() {
        // As a row, the paragraph would be split at the `|` in its code, and
        // the code would be read as Markdown and HTML.
        let markdown = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::ListItem {
            level: 0,
            ordered: false,
            blocks: vec![
                ParsedBlock::Table(TableBlock {
                    rows: vec![table_row(true, &["a"])],
                }),
                para(vec![Inline::Code {
                    text: "<b>|</b>".to_owned(),
                    style: InlineStyle::default(),
                }]),
            ],
        }]));
        assert_eq!(markdown, "- | a |\n  | --- |\n\n  `<b>|</b>`\n");
    }

    #[test]
    fn a_table_in_a_cell_escapes_each_pipe_once() {
        // Escaped again, the inner `\|` would be `\\|`, which some readers
        // take as a backslash and a cell boundary: the image would break and
        // its title would be read as Markdown.
        let table = |blocks: Vec<ParsedBlock>| {
            ParsedBlock::Table(TableBlock {
                rows: vec![TableRow {
                    is_header: true,
                    cells: vec![TableCell { blocks }],
                }],
            })
        };
        let inner = table(vec![
            ParsedBlock::Image {
                alt: Some("a".to_owned()),
                title: Some("[b](e)|".to_owned()),
                src: Some("ok".to_owned()),
            },
            ParsedBlock::CodeBlock {
                language: None,
                code: "x|y".to_owned(),
            },
        ]);
        assert_eq!(
            MarkdownRenderer::render(&doc_of(vec![table(vec![inner])])),
            "| \\| ![a](ok \"[b](e)\\|\")<br/>```<br/>x\\|y<br/>``` \\|<br/>\\| --- \\| |\n| --- |\n\n\n"
        );
    }

    #[test]
    fn a_nested_item_starts_at_its_parents_content() {
        // Two columns per level would put the code block's item four
        // columns into "1. b": too far in to start an item, so the lines
        // would be read as text of "b", the code as Markdown and HTML.
        let item = |level: u8, ordered: bool, block: ParsedBlock| ParsedBlock::ListItem {
            level,
            ordered,
            blocks: vec![block],
        };
        let blocks = vec![
            item(0, true, para(vec![Inline::plain("a")])),
            item(1, true, para(vec![Inline::plain("b")])),
            item(
                2,
                false,
                ParsedBlock::CodeBlock {
                    language: None,
                    code: "<script>x</script>".to_owned(),
                },
            ),
            // A level skipped nests one deeper, not four columns in.
            item(0, false, para(vec![Inline::plain("c")])),
            item(2, false, para(vec![Inline::plain("d")])),
            para(vec![Inline::plain("after")]),
            item(1, true, para(vec![Inline::plain("e")])),
        ];
        let expected = "1. a\n   1. b\n      - ```\n        <script>x</script>\n        ```\n\
                        - c\n  - d\n\nafter\n\n1. e\n";
        let doc = doc_of(blocks.clone());
        let streamed: String = MarkdownRenderer::render_iter(doc.clone()).collect();
        assert_eq!(MarkdownRenderer::render(&doc), expected);
        assert_eq!(streamed, expected);
        let quoted = MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Quote { blocks }]));
        let mut lines = String::new();
        for line in expected.lines() {
            lines.push_str("> ");
            lines.push_str(line);
            lines.push('\n');
        }
        assert_eq!(quoted, lines + "\n");
    }

    #[test]
    fn link_text_and_alt_cannot_break_the_link() {
        // A line break could end the paragraph or start a block inside the
        // brackets; the rest, destination and title included, would then
        // be read as Markdown.
        let link = Inline::Link {
            text: "a\n\nb\r# c".to_owned(),
            target: "https://ok/ [y](javascript:alert(1))".to_owned(),
            style: InlineStyle::default(),
        };
        let escaped = "[a  b # c](<https://ok/ [y](javascript:alert(1))>)";
        assert_renders(para(vec![link]), escaped, escaped);
        let image = ParsedBlock::Image {
            alt: Some("a\r\n\r\nb".to_owned()),
            title: Some("<x>".to_owned()),
            src: Some("p.png".to_owned()),
        };
        let escaped = "![a    b](p.png \"<x>\")";
        assert_renders(image, escaped, escaped);
    }

    #[test]
    fn adjacent_code_spans_become_one() {
        // Written apart, `x` and ```a``b``` would touch as "````": no span
        // would close, and the code would be read as Markdown and HTML.
        let code = |text: &str| Inline::Code {
            text: text.to_owned(),
            style: InlineStyle::default(),
        };
        let code_style = InlineStyle {
            code: true,
            ..InlineStyle::default()
        };
        let merged = "```<img src=x onerror=1>a``b` c``` `d`";
        assert_renders(
            para(vec![
                code("<img src=x onerror=1>"),
                code("a``b"),
                Inline::styled("` c", code_style.clone()),
                Inline::plain(" "),
                Inline::styled("d", code_style),
            ]),
            merged,
            merged,
        );
    }

    #[test]
    fn code_in_a_table_cell_keeps_its_backslashes() {
        // GFM turns `\|` in a cell back into `|` before it parses the cell,
        // but a doubled backslash would stay doubled inside code.
        assert_renders(
            para(vec![Inline::Code {
                text: "a\\b|c".to_owned(),
                style: InlineStyle::default(),
            }]),
            "`a\\b|c`",
            "`a\\b\\|c`",
        );
        assert_renders(
            ParsedBlock::CodeBlock {
                language: None,
                code: "C:\\x|y".to_owned(),
            },
            "```\nC:\\x|y\n```",
            "```<br/>C:\\x\\|y<br/>```",
        );
    }

    #[test]
    fn tildes_cannot_open_a_fence() {
        let strike = InlineStyle {
            strike: true,
            ..InlineStyle::default()
        };
        // After a list or quote marker, which text does not escape.
        assert_renders(
            para(vec![Inline::plain("x\n- ~~~\n> ~~~ a~~b")]),
            "x\n- \\~\\~\\~\n> \\~\\~\\~ a~~b",
            r"x<br/>- \~\~\~<br/>> \~\~\~ a~~b",
        );
        // Next to the renderer's own `~~` strikethrough markers.
        assert_renders(
            para(vec![
                Inline::plain("x\n- ~~"),
                Inline::styled("y", strike.clone()),
                Inline::plain(" "),
                Inline::styled("~z", strike.clone()),
            ]),
            "x\n- \\~\\~~~y~~ ~~\\~z~~",
            r"x<br/>- \~\~~~y~~ ~~\~z~~",
        );
        // The whole run: pulldown-cmark reads `~~\~~x~~` as no
        // strikethrough.
        assert_renders(
            para(vec![Inline::styled("~~x", strike)]),
            r"~~\~\~x~~",
            r"~~\~\~x~~",
        );
    }

    #[test]
    fn a_code_block_in_a_list_item_stays_in_the_item() {
        let code = ParsedBlock::CodeBlock {
            language: None,
            code: "<script>x</script>\n\n```".to_owned(),
        };
        let item = |ordered: bool, blocks: Vec<ParsedBlock>| {
            MarkdownRenderer::render(&doc_of(vec![ParsedBlock::ListItem {
                level: 0,
                ordered,
                blocks,
            }]))
        };
        assert_eq!(
            item(false, vec![code.clone()]),
            "- ````\n  <script>x</script>\n\n  ```\n  ````\n"
        );
        assert_eq!(
            item(true, vec![para(vec![Inline::plain("p\n\nq")]), code]),
            "1. p\n\n   q\n   ````\n   <script>x</script>\n\n   ```\n   ````\n"
        );
    }

    fn eager_and_streamed(blocks: Vec<ParsedBlock>) -> String {
        let doc = doc_of(blocks);
        let eager = MarkdownRenderer::render(&doc);
        let streamed: String = MarkdownRenderer::render_iter(doc).collect();
        assert_eq!(eager, streamed);
        eager
    }

    #[test]
    fn leading_line_breaks_do_not_end_the_item() {
        // A blank line right after the marker would leave the item empty
        // and the rest of it outside the list.
        let item = |ordered: bool, blocks: Vec<ParsedBlock>| {
            eager_and_streamed(vec![ParsedBlock::ListItem {
                level: 0,
                ordered,
                blocks,
            }])
        };
        let code = ParsedBlock::CodeBlock {
            language: None,
            code: "<b>".to_owned(),
        };
        assert_eq!(
            item(
                true,
                vec![para(vec![Inline::plain("\n \n\nfoo")]), code.clone()]
            ),
            "1. foo\n   ```\n   <b>\n   ```\n"
        );
        assert_eq!(
            item(false, vec![para(vec![Inline::plain("\n")]), code]),
            "- ```\n  <b>\n  ```\n"
        );
        // `- ---` would be a rule in place of the item, and `---` under a
        // paragraph line would make it a heading.
        assert_eq!(
            item(
                false,
                vec![ParsedBlock::PageBreak, para(vec![Inline::plain("a")])]
            ),
            "- ***\n  a\n"
        );
        assert_eq!(
            item(
                false,
                vec![para(vec![Inline::plain("a")]), ParsedBlock::HorizontalRule]
            ),
            "- a\n  ***\n"
        );
    }

    #[test]
    fn a_block_after_a_nested_item_or_quote_is_not_their_continuation() {
        let item = |level: u8, blocks: Vec<ParsedBlock>| ParsedBlock::ListItem {
            level,
            ordered: false,
            blocks,
        };
        let a = || para(vec![Inline::plain("a")]);
        let b = || para(vec![Inline::plain("b")]);
        assert_eq!(
            eager_and_streamed(vec![item(0, vec![item(1, vec![a()]), b()])]),
            "- - a\n\n  b\n"
        );
        assert_eq!(
            eager_and_streamed(vec![item(
                0,
                vec![a(), ParsedBlock::Quote { blocks: vec![a()] }, b()]
            )]),
            "- a\n  > a\n  >\n\n  b\n"
        );
    }

    #[test]
    fn paragraphs_in_one_item_stay_apart() {
        let item = |blocks: Vec<ParsedBlock>| ParsedBlock::ListItem {
            level: 0,
            ordered: false,
            blocks,
        };
        let p = |t: &str| para(vec![Inline::plain(t)]);
        let image = ParsedBlock::Image {
            alt: Some("i".to_owned()),
            title: None,
            src: None,
        };
        assert_eq!(
            eager_and_streamed(vec![item(vec![p("a"), p("b")]), item(vec![p("c")])]),
            "- a\n\n  b\n- c\n"
        );
        assert_eq!(
            eager_and_streamed(vec![item(vec![image.clone(), p("b"), image])]),
            "- ![i]()\n\n  b\n\n  ![i]()\n"
        );
        // A heading ends at its line: no blank line needed.
        let heading = ParsedBlock::Heading {
            level: 2,
            inlines: vec![Inline::plain("h")],
        };
        assert_eq!(
            eager_and_streamed(vec![item(vec![heading, p("b")])]),
            "- ## h\n  b\n"
        );
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line_in_a_quote() {
        let quote = |blocks: Vec<ParsedBlock>| {
            MarkdownRenderer::render(&doc_of(vec![ParsedBlock::Quote { blocks }]))
        };
        assert_eq!(
            quote(vec![ParsedBlock::CodeBlock {
                language: None,
                code: "a\r<script>x</script>\r\nb".to_owned(),
            }]),
            "> ```\n> a\n> <script>x</script>\n> b\n> ```\n> \n\n"
        );
        assert_eq!(quote(vec![]), "\n");
    }

    #[test]
    fn code_and_styles_are_not_escaped() {
        let bold = InlineStyle {
            bold: true,
            ..InlineStyle::default()
        };
        let code = InlineStyle {
            code: true,
            ..InlineStyle::default()
        };
        let underline = InlineStyle {
            underline: true,
            ..InlineStyle::default()
        };
        let markdown = MarkdownRenderer::render(&doc_of(vec![
            para(vec![
                Inline::Code {
                    text: "[a]<b>&lt;".to_owned(),
                    style: InlineStyle::default(),
                },
                Inline::plain(" "),
                Inline::styled("[c]", code),
                Inline::plain(" "),
                Inline::styled("<d>", bold),
                Inline::plain(" "),
                Inline::styled("*e* _f_ #g", underline),
            ]),
            ParsedBlock::CodeBlock {
                language: None,
                code: "<x>[y]&amp;".to_owned(),
            },
        ]));
        assert_eq!(
            markdown,
            "`[a]<b>&lt;` `[c]` **\\<d>** <u>*e* _f_ #g</u>\n\n```\n<x>[y]&amp;\n```\n\n"
        );
    }

    #[test]
    fn link_text_and_alt_cannot_open_a_footnote() {
        // comrak takes `[^x\]: y` at the start of a line for a footnote
        // definition: it ignores the escape, and the line would be gone.
        let link = Inline::Link {
            text: "^x]: y".to_owned(),
            target: "https://ok".to_owned(),
            style: InlineStyle::default(),
        };
        let escaped = r"[\^x\]: y](https://ok)";
        assert_renders(para(vec![link]), escaped, escaped);
        let image = ParsedBlock::Image {
            alt: Some("^a^".to_owned()),
            title: None,
            src: Some("p.png".to_owned()),
        };
        assert_renders(image, r"![\^a^](p.png)", r"![\^a^](p.png)");
    }

    #[test]
    fn underlined_text_cannot_open_an_html_block() {
        // `></u>` alone on a line is a quote holding an HTML block, and in an
        // HTML block escapes are not read: `\<?x` would be raw HTML.
        let underline = InlineStyle {
            underline: true,
            ..InlineStyle::default()
        };
        assert_renders(
            para(vec![
                Inline::styled("a\n>", underline),
                Inline::plain(" \n> <?x"),
            ]),
            "<u>a\n\\></u> \n> \\<?x",
            r"<u>a<br/>\></u> <br/>> \<?x",
        );
    }

    #[test]
    fn emphasis_around_escaped_text() {
        let style = |bold: bool, italic: bool, strike: bool| InlineStyle {
            bold,
            italic,
            strike,
            ..InlineStyle::default()
        };
        assert_renders(
            para(vec![
                Inline::styled("[1]", style(true, false, false)),
                Inline::plain(" "),
                Inline::styled("<b>", style(false, true, false)),
                Inline::plain(" "),
                Inline::styled(r"C:\x\", style(true, true, true)),
            ]),
            r"**\[1]** *\<b>* ***~~C:\x\\~~***",
            r"**\[1]** *\<b>* ***~~C:\x\\~~***",
        );
    }

    #[test]
    fn an_image_without_a_source_keeps_its_title() {
        // `( "t")` would make the title the destination and the rest of it
        // Markdown.
        let image = |src: Option<&str>| ParsedBlock::Image {
            alt: Some("a".to_owned()),
            title: Some("x [y](javascript:1)".to_owned()),
            src: src.map(str::to_owned),
        };
        let escaped = r#"![a](<> "x [y](javascript:1)")"#;
        assert_renders(image(None), escaped, escaped);
        assert_renders(image(Some("")), escaped, escaped);
    }

    #[test]
    fn a_long_tilde_run_is_escaped_in_one_pass() {
        // Scanned again for each tilde, this run takes minutes and the test
        // hits the per-test timeout.
        let n = 1_000_000;
        let markdown = eager_and_streamed(vec![para(vec![Inline::plain(format!(
            "a{}b~~c",
            "~".repeat(n)
        ))])]);
        assert_eq!(markdown, format!("a{}b~~c\n\n", "\\~".repeat(n)));
    }

    #[test]
    fn line_indents_cannot_make_a_code_block() {
        let code = InlineStyle {
            code: true,
            ..InlineStyle::default()
        };
        // Four columns at the start of the paragraph or after a blank line
        // would make an indented code block. Code keeps its spaces.
        assert_renders(
            para(vec![
                Inline::plain("\t  a\n\n    b \n"),
                Inline::plain("  \tc "),
                Inline::styled("  x  y", code),
                Inline::plain("\r    d"),
            ]),
            "a\n\nb \nc   `x  y`\rd",
            "a<br/><br/>b <br/>c   `x  y`<br/>d",
        );
        let item = ParsedBlock::ListItem {
            level: 0,
            ordered: false,
            blocks: vec![para(vec![Inline::plain("    a\n\n     b")])],
        };
        assert_eq!(eager_and_streamed(vec![item.clone()]), "- a\n\n  b\n");
        assert_eq!(
            eager_and_streamed(vec![ParsedBlock::Quote { blocks: vec![item] }]),
            "> - a\n> \n>   b\n\n"
        );
        assert_eq!(
            eager_and_streamed(vec![ParsedBlock::Heading {
                level: 2,
                inlines: vec![Inline::plain("h\n     x")],
            }]),
            "## h\nx\n\n"
        );
    }

    #[test]
    fn an_ampersand_at_the_end_of_a_run_cannot_join_the_next() {
        assert_renders(
            para(vec![Inline::plain("AT&"), Inline::plain("lt;T")]),
            "AT&amp;lt;T",
            "AT&amp;lt;T",
        );
    }

    #[test]
    fn angle_brackets_are_escaped_only_where_they_could_open_html() {
        let underline = InlineStyle {
            underline: true,
            ..InlineStyle::default()
        };
        assert_renders(
            para(vec![Inline::plain(
                "a < b, x<=1 <a <1@x.y> </p> <!-- <? <( <> <",
            )]),
            r"a < b, x\<=1 \<a \<1@x.y> \</p> \<!-- \<? <( <> \<",
            r"a < b, x\<=1 \<a \<1@x.y> \</p> \<!-- \<? <( <> \<",
        );
        // A trailing `>` is escaped only before the renderer's `</u>`.
        assert_renders(
            para(vec![Inline::plain("a>"), Inline::styled("b>", underline)]),
            r"a><u>b\></u>",
            r"a><u>b\></u>",
        );
    }

    #[test]
    fn a_closing_hash_run_in_a_heading_stays() {
        let heading = |text: &str| {
            eager_and_streamed(vec![ParsedBlock::Heading {
                level: 1,
                inlines: vec![Inline::plain(text)],
            }])
        };
        assert_eq!(heading("Issue #"), "# Issue \\#\n\n");
        assert_eq!(heading("Issue ## "), "# Issue \\## \n\n");
        assert_eq!(heading("#"), "# \\#\n\n");
        assert_eq!(heading("C#"), "# C#\n\n");
        assert_eq!(heading("a #\nb #"), "# a \\#\nb #\n\n");
        let mut doc = doc_of(vec![]);
        doc.title = Some("Issue #".to_owned());
        assert_eq!(MarkdownRenderer::render(&doc), "# Issue \\#\n\n");
    }
}
