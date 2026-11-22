//! The body of `word/document.xml`, read as a flat stream of XML events.
//!
//! Explicit stacks stand in for recursion: output sinks (the document, a
//! table cell, a text box), paragraphs suspended by a text box, tables,
//! fields and hyperlinks. Each is bounded, and the work is linear in the
//! part.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, InlineStyle, ParsedBlock, TableBlock, TableCell, TableRow};
use crate::infra::parsers::inlines::{Inlines, link_target};
use crate::infra::parsers::limits::{Budgets, ParseLimits, table_note};
use crate::infra::parsers::ooxml::{AltAction, Alternates, Events, Relationships, xml};

use super::styles::{ListKind, Numbering, Role, Styles, Toggles, level_attr, on_off};

/// Deepest list level (`w:ilvl` 8).
const MAX_LEVEL: u8 = 8;
/// Nested tables kept as tables at most; deeper ones are read as text.
const MAX_TABLE_DEPTH: usize = 32;
/// Nested text boxes kept apart at most; deeper ones join their host.
const MAX_BOX_DEPTH: usize = 8;
/// Nested VML shapes tracked at most; deeper ones are transparent.
const MAX_VML_DEPTH: usize = 32;
/// Fields tracked at most; deeper ones are transparent.
pub(super) const MAX_FIELDS: usize = 64;
/// Longest field instruction kept.
const MAX_INSTRUCTION: usize = 2048;
/// Widest cell span, as for HTML `colspan`.
const MAX_SPAN: usize = 1000;

/// What the body is resolved against.
pub(super) struct DocContext<'a> {
    pub(super) styles: &'a Styles,
    pub(super) numbering: &'a Numbering,
    pub(super) rels: &'a Relationships,
    pub(super) limits: &'a ParseLimits,
}

/// Reads the blocks of the document body.
pub(super) fn read_body(
    text: &str,
    ctx: &DocContext<'_>,
    budgets: &mut Budgets,
) -> Result<Vec<ParsedBlock>, DomainError> {
    let mut walker = Walker::new(ctx, budgets);
    let mut events = Events::new(text, ctx.limits, "DOCX");
    let mut root_seen = false;
    loop {
        let event = events.next()?;
        if !root_seen && let Event::Start(e) | Event::Empty(e) = &event {
            root_seen = true;
            if xml::qname(e) != "w:document" {
                // Elements are matched by the conventional `w:` prefix.
                tracing::warn!(
                    "DOCX: the document part does not use the `w:` prefix; its text may be missed"
                );
            }
        }
        if matches!(event, Event::Eof) {
            break;
        }
        walker.event(event)?;
    }
    walker.finish_para()?;
    Ok(walker.document)
}

/// Which property element is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Props {
    None,
    Paragraph,
    /// Paragraph borders.
    Borders,
    /// A section break in a paragraph's properties.
    Section,
    /// The paragraph mark's run properties.
    Mark,
    Run,
    Row,
    Cell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    None,
    /// `w:t` or equation text.
    Shown,
    /// A field instruction.
    Instruction,
}

/// A paragraph being read.
#[derive(Default)]
#[expect(clippy::struct_excessive_bools, reason = "independent paragraph flags")]
struct Para {
    style: Option<String>,
    outline: Option<u8>,
    num_id: Option<u32>,
    ilvl: Option<u8>,
    page_break_before: Option<bool>,
    section_break: bool,
    /// top, bottom, left, right
    borders: [bool; 4],
    inlines: Inlines,
    /// Something of this paragraph was already put out.
    emitted: bool,
    /// Text boxes anchored in this paragraph, put out after it.
    pending: Vec<ParsedBlock>,
    /// The paragraph mark is a tracked deletion: the next paragraph goes on
    /// from here.
    mark_deleted: bool,
}

/// The run (`w:r`) being read.
#[derive(Default)]
struct Run {
    style_id: Option<String>,
    direct: Toggles,
    style: InlineStyle,
    hidden: bool,
}

/// A field (`w:fldChar` / `w:fldSimple`).
struct Field {
    instruction: String,
    /// Past `separate`: its result is shown.
    showing: bool,
    link: Option<String>,
    /// A `w:fldSimple`, closed by its end tag rather than `fldChar end`.
    simple: bool,
}

/// A table being read.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent per-table and per-cell state"
)]
struct Table {
    rows: Vec<Vec<TableCell>>,
    cells: usize,
    overflow: bool,
    header_rows: usize,
    row_header: bool,
    row_after: usize,
    span: usize,
    continuation: bool,
    /// The row or cell is a tracked deletion.
    row_deleted: bool,
    cell_deleted: bool,
}

/// A picture (`wp:docPr`) being described.
#[derive(Default)]
struct Picture {
    alt: Option<String>,
    title: Option<String>,
    hidden: bool,
    decorative: bool,
}

/// A legacy VML shape (`v:shape`, `v:rect`, ...) being read: a picture once
/// it ends.
#[derive(Default)]
struct VmlShape {
    alt: Option<String>,
    title: Option<String>,
    /// It holds a text box: its text is read, it is no picture.
    text_box: bool,
}

/// What a text box suspends: the host paragraph, its open run, and the
/// count of transparent nested paragraphs.
struct Suspended {
    para: Option<Para>,
    run: Option<Run>,
    nested_p: usize,
}

