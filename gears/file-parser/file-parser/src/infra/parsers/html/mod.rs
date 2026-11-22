//! HTML documents.
//!
//! [`tokenize`] and [`dom`] build a small tree in linear time; html5ever was
//! tried first and dropped because several of its spec algorithms are
//! super-linear on hostile input (scope walks on deep nesting and unmatched
//! end tags, formatting-element reconstruction, attribute de-duplication).
//! The tree is walked once. Block elements (headings, paragraphs, lists,
//! tables, quotes, preformatted text, rules, images) become blocks; inline
//! elements become styled runs and links inside the current paragraph.
//! Scripts, styles, form controls, embedded media and hidden elements are
//! skipped. Whitespace collapses as a browser renders it, except in `<pre>`.

mod dom;
mod encoding;
mod tokenize;

use std::path::Path;

use async_trait::async_trait;
use bytes::Bytes;
use dom::{Dom, Node, NodeRef};

use crate::domain::error::DomainError;
use crate::domain::ir::{
    DocumentBuilder, Inline, InlineStyle, ParsedBlock, ParsedDocument, ParsedSource, TableBlock,
    TableCell, TableRow,
};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::{ParseLimits, TableBudget, limit_exceeded};

/// Ceiling applied by [`HtmlParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default; `Gear::init` passes the configured value.
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

const SUPPORTED_EXTENSIONS: &[&str] = &["html", "htm"];

/// Longest link target or image source kept; longer ones (inline data) drop.
const MAX_URL_LEN: usize = 2048;
/// Element nesting the converter follows. Deeper content is flattened into
/// the current paragraph (as browsers flatten over-deep trees), which keeps
/// the recursion well inside a 2 MiB thread stack.
const MAX_DEPTH: usize = 256;

/// Upper bound for `colspan` / `rowspan`, as in the HTML standard.
const MAX_COLSPAN: usize = 1000;
const MAX_ROWSPAN: usize = 65_534;

/// HTML parser.
pub struct HtmlParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl HtmlParser {
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            limits: ParseLimits::default(),
        }
    }

    /// Caps how much of a local file is read into memory.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Replaces the default [`ParseLimits`].
    #[must_use]
    pub fn with_limits(mut self, limits: ParseLimits) -> Self {
        self.limits = limits;
        self
    }

    async fn parse(
        &self,
        bytes: Bytes,
        content_type: Option<&str>,
        source: ParsedSource,
        filename: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let limits = self.limits.clone();
        let content_type = content_type.map(str::to_owned);
        let page =
            tokio::task::spawn_blocking(move || convert(&bytes, content_type.as_deref(), &limits))
                .await
                .map_err(|e| DomainError::parse_error(format!("HTML task failed: {e}")))??;

        let mut builder = DocumentBuilder::new(source)
            .content_type("text/html")
            .blocks(page.blocks);
        if let Some(title) = page.title.as_deref().or(filename) {
            builder = builder.title(title);
        }
        if let Some(language) = page.language {
            builder = builder.language(language);
        }
        if let Some(filename) = filename {
            builder = builder.original_filename(filename);
        }
        Ok(builder.build())
    }
}

impl Default for HtmlParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for HtmlParser {
    fn id(&self) -> &'static str {
        "html"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        SUPPORTED_EXTENSIONS
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        resolved_content_type: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let bytes = read_bounded(path, self.max_bytes).await?;
        let filename = path.file_name().and_then(|s| s.to_str());
        let source = ParsedSource::LocalPath(path.display().to_string());
        self.parse(Bytes::from(bytes), resolved_content_type, source, filename)
            .await
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        content_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<ParsedDocument, DomainError> {
        let source = ParsedSource::Uploaded {
            original_name: filename_hint.unwrap_or("unknown").to_owned(),
        };
        self.parse(bytes, content_type, source, filename_hint).await
    }
}

/// The converted page.
struct Page {
    title: Option<String>,
    language: Option<String>,
    blocks: Vec<ParsedBlock>,
}

