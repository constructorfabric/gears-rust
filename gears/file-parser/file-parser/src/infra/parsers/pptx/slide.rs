//! The content of one slide part.
//!
//! The part is read as a flat stream of XML events with a little state (the
//! shape, text body, paragraph and table being read), so group shapes nest
//! without recursion and the work is linear in the part. Shapes come out in
//! `p:spTree` order, the order `PowerPoint`'s reading order and accessibility
//! tools start from.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, InlineStyle, ParsedBlock, TableBlock, TableCell, TableRow};
use crate::infra::parsers::inlines::{Inlines, link_target};
use crate::infra::parsers::limits::{Budgets, ParseLimits, TextBudget, table_note};
use crate::infra::parsers::ooxml::{AltAction, Alternates, Events, Relationships, xml};

use super::styles::{Bullet, Inherited, Kind, LEVELS, ListStyle, Placeholder};

/// What one slide holds.
#[derive(Debug, Default)]
pub(super) struct Slide {
    /// Text of the slide's title placeholder; empty when it has none.
    pub(super) title: String,
    pub(super) blocks: Vec<ParsedBlock>,
    /// Targets of the `SmartArt` data parts the slide refers to, each with
    /// the index in `blocks` where its text belongs.
    pub(super) diagrams: Vec<(usize, String)>,
}

/// What a slide's shapes are resolved against.
pub(super) struct SlideContext<'a> {
    pub(super) inherited: &'a Inherited,
    pub(super) rels: &'a Relationships,
    pub(super) limits: &'a ParseLimits,
}

/// Reads the shapes of a slide part.
pub(super) fn read_slide(
    text: &str,
    ctx: &SlideContext<'_>,
    budgets: &mut Budgets,
) -> Result<Slide, DomainError> {
    let mut walker = Walker::new(ctx, budgets);
    let mut events = Events::new(text, ctx.limits, "PPTX");
    loop {
        match events.next()? {
            Event::Eof => return Ok(walker.slide),
            event => walker.event(event)?,
        }
    }
}

/// A paragraph being read.
#[derive(Default)]
struct Para {
    level: usize,
    bullet: Option<Bullet>,
    inlines: Inlines,
}

/// The run (`a:r`, `a:fld`) being read.
#[derive(Default)]
struct Run {
    style: InlineStyle,
    link: Option<String>,
    in_props: bool,
}

/// A text body (`p:txBody`, `a:txBody`) being read.
#[derive(Default)]
struct Body {
    style: ListStyle,
    list_style: ListStyleAt,
    para: Option<Para>,
    in_para_props: bool,
    run: Option<Run>,
    /// Inside `a:t` of a run, or `m:t` of an equation.
    in_text: bool,
    paras: Vec<Para>,
}

/// Where in a text body's `a:lstStyle` the reader is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ListStyleAt {
    #[default]
    Outside,
    Inside,
    /// Inside the `a:lvlNpPr` of this level.
    Level(usize),
}

/// A shape (`p:sp`) being read.
#[derive(Default)]
struct Shape {
    placeholder: Option<Placeholder>,
    style: ListStyle,
    paras: Vec<Para>,
}

/// A table (`a:tbl`) being read.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<TableCell>>,
    cell: Option<Vec<ParsedBlock>>,
    /// Cells read, whether kept or not.
    cells: usize,
    /// Set once the table has more cells than the budget has left; the rest
    /// of its cells are counted but not read.
    overflow: bool,
}

/// A picture (`p:pic`) whose `p:cNvPr` is being read.
#[derive(Default)]
struct Picture {
    alt: Option<String>,
    title: Option<String>,
    decorative: bool,
    /// `p:cNvPr` was seen; later ones belong to something else.
    described: bool,
}