struct Walker<'a, 'b> {
    ctx: &'a DocContext<'a>,
    budgets: &'b mut Budgets,
    /// Depth inside a subtree being skipped; 0 when reading.
    skip: usize,
    alternates: Alternates,
    /// The document's blocks.
    document: Vec<ParsedBlock>,
    /// Where blocks go instead while a table cell or text box is open.
    sinks: Vec<Vec<ParsedBlock>>,
    para: Option<Para>,
    /// The open paragraph ended with a deleted mark; the next `w:p` joins it.
    joining: bool,
    /// `w:p` inside an open paragraph (not valid markup): read as part of it.
    nested_p: usize,
    suspended: Vec<Suspended>,
    flat_boxes: usize,
    tables: Vec<Table>,
    flat_tables: usize,
    fields: Vec<Field>,
    field_overflow: usize,
    links: Vec<Option<String>>,
    link_cache: HashMap<String, Option<String>>,
    run: Option<Run>,
    text: TextKind,
    props: Props,
    picture: Option<Picture>,
    /// Open VML shapes, innermost last (a text box can hold another).
    vml: Vec<VmlShape>,
    /// Open VML shapes past [`MAX_VML_DEPTH`].
    vml_overflow: usize,
    /// Depth inside `w:sdtPr`.
    sdt_props: usize,
    /// One entry per open `w:sdt`: whether it shows placeholder text.
    sdts: Vec<bool>,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(ctx: &'a DocContext<'a>, budgets: &'b mut Budgets) -> Self {
        Self {
            ctx,
            budgets,
            skip: 0,
            alternates: Alternates::default(),
            document: Vec::new(),
            sinks: Vec::new(),
            para: None,
            joining: false,
            nested_p: 0,
            suspended: Vec::new(),
            flat_boxes: 0,
            tables: Vec::new(),
            flat_tables: 0,
            fields: Vec::new(),
            field_overflow: 0,
            links: Vec::new(),
            link_cache: HashMap::new(),
            run: None,
            text: TextKind::None,
            props: Props::None,
            picture: None,
            vml: Vec::new(),
            vml_overflow: 0,
            sdt_props: 0,
            sdts: Vec::new(),
        }
    }