fn convert(
    bytes: &[u8],
    content_type: Option<&str>,
    limits: &ParseLimits,
) -> Result<Page, DomainError> {
    let text = encoding::decode(bytes, content_type);
    let dom = Dom::parse(&text, limits.max_html_nodes)?;
    drop(text);
    let root = dom.root();
    let language = attr(root, "lang")
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.len() <= 35)
        .map(str::to_owned);
    // A title is honoured wherever it is, as browsers do.
    let title = dom.find("title").and_then(title_text);
    let mut converter = Converter::new(limits);
    let mut blocks = Vec::new();
    converter.block_children(root, &mut blocks)?;
    Ok(Page {
        title,
        language,
        blocks,
    })
}

fn element_name(node: NodeRef<'_>) -> Option<&str> {
    match node.value() {
        Node::Element(e) => Some(e.name()),
        Node::Text(_) => None,
    }
}

fn attr<'a>(node: NodeRef<'a>, name: &str) -> Option<&'a str> {
    match node.value() {
        Node::Element(e) => e.attr(name),
        Node::Text(_) => None,
    }
}

/// The text of a `<title>`, whitespace collapsed.
fn title_text(title: NodeRef<'_>) -> Option<String> {
    let mut text = String::new();
    for node in title.children() {
        if let Node::Text(t) = node.value() {
            text.push_str(t);
        }
    }
    let title = collapse(&text).trim().to_owned();
    (!title.is_empty()).then_some(title)
}

/// Elements whose content is never document text.
fn is_skipped(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "noscript"
            | "noframes"
            | "noembed"
            | "template"
            | "head"
            | "title"
            | "svg"
            | "math"
            | "iframe"
            | "object"
            | "embed"
            | "canvas"
            | "video"
            | "audio"
            | "map"
            | "select"
            | "option"
            | "button"
            | "input"
            | "textarea"
    )
}

/// Elements that start a new block.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "img"
            | "legend"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Whether the element is hidden from readers.
fn is_hidden(node: NodeRef<'_>) -> bool {
    let Node::Element(e) = node.value() else {
        return false;
    };
    if e.attr("hidden").is_some() || e.attr("aria-hidden") == Some("true") {
        return true;
    }
    e.attr("style").is_some_and(|style| {
        let compact: String = style
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        compact.contains("display:none") || compact.contains("visibility:hidden")
    })
}

