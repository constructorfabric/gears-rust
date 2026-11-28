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

use std::rc::Rc;

use bytes::Bytes;
use dom::{Dom, Node, NodeRef};

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, InlineStyle, ParsedBlock, TableBlock, TableCell, TableRow};
use crate::domain::mime_table;

use super::backend::{DocumentFormat, DocumentParser, ReadDocument, run_blocking};
use super::inlines::{Inlines, link_target};
use super::limits::{
    ParseLimits, TableBudget, TextBudget, finish_title, push_title_text, table_note,
};

/// Element nesting the converter follows. Deeper content is flattened into
/// the current paragraph (as browsers flatten over-deep trees), which keeps
/// the recursion well inside a 2 MiB thread stack.
const MAX_DEPTH: usize = 256;

/// Upper bound for `colspan` / `rowspan`, as in the HTML standard.
const MAX_COLSPAN: usize = 1000;
const MAX_ROWSPAN: usize = 65_534;

/// HTML: the charset comes from the content, else the caller's content type.
pub struct Html;

/// HTML parser.
pub type HtmlParser = DocumentParser<Html>;

impl DocumentFormat for Html {
    const ID: &'static str = "html";
    const EXTENSIONS: &'static [&'static str] = &["html", "htm"];

    async fn read(
        bytes: Bytes,
        content_type: Option<String>,
        limits: ParseLimits,
    ) -> Result<ReadDocument, DomainError> {
        let page = run_blocking("HTML", move || {
            convert(&bytes, content_type.as_deref(), &limits)
        })
        .await?;
        Ok(ReadDocument {
            language: page.language,
            ..ReadDocument::new(mime_table::HTML, page.title, page.blocks)
        })
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
    let title = dom.title().and_then(title_text);
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
            push_title_text(&mut text, t);
        }
    }
    finish_title(&text)
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

struct Converter {
    depth: usize,
    /// Above zero while collecting a heading, term or caption: block
    /// elements there add to the same line instead of starting blocks.
    inline_mode: usize,
    budget: TableBudget,
    /// Cells held by enclosing tables still being built: they are charged
    /// only when their table ends, so a nested table must leave room for
    /// them.
    building: usize,
    text: TextBudget,
    /// What blocks inside inline elements inherit.
    inherited: Inherited,
}

/// The style and link of the inline elements around a block, which its
/// text keeps (`<a href="/p"><h3>Title</h3></a>` is a linked heading).
#[derive(Clone, Default)]
struct Inherited {
    style: InlineStyle,
    link: Option<Rc<str>>,
}