    fn event(&mut self, event: Event<'_>) -> Result<(), DomainError> {
        match event {
            Event::Start(e) => self.start(&e, true),
            Event::Empty(e) => self.start(&e, false),
            Event::End(e) => self.end(e.name().into_inner()),
            Event::Text(t) => self.text(&t.xml10_content()),
            Event::GeneralRef(r) => match xml::resolve_ref(&r) {
                Some(text) => self.text(&text),
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }

    fn start(&mut self, e: &BytesStart<'_>, is_start: bool) -> Result<(), DomainError> {
        if self.skip > 0 {
            self.skip += usize::from(is_start);
            return Ok(());
        }
        let name = xml::qname(e);
        if self.sdt_props > 0 {
            self.sdt_props += usize::from(is_start);
            if name == "w:showingPlcHdr"
                && on_off(e)
                && let Some(placeholder) = self.sdts.last_mut()
            {
                *placeholder = true;
            }
            return Ok(());
        }
        if self.alternate(name, is_start) {
            return Ok(());
        }
        // Tracked deletions of rows and cells (in their properties).
        if let Some(table) = self.tables.last_mut().filter(|_| self.flat_tables == 0) {
            match (self.props, name) {
                (Props::Row, "w:del") => table.row_deleted = true,
                (Props::Cell, "w:cellDel") => table.cell_deleted = true,
                _ => {}
            }
        }
        // A deleted paragraph mark (in its run properties).
        if self.props == Props::Mark
            && matches!(name, "w:del" | "w:moveFrom")
            && let Some(para) = self.para.as_mut()
        {
            para.mark_deleted = true;
        }
        if skipped(name) {
            self.skip = usize::from(is_start);
            return Ok(());
        }
        if self.structure(name, is_start)? {
            return Ok(());
        }
        self.content(e, name, is_start)
    }

    /// `w:sectPr`: in a paragraph's properties a section ends there (without
    /// a type it is nextPage); the body's last section puts out nothing.
    fn section(&mut self, is_start: bool) {
        if self.props == Props::Paragraph {
            if let Some(p) = self.para.as_mut() {
                p.section_break = true;
            }
            if is_start {
                self.props = Props::Section;
            }
        } else {
            self.skip = usize::from(is_start);
        }
    }

    /// Paragraphs, properties, content controls, text boxes and tables;
    /// returns whether `name` was one of them.
    fn structure(&mut self, name: &str, is_start: bool) -> Result<bool, DomainError> {
        match name {
            "w:sdt" if is_start => self.sdts.push(false),
            "w:sdtPr" if is_start => self.sdt_props = 1,
            "w:sdtContent" if is_start => {
                if self.sdts.last().copied().unwrap_or(false) {
                    self.skip = 1;
                }
            }
            "w:p" if is_start => self.para_start(),
            "w:pPr" if is_start && self.para.is_some() => self.props = Props::Paragraph,
            "w:sectPr" => self.section(is_start),
            "w:pBdr" if is_start && self.props == Props::Paragraph => self.props = Props::Borders,
            "w:rPr" if is_start && self.props == Props::Paragraph => self.props = Props::Mark,
            "w:r" if is_start => {
                let hidden = self.paragraph_hidden();
                self.run = Some(Run {
                    hidden,
                    ..Run::default()
                });
            }
            "w:rPr" if is_start && self.run.is_some() => self.props = Props::Run,
            "w:txbxContent" if is_start => self.box_start()?,
            "w:tbl" if is_start => self.table_start()?,
            "w:tr" if is_start => self.row_start(),
            "w:trPr" if is_start => self.props = Props::Row,
            "w:tc" if is_start => self.cell_start(),
            "w:tcPr" if is_start => self.props = Props::Cell,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Text, breaks, fields, links, pictures and properties.
    fn content(
        &mut self,
        e: &BytesStart<'_>,
        name: &str,
        is_start: bool,
    ) -> Result<(), DomainError> {
        match name {
            "w:t" | "m:t" if is_start => self.text = TextKind::Shown,
            "w:instrText" if is_start => self.text = TextKind::Instruction,
            "w:tab" | "w:ptab" if self.props == Props::None => self.push_text("\t")?,
            "w:cr" => self.push_text("\n")?,
            "w:br" if self.props == Props::None => {
                if xml::attr(e, "w:type").is_some_and(|t| t.trim() == "page") {
                    self.page_break_in_para()?;
                } else {
                    self.push_text("\n")?;
                }
            }
            "w:noBreakHyphen" => self.push_text("-")?,
            "w:sym" => {
                if let Some(c) = symbol(e) {
                    self.push_text(c.encode_utf8(&mut [0; 4]))?;
                }
            }
            "w:fldChar" => self.field_char(e),
            "w:fldSimple" if is_start => {
                let link =
                    xml::attr(e, "w:instr").and_then(|i| hyperlink_field(cut_instruction(&i)));
                self.push_field(Field {
                    instruction: String::new(),
                    showing: true,
                    link,
                    simple: true,
                });
            }
            "w:hyperlink" if is_start => {
                let link = self.hyperlink(e);
                self.links.push(link);
            }
            "wp:docPr" => {
                self.picture = Some(Picture {
                    alt: xml::attr(e, "descr"),
                    title: xml::attr(e, "title"),
                    hidden: xml::attr(e, "hidden").is_some_and(|v| v == "1" || v == "true"),
                    decorative: false,
                });
                if !is_start {
                    self.emit_picture()?;
                }
            }
            "adec:decorative" => {
                if let Some(p) = self.picture.as_mut() {
                    p.decorative = xml::attr(e, "val").is_none_or(|v| v == "1" || v == "true");
                }
            }
            _ if is_vml_shape(name) => {
                let hr = name == "v:rect"
                    && xml::attr(e, "o:hr").is_some_and(|v| v == "t" || v == "true");
                if hr {
                    self.split_para(ParsedBlock::HorizontalRule)?;
                }
                // A group's alt text describes its shapes, which come out on
                // their own.
                let shape = VmlShape {
                    alt: xml::attr(e, "alt").filter(|_| !hr && name != "v:group"),
                    ..VmlShape::default()
                };
                if !is_start {
                    self.emit_vml(shape)?;
                } else if self.vml.len() < MAX_VML_DEPTH {
                    self.vml.push(shape);
                } else {
                    self.vml_overflow += 1;
                }
            }
            "v:imagedata" => {
                let title = xml::attr(e, "o:title");
                match self.vml.last_mut() {
                    Some(shape) => shape.title = title,
                    None => self.emit_vml(VmlShape {
                        title,
                        ..VmlShape::default()
                    })?,
                }
            }
            "v:textbox" => {
                if let Some(shape) = self.vml.last_mut() {
                    shape.text_box = true;
                }
            }
            _ => self.property(e, name),
        }
        Ok(())
    }

    /// Paragraph, border, section, run, row and cell properties.
    fn property(&mut self, e: &BytesStart<'_>, name: &str) {
        match self.props {
            Props::Paragraph => {
                let Some(para) = self.para.as_mut() else {
                    return;
                };
                match name {
                    "w:pStyle" => para.style = xml::attr(e, "w:val"),
                    "w:outlineLvl" => para.outline = level_attr(e).or(Some(u8::MAX)),
                    "w:numId" => {
                        para.num_id = xml::attr(e, "w:val").and_then(|v| v.trim().parse().ok());
                    }
                    "w:ilvl" => para.ilvl = Some(level_attr(e).unwrap_or(0)),
                    "w:pageBreakBefore" => para.page_break_before = Some(on_off(e)),
                    _ => {}
                }
            }
            Props::Borders => {
                let Some(para) = self.para.as_mut() else {
                    return;
                };
                let drawn = xml::attr(e, "w:val").is_some_and(|v| v != "none" && v != "nil");
                let side = match name {
                    "w:top" => 0,
                    "w:bottom" => 1,
                    "w:left" | "w:start" => 2,
                    "w:right" | "w:end" => 3,
                    _ => return,
                };
                if let Some(b) = para.borders.get_mut(side) {
                    *b = drawn;
                }
            }
            Props::Section => {
                if name == "w:type"
                    && let Some(para) = self.para.as_mut()
                {
                    para.section_break = !xml::attr(e, "w:val")
                        .is_some_and(|t| matches!(t.trim(), "continuous" | "nextColumn"));
                }
            }
            Props::Run => {
                if let Some(run) = self.run.as_mut() {
                    if name == "w:rStyle" {
                        run.style_id = xml::attr(e, "w:val");
                    } else {
                        run.direct.read(e);
                    }
                }
            }
            Props::Row => {
                let max = self.max_span();
                // Rows of tables nested past the depth cap are not tracked.
                if let Some(table) = self.tables.last_mut().filter(|_| self.flat_tables == 0) {
                    match name {
                        "w:tblHeader" => table.row_header = on_off(e),
                        "w:gridBefore" => {
                            push_empty_cells(table, span_attr(e, max));
                            self.check_overflow();
                        }
                        "w:gridAfter" => table.row_after = span_attr(e, max),
                        _ => {}
                    }
                }
            }
            Props::Cell => {
                let max = self.max_span();
                if let Some(table) = self.tables.last_mut().filter(|_| self.flat_tables == 0) {
                    match name {
                        "w:gridSpan" => table.span = span_attr(e, max).max(1),
                        "w:vMerge" => {
                            table.continuation =
                                xml::attr(e, "w:val").is_none_or(|v| v.trim() != "restart");
                        }
                        "w:hMerge" => {
                            table.continuation =
                                xml::attr(e, "w:val").is_none_or(|v| v.trim() != "restart");
                        }
                        _ => {}
                    }
                }
            }
            Props::Mark | Props::None => {}
        }
    }

    fn end(&mut self, name: &str) -> Result<(), DomainError> {
        if self.skip > 0 {
            self.skip -= 1;
            return Ok(());
        }
        if self.sdt_props > 0 {
            self.sdt_props -= 1;
            return Ok(());
        }
        match name {
            "mc:AlternateContent" => {
                self.alternates.end(name);
            }
            "w:pPr" | "w:trPr" | "w:tcPr" => self.props = Props::None,
            "w:pBdr" | "w:sectPr" => {
                if matches!(self.props, Props::Borders | Props::Section) {
                    self.props = Props::Paragraph;
                }
            }
            "w:rPr" if self.props == Props::Mark => self.props = Props::Paragraph,
            "w:rPr" if self.props == Props::Run => {
                self.props = Props::None;
                self.resolve_run();
            }
            "w:r" => self.run = None,
            "w:t" | "m:t" | "w:instrText" => self.text = TextKind::None,
            "w:fldSimple" => self.close_simple_field(),
            "w:sdt" => {
                self.sdts.pop();
            }
            _ if is_vml_shape(name) => {
                if self.vml_overflow > 0 {
                    self.vml_overflow -= 1;
                } else if let Some(shape) = self.vml.pop() {
                    self.emit_vml(shape)?;
                }
            }
            "w:hyperlink" => {
                self.links.pop();
            }
            "wp:docPr" => self.emit_picture()?,
            "w:p" => {
                if self.nested_p > 0 {
                    self.nested_p -= 1;
                } else if self.para.as_ref().is_some_and(|p| p.mark_deleted) {
                    // Read as accepted: the paragraph goes on in the next one.
                    self.joining = true;
                    self.run = None;
                    self.text = TextKind::None;
                } else {
                    self.finish_para()?;
                }
            }
            "w:txbxContent" => self.box_end()?,
            "w:tc" => self.cell_end()?,
            "w:tr" => self.row_end(),
            "w:tbl" => self.table_end()?,
            _ => {}
        }
        Ok(())
    }

    /// `mc:AlternateContent`: the first branch is read, the others skipped.
    /// Returns whether `name` was one of its elements.
    fn alternate(&mut self, name: &str, is_start: bool) -> bool {
        match self.alternates.start(name, is_start) {
            AltAction::Other => false,
            AltAction::Consumed => true,
            AltAction::SkipElement => {
                self.skip = 1;
                true
            }
        }
    }

    fn sink(&mut self) -> &mut Vec<ParsedBlock> {
        self.sinks.last_mut().unwrap_or(&mut self.document)
    }

    /// Puts out a block, charging it to the text budget.
    fn emit(&mut self, block: ParsedBlock) -> Result<(), DomainError> {
        self.budgets.text.charge_block()?;
        self.sink().push(block);
        Ok(())
    }

    fn in_table(&self) -> bool {
        !self.tables.is_empty()
    }

    // --- paragraphs ---------------------------------------------------------------

    fn para_start(&mut self) {
        if std::mem::take(&mut self.joining)
            && let Some(joined) = self.para.take()
        {
            // The text so far, with this paragraph's properties.
            self.para = Some(Para {
                inlines: joined.inlines,
                emitted: joined.emitted,
                pending: joined.pending,
                ..Para::default()
            });
            return;
        }
        if self.para.is_some() {
            self.nested_p += 1;
            return;
        }
        self.para = Some(Para::default());
    }

    /// What the open paragraph is: its block role and list membership.
    fn classify(&self, para: &Para) -> (Role, Option<(u8, bool)>) {
        let styles = self.ctx.styles;
        let role = styles.role(para.style.as_deref(), para.outline);
        // Tables hold paragraphs only: headings, code and quotes would break
        // the Markdown table.
        let role = if self.in_table() { Role::Body } else { role };
        let (style_num, style_ilvl) = styles.numbering(para.style.as_deref());
        let num_id = para.num_id.or(style_num).filter(|&n| n != 0);
        let level = para.ilvl.or(style_ilvl).unwrap_or(0).min(MAX_LEVEL);
        let numbering = self.ctx.numbering;
        // A definition that only links to a numbering style (`numStyleLink`)
        // takes its levels from that style's numbering, followed once.
        let list = num_id
            .and_then(|n| {
                numbering.kind(n, level).or_else(|| {
                    let style = numbering.style_link(n)?;
                    numbering.kind(styles.linked_numbering(style)?, level)
                })
            })
            .and_then(|kind| match kind {
                ListKind::Unordered => Some((level, false)),
                ListKind::Ordered => Some((level, true)),
                ListKind::Plain => None,
            });
        (role, list)
    }

    /// Puts out the paragraph's text so far as one block.
    fn flush_segment(&mut self, closing: bool) -> Result<(), DomainError> {
        let Some(mut para) = self.para.take() else {
            return Ok(());
        };
        let (role, list) = self.classify(&para);
        if !para.emitted
            && !self.in_table()
            && para
                .page_break_before
                .unwrap_or_else(|| self.ctx.styles.page_break_before(para.style.as_deref()))
        {
            self.push_page_break()?;
        }
        let block = if role == Role::Code {
            let code = para.inlines.text();
            para.inlines = Inlines::default();
            Some(ParsedBlock::CodeBlock {
                language: None,
                code,
            })
        } else {
            let inlines = para.inlines.take();
            if inlines.is_empty() {
                let [top, bottom, left, right] = para.borders;
                (closing && !para.emitted && (top || bottom) && !left && !right)
                    .then_some(ParsedBlock::HorizontalRule)
            } else {
                let paragraph = ParsedBlock::Paragraph { inlines };
                Some(match (role, list) {
                    (Role::Heading(level), _) => ParsedBlock::Heading {
                        level,
                        // One line: breaks and tabs in a heading read as spaces.
                        inlines: vec![Inline::plain(
                            plain_text(&paragraph)
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" "),
                        )],
                    },
                    (_, Some((level, ordered))) => ParsedBlock::ListItem {
                        level,
                        ordered,
                        blocks: vec![paragraph],
                    },
                    (Role::Quote, None) => ParsedBlock::Quote {
                        blocks: vec![paragraph],
                    },
                    _ => paragraph,
                })
            }
        };
        self.para = Some(para);
        if let Some(block) = block {
            if let Some(para) = self.para.as_mut() {
                para.emitted = true;
            }
            self.emit(block)?;
        }
        Ok(())
    }

    fn finish_para(&mut self) -> Result<(), DomainError> {
        self.joining = false;
        if self.para.is_none() {
            return Ok(());
        }
        self.flush_segment(true)?;
        // Field instructions never span paragraphs; an unclosed one must not
        // hide what follows. Fields showing their result (a table of
        // contents) stay open.
        self.fields
            .retain(|f| f.showing && f.link.is_none() && !f.simple);
        self.field_overflow = 0;
        self.run = None;
        self.text = TextKind::None;
        if let Some(para) = self.para.take() {
            if para.section_break && !self.in_table() {
                self.push_page_break()?;
            }
            // Charged as they were put out in the text box.
            let pending = para.pending;
            self.sink().extend(pending);
        }
        Ok(())
    }

    /// A block inside a paragraph (an image, a page break, a rule): the text
    /// before it, the block, then the rest of the paragraph.
    fn split_para(&mut self, block: ParsedBlock) -> Result<(), DomainError> {
        if self.para.is_some() {
            self.flush_segment(false)?;
            if let Some(para) = self.para.as_mut() {
                para.emitted = true;
            }
        }
        if block == ParsedBlock::PageBreak {
            self.push_page_break()
        } else {
            self.emit(block)
        }
    }

    fn page_break_in_para(&mut self) -> Result<(), DomainError> {
        if self.in_table() {
            // A table row cannot hold a page break.
            self.push_text("\n")
        } else {
            self.split_para(ParsedBlock::PageBreak)
        }
    }

    /// A page break, unless the output is empty or already ends with one.
    fn push_page_break(&mut self) -> Result<(), DomainError> {
        if self
            .sink()
            .last()
            .is_some_and(|b| *b != ParsedBlock::PageBreak)
        {
            self.emit(ParsedBlock::PageBreak)?;
        }
        Ok(())
    }

    // --- runs and text -----------------------------------------------------------

    fn resolve_run(&mut self) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let (style_toggles, style_code) = self.ctx.styles.character(run.style_id.as_deref());
        let t = run.direct.over(style_toggles);
        run.style = InlineStyle {
            bold: t.bold.unwrap_or(false),
            italic: t.italic.unwrap_or(false),
            underline: t.underline.unwrap_or(false),
            strike: t.strike.unwrap_or(false),
            code: style_code || t.mono.unwrap_or(false),
        };
        run.hidden = t.hidden.unwrap_or_else(|| {
            self.ctx
                .styles
                .hidden(self.para.as_ref().and_then(|p| p.style.as_deref()))
        });
    }

    /// Whether the open paragraph's style hides its text.
    fn paragraph_hidden(&self) -> bool {
        self.ctx
            .styles
            .hidden(self.para.as_ref().and_then(|p| p.style.as_deref()))
    }

    fn text(&mut self, text: &str) -> Result<(), DomainError> {
        if self.skip > 0 || self.sdt_props > 0 || text.is_empty() {
            return Ok(());
        }
        match self.text {
            TextKind::None => Ok(()),
            TextKind::Instruction => {
                if let Some(field) = self.fields.last_mut().filter(|f| !f.showing) {
                    let room = MAX_INSTRUCTION.saturating_sub(field.instruction.len());
                    let cut: String = text.chars().take(room).collect();
                    field.instruction.push_str(&cut);
                }
                Ok(())
            }
            TextKind::Shown => self.push_text(text),
        }
    }

    /// Adds text to the open paragraph in the current run's style, unless it
    /// is hidden or part of a field instruction. Its link is the innermost
    /// hyperlink field showing its result, else the innermost `w:hyperlink`
    /// (an unsafe inner link leaves its text plain).
    fn push_text(&mut self, text: &str) -> Result<(), DomainError> {
        if self.para.is_none()
            || self.run.as_ref().is_some_and(|r| r.hidden)
            || self.fields.iter().any(|f| !f.showing)
        {
            return Ok(());
        }
        // A line break belongs to no style.
        let style = match self.run.as_ref() {
            Some(r) if text != "\n" => r.style.clone(),
            _ => InlineStyle::default(),
        };
        let text = clean_text(text);
        // The link is borrowed from the field and hyperlink stacks while the
        // paragraph is borrowed mutably: no copy per run.
        let link = self
            .fields
            .iter()
            .rev()
            .find_map(|f| f.link.as_deref())
            .or_else(|| self.links.last().and_then(Option::as_deref));
        if let Some(para) = self.para.as_mut() {
            para.inlines
                .push(&text, &style, link, &mut self.budgets.text)?;
        }
        Ok(())
    }

    // --- fields and links --------------------------------------------------------

    fn push_field(&mut self, field: Field) {
        if self.fields.len() < MAX_FIELDS {
            self.fields.push(field);
        } else {
            self.field_overflow += 1;
        }
    }

    fn pop_field(&mut self) {
        if self.field_overflow > 0 {
            self.field_overflow -= 1;
        } else {
            self.fields.pop();
        }
    }

    /// `</w:fldSimple>`: closes that field and any complex field left open
    /// inside it.
    fn close_simple_field(&mut self) {
        if self.field_overflow > 0 {
            self.field_overflow -= 1;
            return;
        }
        while let Some(field) = self.fields.pop() {
            if field.simple {
                break;
            }
        }
    }

    fn field_char(&mut self, e: &BytesStart<'_>) {
        match xml::attr(e, "w:fldCharType").as_deref().map(str::trim) {
            Some("begin") => self.push_field(Field {
                instruction: String::new(),
                showing: false,
                link: None,
                simple: false,
            }),
            Some("separate") => {
                if self.field_overflow == 0
                    && let Some(field) = self.fields.last_mut()
                {
                    field.showing = true;
                    field.link = hyperlink_field(&field.instruction);
                }
            }
            Some("end") => self.pop_field(),
            _ => {}
        }
    }

    /// The target of `w:hyperlink`: an external hyperlink relationship with a
    /// safe scheme, with its anchor. Links within the document stay text.
    fn hyperlink(&mut self, e: &BytesStart<'_>) -> Option<String> {
        let id = xml::prefixed_attr(e, "id")?;
        // The relationship's target is checked once per id; the anchor
        // belongs to each element.
        let base = if let Some(cached) = self.link_cache.get(&id) {
            cached.clone()
        } else {
            let rel = self.ctx.rels.get(&id)?;
            let base = (rel.external && rel.is_type("hyperlink"))
                .then(|| link_target(&rel.target))
                .flatten();
            self.link_cache.insert(id, base.clone());
            base
        }?;
        match xml::attr(e, "w:anchor") {
            Some(a) if !a.is_empty() && !base.contains('#') => link_target(&format!("{base}#{a}")),
            _ => Some(base),
        }
    }

    // --- pictures and text boxes -------------------------------------------------

    /// A legacy VML picture: the shape's alt text and its image's title. A
    /// shape holding a text box is not a picture.
    fn emit_vml(&mut self, shape: VmlShape) -> Result<(), DomainError> {
        if shape.text_box {
            return Ok(());
        }
        self.picture = Some(Picture {
            alt: shape.alt,
            title: shape.title,
            ..Picture::default()
        });
        self.emit_picture()
    }

    fn emit_picture(&mut self) -> Result<(), DomainError> {
        let Some(picture) = self.picture.take() else {
            return Ok(());
        };
        if picture.hidden || picture.decorative || self.run.as_ref().is_some_and(|r| r.hidden) {
            return Ok(());
        }
        let alt = picture
            .alt
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty());
        let title = picture
            .title
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        if alt.is_none() && title.is_none() {
            return Ok(());
        }
        self.budgets
            .text
            .charge(alt.as_ref().map_or(0, String::len) + title.as_ref().map_or(0, String::len))?;
        self.split_para(ParsedBlock::Image {
            alt,
            title,
            src: None,
        })
    }

    fn box_start(&mut self) -> Result<(), DomainError> {
        if self.joining {
            // Outside any paragraph: the joined one ends here.
            self.finish_para()?;
        }
        // A text box in hidden text is hidden too.
        if self.run.as_ref().is_some_and(|r| r.hidden) {
            self.skip = 1;
            return Ok(());
        }
        if self.suspended.len() >= MAX_BOX_DEPTH {
            self.flat_boxes += 1;
            return Ok(());
        }
        self.suspended.push(Suspended {
            para: self.para.take(),
            run: self.run.take(),
            nested_p: std::mem::take(&mut self.nested_p),
        });
        self.sinks.push(Vec::new());
        Ok(())
    }

    fn box_end(&mut self) -> Result<(), DomainError> {
        if self.flat_boxes > 0 {
            self.flat_boxes -= 1;
            return Ok(());
        }
        let Some(saved) = self.suspended.pop() else {
            return Ok(());
        };
        // A paragraph left open in the box ends with it.
        if self.para.is_some() {
            self.finish_para()?;
        }
        let blocks = self.sinks.pop().unwrap_or_default();
        self.para = saved.para;
        self.run = saved.run;
        self.nested_p = saved.nested_p;
        match self.para.as_mut() {
            Some(host) => host.pending.extend(blocks),
            None => self.sink().extend(blocks),
        }
        Ok(())
    }

    // --- tables ------------------------------------------------------------------

    fn max_span(&self) -> usize {
        MAX_SPAN.min(self.ctx.limits.max_table_columns)
    }

    fn table_start(&mut self) -> Result<(), DomainError> {
        if self.para.is_some() {
            // Not valid inside a paragraph; end the paragraph first.
            self.finish_para()?;
        }
        if self.tables.len() >= MAX_TABLE_DEPTH {
            self.flat_tables += 1;
        } else {
            self.tables.push(Table::default());
        }
        Ok(())
    }

    fn row_start(&mut self) {
        if self.flat_tables > 0 {
            return;
        }
        if let Some(table) = self.tables.last_mut() {
            table.row_header = false;
            table.row_after = 0;
            table.row_deleted = false;
            if !table.overflow {
                table.rows.push(Vec::new());
            }
        }
    }

    fn cell_start(&mut self) {
        if self.flat_tables > 0 {
            return;
        }
        if let Some(table) = self.tables.last_mut() {
            table.span = 1;
            table.continuation = false;
            table.cell_deleted = false;
            self.sinks.push(Vec::new());
        }
    }

    /// Marks the table overflowing once it holds more cells than the budget
    /// has left; its cells are then counted but not kept.
    /// Every open table's cells count: an enclosing table is charged only
    /// when it ends, so nested tables would otherwise each get the whole
    /// budget.
    fn check_overflow(&mut self) {
        let remaining = self.budgets.tables.remaining();
        let open: usize = self.tables.iter().map(|t| t.cells).sum();
        if let Some(table) = self.tables.last_mut()
            && open > remaining
        {
            table.overflow = true;
        }
    }

    fn cell_end(&mut self) -> Result<(), DomainError> {
        if self.flat_tables > 0 || self.tables.is_empty() {
            return Ok(());
        }
        if self.para.is_some() {
            self.finish_para()?;
        }
        let mut blocks = self.sinks.pop().unwrap_or_default();
        let Some(table) = self.tables.last_mut() else {
            return Ok(());
        };
        if table.cell_deleted {
            // A tracked deletion: the cell is gone in the accepted document.
            return Ok(());
        }
        if table.continuation {
            // The merged cell's content is in the cell that starts the merge.
            blocks.clear();
        }
        push_cell(table, blocks);
        push_empty_cells(table, table.span.saturating_sub(1));
        self.check_overflow();
        Ok(())
    }

    fn row_end(&mut self) {
        if self.flat_tables > 0 {
            return;
        }
        if let Some(table) = self.tables.last_mut() {
            if table.row_deleted {
                if !table.overflow {
                    table.rows.pop();
                }
                return;
            }
            push_empty_cells(table, table.row_after);
            if table.row_header && table.rows.len() == table.header_rows + 1 {
                table.header_rows += 1;
            }
        }
        self.check_overflow();
    }

    fn table_end(&mut self) -> Result<(), DomainError> {
        if self.flat_tables > 0 {
            self.flat_tables -= 1;
            return Ok(());
        }
        let Some(mut table) = self.tables.pop() else {
            return Ok(());
        };
        table.rows.retain(|r| !r.is_empty());
        if !self.tables.is_empty() {
            // A table inside a cell cannot be drawn there: its cells' blocks
            // join the enclosing cell, row by row (charged as they were put
            // out).
            self.budgets.tables.spend(table.cells);
            let blocks: Vec<ParsedBlock> = table
                .rows
                .into_iter()
                .flatten()
                .flat_map(|c| c.blocks)
                .filter(|b| !is_empty_paragraph(b))
                .collect();
            self.sink().extend(blocks);
            return Ok(());
        }
        let width = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 && !table.overflow {
            return Ok(());
        }
        let kept =
            self.budgets
                .tables
                .admit("DOCX", table.rows.len(), width, table.cells, table.overflow);
        if !kept {
            let remaining = self.budgets.tables.remaining();
            let note = table_note(remaining, &mut self.budgets.text)?;
            self.sink().push(note);
            return Ok(());
        }
        // Rows marked as repeating headers; with none, the first row (a
        // Markdown table needs a header).
        let headers = table.header_rows.max(1);
        let rows = table
            .rows
            .into_iter()
            .enumerate()
            .map(|(i, mut cells)| {
                cells.resize_with(width, empty_cell);
                TableRow {
                    is_header: i < headers,
                    cells,
                }
            })
            .collect();
        self.emit(ParsedBlock::Table(TableBlock { rows }))
    }
}

/// VML elements that can hold a picture or a text box.
fn is_vml_shape(name: &str) -> bool {
    matches!(
        name,
        "v:shape"
            | "v:rect"
            | "v:roundrect"
            | "v:oval"
            | "v:line"
            | "v:polyline"
            | "v:arc"
            | "v:curve"
            | "v:image"
            | "v:group"
    )
}

/// Elements whose subtree holds nothing to read: old properties of tracked
/// changes, deleted and moved-away text, ruby annotations, embedded objects,
/// table layout and tab stops.
fn skipped(name: &str) -> bool {
    name.ends_with("Change")
        || matches!(
            name,
            "w:del"
                | "w:moveFrom"
                | "w:delText"
                | "w:delInstrText"
                | "w:sdtEndPr"
                | "w:smartTagPr"
                | "w:customXmlPr"
                | "w:rt"
                | "w:object"
                | "w:control"
                | "w:altChunk"
                | "w:subDoc"
                | "w:tblPr"
                | "w:tblGrid"
                | "w:tblPrEx"
                | "w:tabs"
                | "w:fldData"
        )
}

fn empty_cell() -> TableCell {
    TableCell {
        blocks: vec![ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("")],
        }],
    }
}