struct Walker<'a, 'b> {
    ctx: &'a SlideContext<'a>,
    budgets: &'b mut Budgets,
    slide: Slide,
    /// Depth inside a subtree being skipped; 0 when reading.
    skip: usize,
    /// One entry per open `mc:AlternateContent`: whether a branch was taken.
    alternates: Alternates,
    shape: Option<Shape>,
    body: Option<Body>,
    table: Option<Table>,
    picture: Option<Picture>,
    /// Inside a `p:nv*Pr` element, where a hidden `p:cNvPr` hides its parent.
    non_visual: bool,
    /// `SmartArt` relationship ids already queued for this slide.
    diagram_ids: HashSet<String>,
    /// Resolved `a:hlinkClick` targets by relationship id: one long target
    /// shared by many runs is judged once.
    links: HashMap<String, Option<String>>,
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(ctx: &'a SlideContext<'a>, budgets: &'b mut Budgets) -> Self {
        Self {
            ctx,
            budgets,
            slide: Slide::default(),
            skip: 0,
            alternates: Alternates::default(),
            shape: None,
            body: None,
            table: None,
            picture: None,
            non_visual: false,
            diagram_ids: HashSet::new(),
            links: HashMap::new(),
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
        if self.alternate(name, is_start) {
            return Ok(());
        }
        if self.body.is_some() {
            return self.body_start(e, name, is_start);
        }
        match name {
            // Extension data holds no text; speaker notes and comments live
            // in other parts and are not read.
            "p:extLst" if is_start => self.skip = 1,
            "p:nvSpPr" | "p:nvPicPr" | "p:nvGrpSpPr" | "p:nvGraphicFramePr" | "p:nvCxnSpPr"
                if is_start =>
            {
                self.non_visual = true;
            }
            "p:cNvPr" => self.non_visual(e, is_start)?,
            "adec:decorative" => {
                if let Some(picture) = self.picture.as_mut()
                    && xml::attr(e, "val").is_some_and(|v| is_true(&v))
                {
                    picture.decorative = true;
                }
            }
            "p:sp" if is_start => {
                if self.shape.is_some() || self.table.is_some() {
                    self.skip = 1;
                } else {
                    self.shape = Some(Shape::default());
                }
            }
            "p:ph" => {
                if let Some(shape) = self.shape.as_mut() {
                    shape.placeholder = Some(Placeholder::from_element(e));
                }
            }
            "p:txBody" if is_start => {
                if self.shape.is_some() {
                    self.body = Some(Body::default());
                } else {
                    self.skip = 1;
                }
            }
            "a:txBody" if is_start => {
                if self.table.as_ref().is_some_and(|t| t.cell.is_some()) {
                    self.body = Some(Body::default());
                } else {
                    self.skip = 1;
                }
            }
            "p:pic" if is_start => self.picture = Some(Picture::default()),
            "dgm:relIds" => {
                if let Some(id) = xml::prefixed_attr(e, "dm")
                    && let Some(rel) = self
                        .ctx
                        .rels
                        .get(&id)
                        .filter(|rel| !rel.external && rel.is_type("diagramData"))
                    && self.diagram_ids.insert(id)
                {
                    self.slide
                        .diagrams
                        .push((self.slide.blocks.len(), rel.target.clone()));
                }
            }
            "a:tbl" if is_start => {
                if self.table.is_some() || self.shape.is_some() {
                    self.skip = 1;
                } else {
                    self.table = Some(Table::default());
                }
            }
            "a:tr" => {
                if let Some(table) = self.table.as_mut()
                    && !table.overflow
                {
                    // Every row costs at least one cell once padded.
                    if table.rows.len() >= self.budgets.tables.remaining() {
                        table.overflow = true;
                    } else {
                        table.rows.push(Vec::new());
                    }
                }
            }
            "a:tc" => self.cell_start(is_start),
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, name: &str) -> Result<(), DomainError> {
        if self.skip > 0 {
            self.skip -= 1;
            return Ok(());
        }
        if self.alternates.end(name) {
            return Ok(());
        }
        if self.body.is_some() {
            return self.body_end(name);
        }
        match name {
            "p:nvSpPr" | "p:nvPicPr" | "p:nvGrpSpPr" | "p:nvGraphicFramePr" | "p:nvCxnSpPr" => {
                self.non_visual = false;
            }
            "p:cNvPr" => self.picture_described()?,
            "p:sp" => {
                if let Some(shape) = self.shape.take() {
                    self.emit_shape(shape)?;
                }
            }
            "p:pic" => self.picture = None,
            "a:tc" => {
                if let Some(table) = self.table.as_mut()
                    && let Some(blocks) = table.cell.take()
                {
                    push_cell(table, blocks);
                }
            }
            "a:tbl" => {
                if let Some(table) = self.table.take() {
                    self.emit_table(table)?;
                }
            }
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

    /// `p:cNvPr` of a shape, group, frame or picture: a hidden one is skipped
    /// with everything it describes.
    fn non_visual(&mut self, e: &BytesStart<'_>, is_start: bool) -> Result<(), DomainError> {
        // Shapes are not valid inside a table, so a `p:cNvPr` there describes
        // nothing that can be skipped safely.
        if self.non_visual
            && self.table.is_none()
            && xml::attr(e, "hidden").is_some_and(|v| is_true(&v))
        {
            // Skip to the end of the element the `p:nv*Pr` describes.
            self.skip = 2 + usize::from(is_start);
            self.non_visual = false;
            self.shape = None;
            self.picture = None;
            return Ok(());
        }
        if let Some(picture) = self.picture.as_mut()
            && !picture.described
        {
            picture.alt = xml::attr(e, "descr").map(|v| v.trim().to_owned());
            picture.title = xml::attr(e, "title").map(|v| v.trim().to_owned());
            if !is_start {
                // No extension list can follow; describe it now.
                picture.described = true;
                return self.emit_picture();
            }
        }
        Ok(())
    }

    fn picture_described(&mut self) -> Result<(), DomainError> {
        if let Some(picture) = self.picture.as_mut()
            && !picture.described
        {
            picture.described = true;
            return self.emit_picture();
        }
        Ok(())
    }

    fn emit_picture(&mut self) -> Result<(), DomainError> {
        let Some(picture) = self.picture.as_mut() else {
            return Ok(());
        };
        if picture.decorative {
            return Ok(());
        }
        let alt = picture.alt.take().filter(|v| !v.is_empty());
        let title = picture.title.take().filter(|v| !v.is_empty());
        if alt.is_none() && title.is_none() {
            return Ok(());
        }
        let bytes = alt.as_ref().map_or(0, String::len) + title.as_ref().map_or(0, String::len);
        self.budgets.text.charge(bytes)?;
        self.budgets.text.charge_block()?;
        self.slide.blocks.push(ParsedBlock::Image {
            alt,
            title,
            src: None,
        });
        Ok(())
    }

    fn cell_start(&mut self, is_start: bool) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        table.cells += 1;
        if table.cells > self.budgets.tables.remaining() {
            table.overflow = true;
        }
        if table.overflow {
            self.skip = usize::from(is_start);
        } else if is_start {
            table.cell = Some(Vec::new());
        } else {
            push_cell(table, Vec::new());
        }
    }

    fn body_start(
        &mut self,
        e: &BytesStart<'_>,
        name: &str,
        is_start: bool,
    ) -> Result<(), DomainError> {
        let Some(body) = self.body.as_mut() else {
            return Ok(());
        };
        if body.list_style != ListStyleAt::Outside {
            if let Some(level) = ListStyle::level_of(name) {
                if is_start {
                    body.list_style = ListStyleAt::Level(level);
                }
            } else if let (ListStyleAt::Level(level), Some(bullet)) =
                (body.list_style, Bullet::from_element(name))
            {
                body.style.set(level, bullet);
            }
            return Ok(());
        }
        match name {
            "a:lstStyle" if is_start => body.list_style = ListStyleAt::Inside,
            "a:p" if is_start => body.para = Some(Para::default()),
            // Only the paragraph's own properties; a field (`a:fld`) may carry
            // an `a:pPr` too.
            "a:pPr" if body.run.is_none() => {
                if let Some(para) = body.para.as_mut() {
                    para.level = xml::attr(e, "lvl")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .map_or(0, |l| l.min(LEVELS - 1));
                    body.in_para_props = is_start;
                }
            }
            "a:r" | "a:fld" if is_start && body.para.is_some() => body.run = Some(Run::default()),
            "a:rPr" => {
                if let Some(run) = body.run.as_mut() {
                    run.style = run_style(e);
                    run.in_props = is_start;
                }
            }
            "a:hlinkClick" => {
                if body.run.as_ref().is_some_and(|r| r.in_props) {
                    let link = self.link(e);
                    if let Some(run) = self.body.as_mut().and_then(|b| b.run.as_mut()) {
                        run.link = link;
                    }
                }
            }
            // Equation text (`m:t`) is kept as plain text.
            "a:t" | "m:t" if is_start => body.in_text = body.para.is_some(),
            "a:br" => {
                if let Some(para) = body.para.as_mut() {
                    para.inlines.push(
                        "\n",
                        &InlineStyle::default(),
                        None,
                        &mut self.budgets.text,
                    )?;
                }
            }
            "a:extLst" if is_start => self.skip = 1,
            _ if body.in_para_props => {
                if let (Some(bullet), Some(para)) = (Bullet::from_element(name), body.para.as_mut())
                {
                    para.bullet = Some(bullet);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn body_end(&mut self, name: &str) -> Result<(), DomainError> {
        let Some(body) = self.body.as_mut() else {
            return Ok(());
        };
        match name {
            "a:lstStyle" => body.list_style = ListStyleAt::Outside,
            _ if body.list_style != ListStyleAt::Outside => {
                if ListStyle::level_of(name).is_some() {
                    body.list_style = ListStyleAt::Inside;
                }
            }
            "a:pPr" => body.in_para_props = false,
            "a:rPr" => {
                if let Some(run) = body.run.as_mut() {
                    run.in_props = false;
                }
            }
            "a:t" | "m:t" => body.in_text = false,
            "a:r" | "a:fld" => body.run = None,
            "a:p" => {
                body.run = None;
                body.in_text = false;
                // An empty paragraph puts out nothing; buffering it until
                // the shape ends would cost memory for nothing.
                if let Some(para) = body.para.take().filter(|p| !p.inlines.is_empty()) {
                    body.paras.push(para);
                }
            }
            "p:txBody" | "a:txBody" => {
                if let Some(body) = self.body.take() {
                    self.finish_body(body)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), DomainError> {
        if self.skip > 0 || text.is_empty() {
            return Ok(());
        }
        let Some(body) = self.body.as_mut().filter(|b| b.in_text) else {
            return Ok(());
        };
        let Some(para) = body.para.as_mut() else {
            return Ok(());
        };
        let (style, link) = match body.run.as_ref() {
            Some(run) => (run.style.clone(), run.link.as_deref()),
            None => (InlineStyle::default(), None),
        };
        para.inlines
            .push(&clean_text(text), &style, link, &mut self.budgets.text)
    }

    /// The link target of `a:hlinkClick`: external hyperlink relationships
    /// with a safe scheme only. Actions (`ppaction://` slide jumps, macros,
    /// programs) and other relationships stay plain text.
    fn link(&mut self, e: &BytesStart<'_>) -> Option<String> {
        if xml::attr(e, "action").is_some_and(|a| a.trim_start().starts_with("ppaction://")) {
            return None;
        }
        let id = xml::prefixed_attr(e, "id")?;
        let rel = self.ctx.rels.get(&id)?;
        self.links
            .entry(id)
            .or_insert_with(|| {
                (rel.external && rel.is_type("hyperlink"))
                    .then(|| link_target(&rel.target))
                    .flatten()
            })
            .clone()
    }

    /// Hands a finished text body to its table cell or shape.
    fn finish_body(&mut self, body: Body) -> Result<(), DomainError> {
        if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
            // Cells inherit no bullets from the layout.
            for para in body.paras {
                let bullet = para.bullet.or_else(|| body.style.get(para.level));
                push_para(cell, para, bullet, &mut self.budgets.text)?;
            }
        } else if let Some(shape) = self.shape.as_mut() {
            shape.style = body.style;
            shape.paras.extend(body.paras);
        }
        Ok(())
    }

    fn emit_shape(&mut self, shape: Shape) -> Result<(), DomainError> {
        let placeholder = shape.placeholder;
        match placeholder.map(|p| self.ctx.inherited.kind(&p)) {
            Some(Kind::Furniture) => return Ok(()),
            Some(Kind::Title) if self.slide.title.is_empty() => {
                self.slide.title = title_text(shape.paras);
                return Ok(());
            }
            Some(Kind::Title) => {
                // A second title: plain paragraphs.
                for para in shape.paras {
                    push_para(&mut self.slide.blocks, para, None, &mut self.budgets.text)?;
                }
                return Ok(());
            }
            _ => {}
        }
        for para in shape.paras {
            let bullet = para
                .bullet
                .or_else(|| shape.style.get(para.level))
                .or_else(|| self.ctx.inherited.bullet(placeholder.as_ref(), para.level));
            push_para(&mut self.slide.blocks, para, bullet, &mut self.budgets.text)?;
        }
        Ok(())
    }

    fn emit_table(&mut self, mut table: Table) -> Result<(), DomainError> {
        let width = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 && !table.overflow {
            return Ok(());
        }
        let kept =
            self.budgets
                .tables
                .admit("PPTX", table.rows.len(), width, table.cells, table.overflow);
        if !kept {
            let remaining = self.budgets.tables.remaining();
            let note = table_note(remaining, &mut self.budgets.text)?;
            self.slide.blocks.push(note);
            return Ok(());
        }
        let rows = table
            .rows
            .drain(..)
            .enumerate()
            .map(|(i, mut cells)| {
                cells.resize_with(width, || TableCell { blocks: Vec::new() });
                TableRow {
                    // The first row is the header: Markdown tables need one,
                    // and PowerPoint tables have one by default (`firstRow`).
                    is_header: i == 0,
                    cells,
                }
            })
            .collect();
        self.budgets.text.charge_block()?;
        self.slide
            .blocks
            .push(ParsedBlock::Table(TableBlock { rows }));
        Ok(())
    }
}

fn push_cell(table: &mut Table, blocks: Vec<ParsedBlock>) {
    let cell = TableCell { blocks };
    match table.rows.last_mut() {
        Some(row) => row.push(cell),
        None => table.rows.push(vec![cell]),
    }
}

/// Appends `para` as a list item or paragraph, charged to `text`; empty
/// paragraphs are dropped, as `PowerPoint` draws no bullet for them.
fn push_para(
    out: &mut Vec<ParsedBlock>,
    mut para: Para,
    bullet: Option<Bullet>,
    text: &mut TextBudget,
) -> Result<(), DomainError> {
    let inlines = para.inlines.take();
    if inlines.is_empty() {
        return Ok(());
    }
    text.charge_block()?;
    let paragraph = ParsedBlock::Paragraph { inlines };
    out.push(match bullet {
        Some(bullet @ (Bullet::Unordered | Bullet::Ordered)) => ParsedBlock::ListItem {
            // At most LEVELS - 1.
            level: u8::try_from(para.level).unwrap_or(u8::MAX),
            ordered: bullet == Bullet::Ordered,
            blocks: vec![paragraph],
        },
        Some(Bullet::None) | None => paragraph,
    });
    Ok(())
}

/// The text of a title placeholder as one line: paragraphs, line breaks and
/// whitespace runs become single spaces. Runs join without a separator, as
/// a word is often split across runs.
fn title_text(paras: Vec<Para>) -> String {
    let mut raw = String::new();
    for mut para in paras {
        raw.push(' ');
        for inline in para.inlines.take() {
            let (Inline::Text { text, .. } | Inline::Link { text, .. } | Inline::Code { text, .. }) =
                inline;
            raw.push_str(&text);
        }
    }
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run text as it reads: a vertical tab (`PowerPoint`'s line break inside a
/// run) and a carriage return become line breaks, other control characters
/// smuggled in through character references are dropped.
pub(super) fn clean_text(text: &str) -> Cow<'_, str> {
    if !text.contains(|c: char| c.is_control() && !matches!(c, '\t' | '\n')) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|c| match c {
                '\u{b}' | '\r' => Some('\n'),
                '\t' | '\n' => Some(c),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect(),
    )
}

/// The style `a:rPr` gives a run.
fn run_style(e: &BytesStart<'_>) -> InlineStyle {
    InlineStyle {
        bold: xml::attr(e, "b").is_some_and(|v| is_true(&v)),
        italic: xml::attr(e, "i").is_some_and(|v| is_true(&v)),
        underline: xml::attr(e, "u").is_some_and(|v| v.trim() != "none"),
        strike: xml::attr(e, "strike").is_some_and(|v| v.trim() != "noStrike"),
        code: false,
    }
}

fn is_true(value: &str) -> bool {
    let value = value.trim();
    value == "1" || value.eq_ignore_ascii_case("true")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use quick_xml::Reader;

    use super::*;

    #[test]
    fn empty_paragraphs_are_dropped_as_they_end() {
        // They put out nothing; kept until the shape ends, millions of them
        // would hold memory no budget charges.
        let xml = format!(
            "<p:sp><p:txBody>{}<a:p><a:r><a:t>x</a:t></a:r></a:p><a:p/>",
            "<a:p></a:p>".repeat(1_000)
        );
        let limits = ParseLimits::default();
        let (inherited, rels) = (Inherited::default(), Relationships::default());
        let ctx = SlideContext {
            inherited: &inherited,
            rels: &rels,
            limits: &limits,
        };
        let mut budgets = Budgets::new(&limits);
        let mut walker = Walker::new(&ctx, &mut budgets);
        let mut reader = Reader::from_str(&xml);
        loop {
            match reader.read_event().expect("well-formed prefix") {
                Event::Eof => break,
                event => walker.event(event).expect("within limits"),
            }
        }
        let paras = walker.body.as_ref().map(|b| b.paras.len());
        assert_eq!(paras, Some(1));
    }
}