impl Converter {
    fn new(limits: &ParseLimits) -> Self {
        Self {
            depth: 0,
            inline_mode: 0,
            budget: TableBudget::new(limits),
            building: 0,
            text: TextBudget::new(limits),
            inherited: Inherited::default(),
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), DomainError> {
        self.text.charge(bytes)
    }

    /// Puts out a block, charging it to the text budget.
    fn emit(&mut self, out: &mut Vec<ParsedBlock>, block: ParsedBlock) -> Result<(), DomainError> {
        self.text.charge_block()?;
        out.push(block);
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
        let Inherited { style, link } = self.inherited.clone();
        for child in parent.children() {
            self.flow(child, &style, link.as_ref(), &mut para, out)?;
        }
        flush_paragraph(&mut para, out, &mut self.text)?;
        self.depth -= 1;
        Ok(())
    }

    /// Handles one node in a flow of inline content: text and inline elements
    /// extend `para`; a block element ends it and is converted on its own.
    fn flow(
        &mut self,
        node: NodeRef<'_>,
        style: &InlineStyle,
        link: Option<&Rc<str>>,
        para: &mut Inlines,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        let link_str = link.map(AsRef::as_ref);
        match node.value() {
            Node::Text(t) => {
                let mut text = collapse(t);
                if text.starts_with(' ') && para.ends_with_space() {
                    text.remove(0);
                }
                para.push(&text, style, link_str, &mut self.text)?;
            }
            Node::Element(e) => {
                let name = e.name();
                if is_skipped(name) || is_hidden(node) {
                    return Ok(());
                }
                if self.depth >= MAX_DEPTH {
                    return self.flatten(node, style, link_str, para);
                }
                if name == "br" {
                    para.push("\n", style, link_str, &mut self.text)?;
                } else if self.inline_mode > 0 && name == "img" {
                    // An image in a heading reads as its alternative text.
                    if let Some(alt) = e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
                        para.push(alt, style, link_str, &mut self.text)?;
                    }
                } else if is_block(name) && self.inline_mode == 0 {
                    flush_paragraph(para, out, &mut self.text)?;
                    let around = Inherited {
                        style: style.clone(),
                        link: link.cloned(),
                    };
                    let saved = std::mem::replace(&mut self.inherited, around);
                    let converted = self.block(node, name, out);
                    self.inherited = saved;
                    converted?;
                } else {
                    // Blocks in inline mode are word-separated containers.
                    let separate = is_block(name);
                    if separate && !para.ends_with_space() {
                        para.push(" ", style, link_str, &mut self.text)?;
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
                    let target: Option<Rc<str>> = if name == "a" {
                        e.attr("href").and_then(link_target).map(Rc::from)
                    } else {
                        None
                    };
                    let link = target.as_ref().or(link);
                    for child in node.children() {
                        self.flow(child, &inner, link, para, out)?;
                    }
                    if separate && !para.ends_with_space() {
                        para.push(" ", style, link.map(AsRef::as_ref), &mut self.text)?;
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
                    para.push(" ", style, link, &mut self.text)?;
                }
                continue;
            };
            match current.value() {
                Node::Text(t) => {
                    let mut text = collapse(t);
                    if text.starts_with(' ') && para.ends_with_space() {
                        text.remove(0);
                    }
                    para.push(&text, style, link, &mut self.text)?;
                }
                Node::Element(e) => {
                    let name = e.name();
                    if is_skipped(name) || is_hidden(current) {
                        continue;
                    }
                    let separate = is_block(name) || matches!(name, "br" | "td" | "th" | "tr");
                    if separate {
                        if !para.ends_with_space() {
                            para.push(" ", style, link, &mut self.text)?;
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
                let style = self.inherited.style.clone();
                let inlines = self.inline_content(node, &style)?;
                if !inlines.is_empty() {
                    self.emit(out, ParsedBlock::Heading { level, inlines })?;
                }
            }
            "ul" | "ol" | "menu" | "dir" => self.list(node, name == "ol", 0, out)?,
            "li" => self.list_item(node, false, 0, out)?,
            "dt" => {
                let bold = InlineStyle {
                    bold: true,
                    ..self.inherited.style.clone()
                };
                let inlines = self.inline_content(node, &bold)?;
                if !inlines.is_empty() {
                    self.emit(out, ParsedBlock::Paragraph { inlines })?;
                }
            }
            "table" => self.table(node, out)?,
            "blockquote" => {
                let mut inner = Vec::new();
                self.block_children(node, &mut inner)?;
                if !inner.is_empty() {
                    self.emit(out, ParsedBlock::Quote { blocks: inner })?;
                }
            }
            "pre" => self.preformatted(node, out)?,
            "hr" => self.emit(out, ParsedBlock::HorizontalRule)?,
            "img" => {
                let alt = attr(node, "alt").map(str::trim).filter(|a| !a.is_empty());
                let title = attr(node, "title").map(str::trim).filter(|t| !t.is_empty());
                let src = attr(node, "src").and_then(link_target);
                if alt.is_some() || src.is_some() {
                    self.charge(
                        alt.map_or(0, str::len)
                            + title.map_or(0, str::len)
                            + src.as_ref().map_or(0, String::len),
                    )?;
                    self.emit(
                        out,
                        ParsedBlock::Image {
                            alt: alt.map(str::to_owned),
                            title: title.map(str::to_owned),
                            src,
                        },
                    )?;
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
        let link = self.inherited.link.clone();
        let result = node
            .children()
            .try_for_each(|child| self.flow(child, style, link.as_ref(), &mut para, &mut unused));
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
        let Inherited { style, link } = self.inherited.clone();
        self.flatten(node, &style, link.as_deref(), &mut para)?;
        flush_paragraph(&mut para, out, &mut self.text)?;
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
        let Inherited { style, link } = self.inherited.clone();
        for child in node.children() {
            match element_name(child) {
                Some(name) if is_skipped(name) || is_hidden(child) => {}
                Some("li") => {
                    flush_item(&mut stray, ordered, level, out, &mut self.text)?;
                    self.list_item(child, ordered, level, out)?;
                }
                // A list directly inside a list (legacy markup) nests.
                Some(name @ ("ul" | "ol" | "menu" | "dir")) => {
                    flush_item(&mut stray, ordered, level, out, &mut self.text)?;
                    self.list(child, name == "ol", level.saturating_add(1), out)?;
                }
                Some(name) if is_block(name) => {
                    flush_item(&mut stray, ordered, level, out, &mut self.text)?;
                    self.block(child, name, out)?;
                }
                _ => self.flow(child, &style, link.as_ref(), &mut stray, out)?,
            }
        }
        flush_item(&mut stray, ordered, level, out, &mut self.text)?;
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
        let Inherited { style, link } = self.inherited.clone();
        for child in node.children() {
            match element_name(child) {
                Some(name @ ("ul" | "ol" | "menu" | "dir")) if !is_hidden(child) => {
                    flush_paragraph(&mut para, &mut blocks, &mut self.text)?;
                    if !blocks.is_empty() {
                        self.emit(
                            out,
                            ParsedBlock::ListItem {
                                level,
                                ordered,
                                blocks: std::mem::take(&mut blocks),
                            },
                        )?;
                    }
                    self.list(child, name == "ol", level.saturating_add(1), out)?;
                }
                _ => self.flow(child, &style, link.as_ref(), &mut para, &mut blocks)?,
            }
        }
        flush_paragraph(&mut para, &mut blocks, &mut self.text)?;
        if !blocks.is_empty() {
            self.emit(
                out,
                ParsedBlock::ListItem {
                    level,
                    ordered,
                    blocks,
                },
            )?;
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
                    if is_skipped(e.name()) || is_hidden(current) {
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
            self.emit(out, ParsedBlock::CodeBlock { language, code })?;
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
        // Cells do not inherit the style or link around the table.
        let around = std::mem::take(&mut self.inherited);

        let mut grid: Vec<TableRow> = Vec::new();
        // Rows still covered by a rowspan from above, per column.
        let mut covered: Vec<usize> = Vec::new();
        let mut width: usize = 0;
        let mut rows_seen: usize = 0;
        let mut built: usize = 0;
        let mut remaining = self.budget.remaining().saturating_sub(self.building);
        let max_columns = self.budget.max_columns();
        // Every row, empty or not, costs the table's width: checking rows x
        // width bounds the grid, the span fill and the per-row span updates.
        let over = |rows: usize, width: usize, remaining: usize| {
            width > max_columns || rows.saturating_mul(width) > remaining
        };
        let mut skipped = false;
        'rows: for (in_head, row) in rows {
            rows_seen += 1;
            if over(rows_seen, width.max(covered.len()), remaining) {
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
                if over(rows_seen, (col + colspan).max(width), remaining) {
                    built += cells.len();
                    skipped = true;
                    break 'rows;
                }
                let mut blocks = Vec::new();
                // Hold this table's cells so far, this cell and its span.
                let held = built + cells.len() + colspan;
                self.building += held;
                let converted = self.block_children(*cell, &mut blocks);
                self.building -= held;
                converted?;
                // A nested table may have spent cells.
                remaining = self.budget.remaining().saturating_sub(self.building);
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

        // Content found outside the cells precedes the table. A table there
        // must leave room for the cells built above, charged only at `admit`.
        self.inherited = around;
        self.building += built;
        let converted = self.outside(outside, out);
        self.building -= built;
        converted?;
        self.depth -= 1;

        let columns = grid.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if (skipped || !grid.is_empty())
            && !self
                .budget
                .admit("HTML", grid.len(), columns, built, skipped)
        {
            out.push(table_note(self.budget.remaining(), &mut self.text)?);
            return Ok(());
        }
        if !grid.is_empty() {
            self.emit(out, ParsedBlock::Table(TableBlock { rows: grid }))?;
        }
        Ok(())
    }

    /// Converts the content of a table found outside its cells.
    fn outside(
        &mut self,
        nodes: Vec<NodeRef<'_>>,
        out: &mut Vec<ParsedBlock>,
    ) -> Result<(), DomainError> {
        let mut para = Inlines::default();
        let Inherited { style, link } = self.inherited.clone();
        for node in nodes {
            match element_name(node) {
                Some("caption") => {
                    flush_paragraph(&mut para, out, &mut self.text)?;
                    let around = std::mem::take(&mut self.inherited);
                    let inlines = self.inline_content(node, &InlineStyle::default());
                    self.inherited = around;
                    let inlines = inlines?;
                    if !inlines.is_empty() {
                        self.emit(out, ParsedBlock::Paragraph { inlines })?;
                    }
                }
                _ => self.flow(node, &style, link.as_ref(), &mut para, out)?,
            }
        }
        flush_paragraph(&mut para, out, &mut self.text)
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
                // Only the columns of a `colgroup` are skipped: rows left
                // in an unclosed one are still rows.
                "col" => {}
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

/// The cells of a row, found also through wrapper elements; text and tables
/// in the row outside any cell go to `outside`, which comes out before the
/// table, as browsers show it. A bare cell is a row of its own.
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
                name if is_skipped(name) || is_hidden(node) => {}
                "td" | "th" => cells.push(node),
                "tr" | "table" => outside.push(node),
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

fn flush_item(
    para: &mut Inlines,
    ordered: bool,
    level: u8,
    out: &mut Vec<ParsedBlock>,
    text: &mut TextBudget,
) -> Result<(), DomainError> {
    let inlines = para.take();
    if !inlines.is_empty() {
        text.charge_block()?;
        out.push(ParsedBlock::ListItem {
            level,
            ordered,
            blocks: vec![ParsedBlock::Paragraph { inlines }],
        });
    }
    Ok(())
}

fn flush_paragraph(
    para: &mut Inlines,
    out: &mut Vec<ParsedBlock>,
    text: &mut TextBudget,
) -> Result<(), DomainError> {
    let inlines = para.take();
    if !inlines.is_empty() {
        text.charge_block()?;
        out.push(ParsedBlock::Paragraph { inlines });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