fn is_empty_paragraph(block: &ParsedBlock) -> bool {
    matches!(block, ParsedBlock::Paragraph { inlines } if inlines.iter().all(|i| plain_inline(i).is_empty()))
}

/// Pads the row with `n` empty cells (spans, `gridBefore`, `gridAfter`); a
/// table already over the budget only counts them.
fn push_empty_cells(table: &mut Table, n: usize) {
    if table.overflow {
        table.cells = table.cells.saturating_add(n);
        return;
    }
    for _ in 0..n {
        push_cell(table, Vec::new());
    }
}

fn push_cell(table: &mut Table, blocks: Vec<ParsedBlock>) {
    table.cells += 1;
    if table.overflow {
        return;
    }
    let cell = if blocks.is_empty() {
        empty_cell()
    } else {
        TableCell { blocks }
    };
    match table.rows.last_mut() {
        Some(row) => row.push(cell),
        None => table.rows.push(vec![cell]),
    }
}

/// A span count (`w:gridSpan`, `w:gridBefore`), clamped to `max`.
fn span_attr(e: &BytesStart<'_>, max: usize) -> usize {
    xml::attr(e, "w:val")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .min(max)
}

fn plain_inline(inline: &Inline) -> &str {
    match inline {
        Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. } => text,
    }
}