/// Collapses runs of ASCII whitespace to one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_ascii_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// A link target worth keeping: relative, or with a scheme readers can
/// follow (`http`, `https`, `mailto`, `ftp`, `tel`). Browsers ignore tabs and
/// line breaks in URLs, so those are removed before the scheme is judged.
fn link_target(href: &str) -> Option<String> {
    let href: String = href
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let href = href.trim_matches(|c: char| c <= ' ');
    // A consumer trimming Unicode whitespace would see the scheme behind it.
    if href.is_empty()
        || href.len() > MAX_URL_LEN
        || href.starts_with(|c: char| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let scheme = href.split_once(':').map(|(scheme, _)| scheme).filter(|s| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    match scheme {
        None => Some(href.to_owned()),
        Some(s)
            if ["http", "https", "mailto", "ftp", "tel"]
                .iter()
                .any(|ok| s.eq_ignore_ascii_case(ok)) =>
        {
            Some(href.to_owned())
        }
        Some(_) => None,
    }
}

/// Inline runs being collected for one paragraph or heading.
#[derive(Default)]
struct Inlines {
    runs: Vec<Inline>,
}

impl Inlines {
    fn push(&mut self, text: &str, style: &InlineStyle, link: Option<&str>) {
        if text.is_empty() {
            return;
        }
        // Merge with the previous run when nothing but the text differs.
        match (self.runs.last_mut(), link) {
            (
                Some(Inline::Text {
                    text: prev,
                    style: s,
                }),
                None,
            ) if s == style => {
                prev.push_str(text);
            }
            (
                Some(Inline::Link {
                    text: prev,
                    target,
                    style: s,
                }),
                Some(l),
            ) if s == style && target == l => prev.push_str(text),
            _ => self.runs.push(match link {
                Some(target) => Inline::Link {
                    text: text.to_owned(),
                    target: target.to_owned(),
                    style: style.clone(),
                },
                None => Inline::Text {
                    text: text.to_owned(),
                    style: style.clone(),
                },
            }),
        }
    }

    /// Whether the last character pushed is a space or line break (or
    /// nothing has been pushed), so a following space is redundant.
    fn ends_with_space(&self) -> bool {
        self.runs.last().is_none_or(|run| {
            let (Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. }) =
                run;
            text.ends_with([' ', '\n'])
        })
    }

    /// The runs with leading and trailing whitespace removed; empty if
    /// nothing but whitespace was collected.
    fn take(&mut self) -> Vec<Inline> {
        let mut runs = std::mem::take(&mut self.runs);
        let mut start = 0;
        while let Some(run) = runs.get_mut(start) {
            let text = run_text(run);
            let trimmed = text.trim_start_matches([' ', '\n']).to_owned();
            if trimmed.is_empty() {
                start += 1;
            } else {
                *text = trimmed;
                break;
            }
        }
        runs.drain(..start);
        while let Some(last) = runs.last_mut() {
            let text = run_text(last);
            let trimmed = text.trim_end_matches([' ', '\n']).to_owned();
            if trimmed.is_empty() {
                runs.pop();
            } else {
                *text = trimmed;
                break;
            }
        }
        runs
    }
}

fn run_text(run: &mut Inline) -> &mut String {
    match run {
        Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => text,
    }
}

struct Converter {
    depth: usize,
    /// Above zero while collecting a heading, term or caption: block
    /// elements there add to the same line instead of starting blocks.
    inline_mode: usize,
    budget: TableBudget,
    text_left: usize,
    max_text: usize,
}

impl Converter {
    fn new(limits: &ParseLimits) -> Self {
        Self {
            depth: 0,
            inline_mode: 0,
            budget: TableBudget::new(limits),
            text_left: limits.max_text_bytes,
            max_text: limits.max_text_bytes,
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), DomainError> {
        self.text_left = self.text_left.checked_sub(bytes).ok_or_else(|| {
            limit_exceeded(
                "max_text_bytes",
                format!("document text exceeds {} bytes", self.max_text),
            )
        })?;
        Ok(())
    }

    /// Converts the children of a block element into `out`, gathering loose
    /// text and inline elements into paragraphs.
    fn block_children(
        &mut self,
        parent: NodeRef<'_>,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        self.depth += 1;
        let mut para = Inlines::default();
        for child in parent.children() {
            self.flow(child, &InlineStyle::default(), None, &mut para, out)?;
        }
        flush_paragraph(&mut para, out);
        self.depth -= 1;
        Ok(())
    }

    /// Handles one node in a flow of inline content: text and inline elements
    /// extend `para`; a block element ends it and is converted on its own.
    fn flow(
        &mut self,
        node: NodeRef<'_>,
        style: &InlineStyle,
        link: Option<&str>,
        para: &mut Inlines,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        match node.value() {
            Node::Text(t) => {
                let mut text = collapse(t);
                if text.starts_with(' ') && para.ends_with_space() {
                    text.remove(0);
                }
                self.charge(text.len())?;
                para.push(&text, style, link);
            }
            Node::Element(e) => {
                let name = e.name();
                if is_skipped(name) || is_hidden(node) {
                    return Ok(());
                }
                if self.depth >= MAX_DEPTH {
                    return self.flatten(node, style, link, para);
                }
                if name == "br" {
                    para.push("\n", style, link);
                } else if self.inline_mode > 0 && name == "img" {
                    // An image in a heading reads as its alternative text.
                    if let Some(alt) = e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
                        self.charge(alt.len())?;
                        para.push(alt, style, link);
                    }
                } else if is_block(name) && self.inline_mode == 0 {
                    flush_paragraph(para, out);
                    self.block(node, name, out)?;
                } else {
                    // Blocks in inline mode are word-separated containers.
                    let separate = is_block(name);
                    if separate && !para.ends_with_space() {
                        para.push(" ", style, link);
                    }
                    self.depth += 1;
                    let mut inner = style.clone();
                    match name {
                        "b" | "strong" => inner.bold = true,
                        "i" | "em" | "cite" | "var" | "dfn" => inner.italic = true,
                        "u" | "ins" => inner.underline = true,
                        "s" | "del" | "strike" => inner.strike = true,
                        "code" | "kbd" | "samp" | "tt" => inner.code = true,
                        _ => {}
                    }
                    let target = if name == "a" {
                        e.attr("href").and_then(link_target)
                    } else {
                        None
                    };
                    let link = target.as_deref().or(link);
                    for child in node.children() {
                        self.flow(child, &inner, link, para, out)?;
                    }
                    if separate && !para.ends_with_space() {
                        para.push(" ", style, link);
                    }
                    self.depth -= 1;
                }
            }
        }
        Ok(())
    }

    /// Appends the text of `node`'s subtree to `para` without structure,
    /// walking iteratively.
    fn flatten(
        &mut self,
        node: NodeRef<'_>,
        style: &InlineStyle,
        link: Option<&str>,
        para: &mut Inlines,
    ) -> Result<(), DomainError> {
        // `None` marks the end of a block: words on either side stay apart.
        let mut stack = vec![Some(node)];
        while let Some(item) = stack.pop() {
            let Some(current) = item else {
                if !para.ends_with_space() {
                    para.push(" ", style, link);
                }
                continue;
            };
            match current.value() {
                Node::Text(t) => {
                    let mut text = collapse(t);
                    if text.starts_with(' ') && para.ends_with_space() {
                        text.remove(0);
                    }
                    self.charge(text.len())?;
                    para.push(&text, style, link);
                }
                Node::Element(e) => {
                    let name = e.name();
                    if is_skipped(name) || is_hidden(current) {
                        continue;
                    }
                    let separate = is_block(name) || matches!(name, "br" | "td" | "th" | "tr");
                    if separate {
                        if !para.ends_with_space() {
                            para.push(" ", style, link);
                        }
                        stack.push(None);
                    }
                    let start = stack.len();
                    stack.extend(current.children().map(Some));
                    stack[start..].reverse();
                }
            }
        }
        Ok(())
    }

    /// Converts one block element.
    fn block(
        &mut self,
        node: NodeRef<'_>,
        name: &str,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name[1..].parse().unwrap_or(1);
                let inlines = self.inline_content(node, &InlineStyle::default())?;
                if !inlines.is_empty() {
                    out.push(ParsedBlock::Heading { level, inlines });
                }
            }
            "ul" | "ol" | "menu" | "dir" => self.list(node, name == "ol", 0, out)?,
            "li" => self.list_item(node, false, 0, out)?,
            "dt" => {
                let bold = InlineStyle {
                    bold: true,
                    ..InlineStyle::default()
                };
                let inlines = self.inline_content(node, &bold)?;
                if !inlines.is_empty() {
                    out.push(ParsedBlock::Paragraph { inlines });
                }
            }
            "table" => self.table(node, out)?,
            "blockquote" => {
                let mut inner = Vec::new();
                self.block_children(node, &mut inner)?;
                if !inner.is_empty() {
                    out.push(ParsedBlock::Quote { blocks: inner });
                }
            }
            "pre" => self.preformatted(node, out)?,
            "hr" => out.push(ParsedBlock::HorizontalRule),
            "img" => {
                let alt = attr(node, "alt").map(str::trim).filter(|a| !a.is_empty());
                let title = attr(node, "title").map(str::trim).filter(|t| !t.is_empty());
                let src = attr(node, "src").and_then(link_target);
                if alt.is_some() || src.is_some() {
                    self.charge(alt.map_or(0, str::len) + title.map_or(0, str::len))?;
                    out.push(ParsedBlock::Image {
                        alt: alt.map(str::to_owned),
                        title: title.map(str::to_owned),
                        src,
                    });
                }
            }
            _ => self.block_children(node, out)?,
        }
        Ok(())
    }

    /// The inline content of an element as runs, block children flattened
    /// into the same line.
    fn inline_content(
        &mut self,
        node: NodeRef<'_>,
        style: &InlineStyle,
    ) -> Result<Vec<Inline>, DomainError> {
        self.depth += 1;
        self.inline_mode += 1;
        let mut para = Inlines::default();
        // Nothing reaches `unused`: in inline mode every element flows into
        // `para`, in document order.
        let mut unused = Vec::new();
        let result = node
            .children()
            .try_for_each(|child| self.flow(child, style, None, &mut para, &mut unused));
        self.inline_mode -= 1;
        self.depth -= 1;
        result?;
        Ok(para.take())
    }

    /// Past [`MAX_DEPTH`], the text of `node` as one paragraph.
    fn flat_paragraph(
        &mut self,
        node: NodeRef<'_>,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        let mut para = Inlines::default();
        self.flatten(node, &InlineStyle::default(), None, &mut para)?;
        flush_paragraph(&mut para, out);
        Ok(())
    }

    /// A `ul` / `ol`: one list item per `li`, nested lists one level deeper.
    fn list(
        &mut self,
        node: NodeRef<'_>,
        ordered: bool,
        level: u8,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        if self.depth >= MAX_DEPTH {
            return self.flat_paragraph(node, out);
        }
        self.depth += 1;
        // Loose text directly in a list reads as an item of its own.
        let mut stray = Inlines::default();
        for child in node.children() {
            match element_name(child) {
                Some(name) if is_skipped(name) || is_hidden(child) => {}
                Some("li") => {
                    flush_item(&mut stray, ordered, level, out);
                    self.list_item(child, ordered, level, out)?;
                }
                // A list directly inside a list (legacy markup) nests.
                Some(name @ ("ul" | "ol" | "menu" | "dir")) => {
                    flush_item(&mut stray, ordered, level, out);
                    self.list(child, name == "ol", level.saturating_add(1), out)?;
                }
                Some(name) if is_block(name) => {
                    flush_item(&mut stray, ordered, level, out);
                    self.block(child, name, out)?;
                }
                _ => self.flow(child, &InlineStyle::default(), None, &mut stray, out)?,
            }
        }
        flush_item(&mut stray, ordered, level, out);
        self.depth -= 1;
        Ok(())
    }

    /// One `li`: its text and blocks form the item; a nested list follows as
    /// items one level deeper, and text after it continues the item.
    fn list_item(
        &mut self,
        node: NodeRef<'_>,
        ordered: bool,
        level: u8,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        if self.depth >= MAX_DEPTH {
            return self.flat_paragraph(node, out);
        }
        self.depth += 1;
        let mut para = Inlines::default();
        let mut blocks = Vec::new();
        for child in node.children() {
            match element_name(child) {
                Some(name @ ("ul" | "ol" | "menu" | "dir")) if !is_hidden(child) => {
                    flush_paragraph(&mut para, &mut blocks);
                    if !blocks.is_empty() {
                        out.push(ParsedBlock::ListItem {
                            level,
                            ordered,
                            blocks: std::mem::take(&mut blocks),
                        });
                    }
                    self.list(child, name == "ol", level.saturating_add(1), out)?;
                }
                _ => self.flow(child, &InlineStyle::default(), None, &mut para, &mut blocks)?,
            }
        }
        flush_paragraph(&mut para, &mut blocks);
        if !blocks.is_empty() {
            out.push(ParsedBlock::ListItem {
                level,
                ordered,
                blocks,
            });
        }
        self.depth -= 1;
        Ok(())
    }

    /// `<pre>`: text kept as written; the language comes from a
    /// `language-x` / `lang-x` class on the `pre` or its `code`.
    fn preformatted(
        &mut self,
        node: NodeRef<'_>,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        let mut code = String::new();
        let mut stack = vec![node];
        let mut language = code_language(node);
        // Iterative, so deep markup inside <pre> cannot exhaust the stack.
        while let Some(current) = stack.pop() {
            match current.value() {
                Node::Text(t) => {
                    self.charge(t.len())?;
                    code.push_str(t);
                }
                Node::Element(e) if current != node => {
                    if is_skipped(e.name()) {
                        continue;
                    }
                    if e.name() == "br" {
                        code.push('\n');
                        continue;
                    }
                    if language.is_none() && e.name() == "code" {
                        language = code_language(current);
                    }
                    push_children_reversed(current, &mut stack);
                }
                Node::Element(_) => push_children_reversed(current, &mut stack),
            }
        }
        // The newline right after <pre> is not content.
        let code = code
            .strip_prefix('\n')
            .unwrap_or(&code)
            .trim_end()
            .to_owned();
        if !code.is_empty() {
            out.push(ParsedBlock::CodeBlock { language, code });
        }
        Ok(())
    }

    /// A table: rows from `thead` / `tbody` / `tfoot` / direct `tr`, found
    /// also through wrappers legacy markup puts around them (`form`, `font`),
    /// and cells from `th` / `td` with `colspan` and `rowspan` laid out on a
    /// grid. Text outside any cell comes out before the table, where browsers
    /// put it.
    fn table(&mut self, node: NodeRef<'_>, out: &mut Vec<ParsedBlock>) -> Result<(), DomainError> {
        self.depth += 1;
        let (rows, mut outside) = table_parts(node);

        let mut grid: Vec<TableRow> = Vec::new();
        // Rows still covered by a rowspan from above, per column.
        let mut covered: Vec<usize> = Vec::new();
        let mut width: usize = 0;
        let mut rows_seen: usize = 0;
        let mut built: usize = 0;
        let remaining = self.budget.remaining();
        let max_columns = self.budget.max_columns();
        // Every row, empty or not, costs the table's width: checking rows x
        // width bounds the grid, the span fill and the per-row span updates.
        let over = |rows: usize, width: usize| {
            width > max_columns || rows.saturating_mul(width) > remaining
        };
        let mut skipped = false;
        'rows: for (in_head, row) in rows {
            rows_seen += 1;
            if over(rows_seen, width.max(covered.len())) {
                skipped = true;
                break;
            }
            let cells_of_row = row_cells(row, &mut outside);
            let mut cells: Vec<TableCell> = Vec::new();
            let mut all_th = true;
            let mut col = 0;
            for cell in &cells_of_row {
                all_th &= element_name(*cell) == Some("th");
                // Skip columns a rowspan from above still covers.
                while covered.get(col).is_some_and(|&n| n > 0) {
                    cells.push(empty_cell());
                    col += 1;
                }
                let span = |a: &str, max: usize| {
                    attr(*cell, a)
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, max)
                };
                let colspan = span("colspan", MAX_COLSPAN);
                let rowspan = span("rowspan", MAX_ROWSPAN);
                if over(rows_seen, (col + colspan).max(width)) {
                    built += cells.len();
                    skipped = true;
                    break 'rows;
                }
                let mut blocks = Vec::new();
                self.block_children(*cell, &mut blocks)?;
                cells.push(TableCell {
                    blocks: if blocks.is_empty() {
                        vec![ParsedBlock::Paragraph {
                            inlines: vec![Inline::plain("")],
                        }]
                    } else {
                        blocks
                    },
                });
                for _ in 1..colspan {
                    cells.push(empty_cell());
                }
                for c in col..col + colspan {
                    if covered.len() <= c {
                        covered.resize(c + 1, 0);
                    }
                    covered[c] = covered[c].max(rowspan);
                }
                col += colspan;
            }
            width = width.max(cells.len()).max(covered.len());
            built += cells.len();
            // An empty row still uses up one row of every rowspan.
            for n in &mut covered {
                *n = n.saturating_sub(1);
            }
            if !cells_of_row.is_empty() {
                grid.push(TableRow {
                    is_header: in_head || all_th,
                    cells,
                });
            }
        }

        // Content found outside the cells precedes the table.
        let mut para = Inlines::default();
        for node in outside {
            match element_name(node) {
                Some("caption") => {
                    flush_paragraph(&mut para, out);
                    let inlines = self.inline_content(node, &InlineStyle::default())?;
                    if !inlines.is_empty() {
                        out.push(ParsedBlock::Paragraph { inlines });
                    }
                }
                _ => self.flow(node, &InlineStyle::default(), None, &mut para, out)?,
            }
        }
        flush_paragraph(&mut para, out);
        self.depth -= 1;

        let columns = grid.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if skipped || (!grid.is_empty() && !self.budget.try_charge(grid.len(), columns)) {
            // What was built counts, so a run of oversized tables cannot each
            // build up to the whole budget.
            self.budget.spend(built);
            out.push(skipped_table_note(remaining));
            return Ok(());
        }
        if !grid.is_empty() {
            out.push(ParsedBlock::Table(TableBlock { rows: grid }));
        }
        Ok(())
    }
}