fn plain_text(block: &ParsedBlock) -> String {
    match block {
        ParsedBlock::Paragraph { inlines } => inlines.iter().map(plain_inline).collect(),
        _ => String::new(),
    }
}

/// The first [`MAX_INSTRUCTION`] characters of a `w:fldSimple` instruction,
/// as kept for complex fields: tokenizing a huge one would cost memory.
fn cut_instruction(instruction: &str) -> &str {
    match instruction.char_indices().nth(MAX_INSTRUCTION) {
        Some((end, _)) => instruction.get(..end).unwrap_or(instruction),
        None => instruction,
    }
}

/// The target of a `HYPERLINK` field instruction (`HYPERLINK "url" \l
/// "anchor"`); links within the document stay text.
fn hyperlink_field(instruction: &str) -> Option<String> {
    let rest = instruction.trim_start();
    let head = rest.get(..9)?;
    if !head.eq_ignore_ascii_case("hyperlink") {
        return None;
    }
    let mut url: Option<String> = None;
    let mut anchor: Option<String> = None;
    let mut tokens = tokens(rest.get(9..)?).into_iter();
    while let Some(token) = tokens.next() {
        match token.as_str() {
            "\\l" => anchor = tokens.next(),
            "\\o" | "\\t" | "\\m" | "\\n" => {
                tokens.next();
            }
            t if !t.starts_with('\\') && url.is_none() => url = Some(token),
            _ => {}
        }
    }
    let url = url?;
    let url = match anchor {
        Some(a) if !url.contains('#') => format!("{url}#{a}"),
        _ => url,
    };
    link_target(&url)
}