/// The rows of a table (with whether they are in `thead`) and the content
/// outside any row, found by an iterative walk in document order that looks
/// through wrapper elements.
fn table_parts(node: NodeRef<'_>) -> (Vec<(bool, NodeRef<'_>)>, Vec<NodeRef<'_>>) {
    let mut rows = Vec::new();
    let mut outside = Vec::new();
    let mut stack: Vec<(NodeRef<'_>, bool)> = Vec::new();
    push_reversed(node, false, &mut stack);
    while let Some((child, in_head)) = stack.pop() {
        match child.value() {
            Node::Text(t) => {
                if !t.bytes().all(|b| b.is_ascii_whitespace()) {
                    outside.push(child);
                }
            }
            Node::Element(e) => match e.name() {
                name if is_skipped(name) || is_hidden(child) => {}
                "colgroup" | "col" => {}
                "caption" | "table" => outside.push(child),
                section @ ("thead" | "tbody" | "tfoot") => {
                    push_reversed(child, section == "thead", &mut stack);
                }
                "tr" | "td" | "th" => rows.push((in_head, child)),
                _ => push_reversed(child, in_head, &mut stack),
            },
        }
    }
    (rows, outside)
}

/// Pushes `node`'s children onto `stack` so they pop in document order.
fn push_reversed<'a>(node: NodeRef<'a>, in_head: bool, stack: &mut Vec<(NodeRef<'a>, bool)>) {
    let start = stack.len();
    stack.extend(node.children().map(|c| (c, in_head)));
    stack[start..].reverse();
}