/// Field instruction tokens: quoted strings and bare words.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            out.push(chars.by_ref().take_while(|&c| c != '"').collect());
        } else {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            out.push(word);
        }
    }
    out
}

/// Text as it reads: control characters other than tab and line feed are
/// dropped.
fn clean_text(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(|c: char| c.is_control() && !matches!(c, '\t' | '\n')) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(
        text.chars()
            .filter(|c| !c.is_control() || matches!(c, '\t' | '\n'))
            .collect(),
    )
}

/// The character `w:sym` shows: Symbol-font code points mapped to Unicode,
/// other private-use code points dropped.
fn symbol(e: &BytesStart<'_>) -> Option<char> {
    let code = u32::from_str_radix(xml::attr(e, "w:char")?.trim(), 16).ok()?;
    let symbol_font = xml::attr(e, "w:font").is_some_and(|f| f.eq_ignore_ascii_case("symbol"));
    let low = if (0xF000..=0xF0FF).contains(&code) {
        code - 0xF000
    } else {
        code
    };
    if symbol_font && low <= 0xFF {
        return symbol_char(u8::try_from(low).ok()?);
    }
    let c = char::from_u32(code)?;
    (!(0xE000..=0xF8FF).contains(&code)).then_some(c)
}

/// The Unicode character of a Symbol-font code.
fn symbol_char(code: u8) -> Option<char> {
    const UPPER: &str = "\u{391}\u{392}\u{3a7}\u{394}\u{395}\u{3a6}\u{393}\u{397}\u{399}\u{3d1}\u{39a}\u{39b}\u{39c}\u{39d}\u{39f}\u{3a0}\u{398}\u{3a1}\u{3a3}\u{3a4}\u{3a5}\u{3c2}\u{3a9}\u{39e}\u{3a8}\u{396}";
    const LOWER: &str = "\u{3b1}\u{3b2}\u{3c7}\u{3b4}\u{3b5}\u{3c6}\u{3b3}\u{3b7}\u{3b9}\u{3d5}\u{3ba}\u{3bb}\u{3bc}\u{3bd}\u{3bf}\u{3c0}\u{3b8}\u{3c1}\u{3c3}\u{3c4}\u{3c5}\u{3d6}\u{3c9}\u{3be}\u{3c8}\u{3b6}";
    match code {
        b'A'..=b'Z' => UPPER.chars().nth(usize::from(code - b'A')),
        b'a'..=b'z' => LOWER.chars().nth(usize::from(code - b'a')),
        0x40 => Some('\u{2245}'),
        0x20..=0x3F if !b"\"$'*-".contains(&code) => Some(char::from(code)),
        0xA3 => Some('\u{2264}'),
        0xA5 => Some('\u{221e}'),
        0xAB => Some('\u{2194}'),
        0xAC => Some('\u{2190}'),
        0xAD => Some('\u{2191}'),
        0xAE => Some('\u{2192}'),
        0xAF => Some('\u{2193}'),
        0xB0 => Some('\u{b0}'),
        0xB1 => Some('\u{b1}'),
        0xB3 => Some('\u{2265}'),
        0xB4 => Some('\u{d7}'),
        0xB7 => Some('\u{2022}'),
        0xB8 => Some('\u{f7}'),
        0xB9 => Some('\u{2260}'),
        0xBA => Some('\u{2261}'),
        0xBB => Some('\u{2248}'),
        0xD6 => Some('\u{221a}'),
        0xE5 => Some('\u{2211}'),
        0xD5 => Some('\u{220f}'),
        0xF2 => Some('\u{222b}'),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use quick_xml::Reader;

    use super::*;

    /// Feeds the start of a body (`xml`, left unclosed) to a walker and
    /// hands the walker to `check`.
    fn walk_prefix(xml: &str, limits: &ParseLimits, check: impl FnOnce(&Walker<'_, '_>)) {
        let (styles, numbering, rels) = (
            Styles::default(),
            Numbering::default(),
            Relationships::default(),
        );
        let ctx = DocContext {
            styles: &styles,
            numbering: &numbering,
            rels: &rels,
            limits,
        };
        let mut budgets = Budgets::new(limits);
        let mut walker = Walker::new(&ctx, &mut budgets);
        let mut reader = Reader::from_str(xml);
        loop {
            match reader.read_event().expect("well-formed prefix") {
                Event::Eof => break,
                event => walker.event(event).expect("within limits"),
            }
        }
        check(&walker);
    }

    #[test]
    fn an_inner_table_counts_the_open_outer_table() {
        // Nothing is charged until the outer table ends, so the inner table
        // must count the outer one's 30 cells: each table fits the budget of
        // 50 alone, both together do not. Once the inner table overflows it
        // keeps no more cells.
        let limits = ParseLimits {
            max_table_cells: 50,
            ..ParseLimits::default()
        };
        let filler = r#"<w:tc><w:tcPr><w:gridSpan w:val="30"/></w:tcPr></w:tc>"#;
        let xml = format!("<w:tbl><w:tr>{filler}<w:tc><w:tbl><w:tr>{filler}<w:tc></w:tc>");
        walk_prefix(&xml, &limits, |walker| {
            let [outer, inner] = walker.tables.as_slice() else {
                panic!("two open tables");
            };
            assert!(!outer.overflow);
            assert!(
                inner.overflow,
                "the inner table counts the outer one's cells"
            );
            assert_eq!(inner.cells, 31);
            assert_eq!(
                inner.rows.iter().map(Vec::len).sum::<usize>(),
                30,
                "the cell after the overflow is counted, not kept"
            );
        });
    }

    #[test]
    fn closed_content_controls_leave_the_stack() {
        let xml = "<w:sdt><w:sdtPr><w:showingPlcHdr/></w:sdtPr><w:sdtContent/></w:sdt><w:sdt><w:sdtPr/><w:sdtContent>";
        walk_prefix(xml, &ParseLimits::default(), |walker| {
            assert_eq!(walker.sdts, [false], "one entry per open content control");
        });
    }

    #[test]
    fn an_overflowing_table_counts_padding_without_building_it() {
        // Spans and grid padding repeat for every row and cell: once the
        // table is over the budget they must not loop cell by cell. A
        // regression shows as a hang, which the nextest timeout catches.
        let mut table = Table {
            overflow: true,
            ..Table::default()
        };
        push_empty_cells(&mut table, usize::MAX);
        assert_eq!(table.cells, usize::MAX);
        assert!(table.rows.is_empty());
    }
}