/// The cells of a row, found also through wrapper elements; text in the row
/// outside any cell goes to `outside`. A bare cell is a row of its own.
fn row_cells<'a>(row: NodeRef<'a>, outside: &mut Vec<NodeRef<'a>>) -> Vec<NodeRef<'a>> {
    if matches!(element_name(row), Some("td" | "th")) {
        return vec![row];
    }
    let mut cells = Vec::new();
    let mut stack: Vec<NodeRef<'a>> = Vec::new();
    let start = stack.len();
    stack.extend(row.children());
    stack[start..].reverse();
    while let Some(node) = stack.pop() {
        match node.value() {
            Node::Text(t) => {
                if !t.bytes().all(|b| b.is_ascii_whitespace()) {
                    outside.push(node);
                }
            }
            Node::Element(e) => match e.name() {
                "td" | "th" => cells.push(node),
                name if is_skipped(name) || is_hidden(node) => {}
                "tr" | "table" => {}
                _ => {
                    let start = stack.len();
                    stack.extend(node.children());
                    stack[start..].reverse();
                }
            },
        }
    }
    cells
}

fn empty_cell() -> TableCell {
    TableCell {
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("")],
        }],
    }
}

fn skipped_table_note(remaining: usize) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(format!(
            "[Table skipped: it does not fit the remaining limit of {remaining} table cells]"
        ))],
    }
}

fn push_children_reversed<'a>(node: NodeRef<'a>, stack: &mut Vec<NodeRef<'a>>) {
    let start = stack.len();
    stack.extend(node.children());
    stack[start..].reverse();
}

fn code_language(node: NodeRef<'_>) -> Option<String> {
    attr(node, "class")?
        .split_ascii_whitespace()
        .find_map(|class| {
            class
                .strip_prefix("language-")
                .or_else(|| class.strip_prefix("lang-"))
                .filter(|l| !l.is_empty() && l.len() <= 32)
                .map(str::to_owned)
        })
}

fn flush_item(para: &mut Inlines, ordered: bool, level: u8, out: &mut Vec<ParsedBlock>) {
    let inlines = para.take();
    if !inlines.is_empty() {
        out.push(ParsedBlock::ListItem {
            level,
            ordered,
            blocks: vec![ParsedBlock::Paragraph { inlines }],
        });
    }
}

fn flush_paragraph(para: &mut Inlines, out: &mut Vec<ParsedBlock>) {
    let inlines = para.take();
    if !inlines.is_empty() {
        out.push(ParsedBlock::Paragraph { inlines });
    }
}

#[cfg(test)]
mod tests;
