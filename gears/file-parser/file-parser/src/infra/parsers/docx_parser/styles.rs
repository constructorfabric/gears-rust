//! Styles (`styles.xml`) and list numbering (`numbering.xml`).
//!
//! Only what the output uses is read: heading outline levels and names,
//! list numbering, page breaks, the code and quote style names, and the run
//! toggles of character styles. Style chains (`w:basedOn`) are followed with
//! a visited set and a hop cap, so cycles and long chains are harmless.

use std::collections::{HashMap, HashSet};

use quick_xml::events::{BytesStart, Event};

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::ParseLimits;
use crate::infra::parsers::ooxml::{Events, xml};

/// Style chain hops followed at most.
const MAX_CHAIN: usize = 20;
/// Paragraph and list levels (`w:ilvl` 0..=8).
pub(super) const LEVELS: usize = 9;

/// `ST_OnOff`: present without a value, or `true`/`1`/`on`, is on.
pub(super) fn on_off(e: &BytesStart<'_>) -> bool {
    xml::attr(e, "w:val").is_none_or(|v| {
        let v = v.trim();
        !(v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off"))
    })
}

/// A level attribute (`w:ilvl`, `w:outlineLvl`): a small number, or `None`.
pub(super) fn level_attr(e: &BytesStart<'_>) -> Option<u8> {
    xml::attr(e, "w:val").and_then(|v| v.trim().parse::<u8>().ok())
}

/// Run toggles a style or run sets; `None` where it says nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Toggles {
    pub(super) bold: Option<bool>,
    pub(super) italic: Option<bool>,
    pub(super) underline: Option<bool>,
    pub(super) strike: Option<bool>,
    pub(super) hidden: Option<bool>,
    /// The font is monospaced.
    pub(super) mono: Option<bool>,
}

impl Toggles {
    /// Reads one run-property element into the toggles; returns whether it
    /// was one.
    pub(super) fn read(&mut self, e: &BytesStart<'_>) -> bool {
        match xml::qname(e) {
            "w:b" => self.bold = Some(on_off(e)),
            "w:i" => self.italic = Some(on_off(e)),
            "w:strike" | "w:dstrike" => {
                // Either one switched on strikes the text through.
                let on = on_off(e);
                self.strike = Some(self.strike.unwrap_or(false) || on);
            }
            "w:u" => {
                self.underline = Some(xml::attr(e, "w:val").is_none_or(|v| {
                    let v = v.trim();
                    v != "none" && v != "0"
                }));
            }
            "w:vanish" | "w:specVanish" => self.hidden = Some(on_off(e)),
            "w:rFonts" => {
                let font = xml::attr(e, "w:ascii").or_else(|| xml::attr(e, "w:hAnsi"));
                if let Some(font) = font {
                    self.mono = Some(is_monospace(&font));
                }
            }
            _ => return false,
        }
        true
    }

    /// `self`, with what it leaves open taken from `base`.
    pub(super) fn over(self, base: Self) -> Self {
        Self {
            bold: self.bold.or(base.bold),
            italic: self.italic.or(base.italic),
            underline: self.underline.or(base.underline),
            strike: self.strike.or(base.strike),
            hidden: self.hidden.or(base.hidden),
            mono: self.mono.or(base.mono),
        }
    }
}

fn is_monospace(font: &str) -> bool {
    let f = font.trim().to_ascii_lowercase();
    [
        "consolas",
        "courier",
        "lucida console",
        "lucida sans typewriter",
        "menlo",
        "monaco",
        "fira code",
        "cascadia",
        "source code",
    ]
    .iter()
    .any(|m| f.contains(m))
        || f.ends_with(" mono")
        || f == "monospace"
}

/// What a style's name says about it, worked out once when styles are read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Named {
    #[default]
    Plain,
    Heading(u8),
    Code,
    Quote,
    /// A character style for code.
    CodeChars,
}

/// Longest style name considered (Word allows 253 characters).
const MAX_NAME: usize = 255;

fn named(name: &str) -> Named {
    let name: String = name
        .trim()
        .chars()
        .take(MAX_NAME)
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(level) = name
        .strip_prefix("heading ")
        .and_then(|n| n.parse::<u8>().ok())
    {
        return heading_for(level.saturating_sub(1)).map_or(Named::Plain, Named::Heading);
    }
    match name.as_str() {
        "title" => Named::Heading(1),
        "html preformatted" | "preformatted text" | "source code" | "code" | "code block"
        | "plain text" => Named::Code,
        "quote" | "intense quote" | "block text" => Named::Quote,
        "html code" | "source text" | "verbatim char" | "html typewriter" | "html keyboard"
        | "code char" | "inline code" => Named::CodeChars,
        _ => Named::Plain,
    }
}

/// What one style sets.
#[derive(Debug, Default)]
struct Style {
    paragraph: bool,
    named: Named,
    based_on: Option<String>,
    outline: Option<u8>,
    num_id: Option<u32>,
    ilvl: Option<u8>,
    page_break_before: Option<bool>,
    toggles: Toggles,
}

/// The styles of a document.
#[derive(Debug, Default)]
pub(super) struct Styles {
    by_id: HashMap<String, Style>,
    default_paragraph: Option<String>,
}

/// The kind of block a paragraph style makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Body,
    Heading(u8),
    Code,
    Quote,
}

impl Styles {
    /// The styles from `id` along its `basedOn` chain, nearest first, within
    /// one style type.
    fn chain<'a>(&'a self, id: Option<&'a str>) -> impl Iterator<Item = &'a Style> + 'a {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut next = id;
        let mut kind: Option<bool> = None;
        std::iter::from_fn(move || {
            let current = next?;
            if !seen.insert(current) || seen.len() > MAX_CHAIN {
                return None;
            }
            let style = self.by_id.get(current)?;
            if kind.is_some_and(|k| k != style.paragraph) {
                return None;
            }
            kind = Some(style.paragraph);
            next = style.based_on.as_deref();
            Some(style)
        })
    }

    /// The paragraph style a paragraph uses: its own, or the default.
    pub(super) fn paragraph_style<'a>(&'a self, id: Option<&'a str>) -> Option<&'a str> {
        id.filter(|id| self.by_id.get(*id).is_some_and(|s| s.paragraph))
            .or(self.default_paragraph.as_deref())
    }

    /// What a paragraph makes of itself: its direct outline level `outline`,
    /// then its style chain nearest first, each style by its outline level or
    /// else its name. An outline level of 9 or more is body text: it rules
    /// out headings further along, not code or quotes.
    pub(super) fn role(&self, given: Option<&str>, outline: Option<u8>) -> Role {
        let id = self.paragraph_style(given);
        let mut headings = true;
        let outlines = std::iter::once((outline, Named::Plain))
            .chain(self.chain(id).map(|s| (s.outline, s.named)));
        for (outline, named) in outlines {
            match outline.map(heading_for) {
                Some(Some(level)) if headings => return Role::Heading(level),
                Some(None) => headings = false,
                _ => {}
            }
            match named {
                Named::Heading(level) if headings => return Role::Heading(level),
                Named::Code => return Role::Code,
                Named::Quote => return Role::Quote,
                Named::Heading(_) | Named::Plain | Named::CodeChars => {}
            }
        }
        if !headings {
            return Role::Body;
        }
        // Last resort: built-in style ids ("Heading2"), even when the
        // styles part does not define them.
        given
            .and_then(|id| id.strip_prefix("Heading"))
            .and_then(|n| n.parse::<u8>().ok())
            .and_then(|n| heading_for(n.saturating_sub(1)))
            .map_or(Role::Body, Role::Heading)
    }

    /// Whether a paragraph style hides its text (`w:vanish` in its run
    /// properties).
    pub(super) fn hidden(&self, id: Option<&str>) -> bool {
        let id = self.paragraph_style(id);
        self.chain(id)
            .find_map(|s| s.toggles.hidden)
            .unwrap_or(false)
    }

    /// The numbering a numbering style (`w:numStyleLink`) stands for.
    pub(super) fn linked_numbering(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).and_then(|s| s.num_id)
    }

    /// The list numbering a paragraph style gives: numbering id and level.
    pub(super) fn numbering(&self, id: Option<&str>) -> (Option<u32>, Option<u8>) {
        let id = self.paragraph_style(id);
        let num = self.chain(id).find_map(|s| s.num_id);
        let ilvl = self.chain(id).find_map(|s| s.ilvl);
        (num, ilvl)
    }

    pub(super) fn page_break_before(&self, id: Option<&str>) -> bool {
        let id = self.paragraph_style(id);
        self.chain(id)
            .find_map(|s| s.page_break_before)
            .unwrap_or(false)
    }

    /// The run toggles a character style gives, and whether its name marks
    /// code.
    pub(super) fn character(&self, id: Option<&str>) -> (Toggles, bool) {
        let mut toggles = Toggles::default();
        let mut code = false;
        for style in self.chain(id) {
            toggles = toggles.over(style.toggles);
            code |= style.named == Named::CodeChars;
        }
        (toggles, code)
    }
}

/// The heading level for an outline level: 0..=8 is a heading (levels past 6
/// read as 6), 9 and more is body text.
pub(super) fn heading_for(outline: u8) -> Option<u8> {
    (usize::from(outline) < LEVELS).then(|| (outline + 1).min(6))
}

/// Which block of a style a style-part element belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum In {
    Other,
    Para,
    Run,
}

pub(super) fn read_styles(text: &str, limits: &ParseLimits) -> Result<Styles, DomainError> {
    let mut events = Events::new(text, limits, "DOCX");
    let mut styles = Styles::default();
    let mut current: Option<(String, Style)> = None;
    let mut part = In::Other;
    let mut skip = 0_usize;
    loop {
        let event = events.next()?;
        let (e, is_start) = match &event {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                match e.name().into_inner() {
                    "w:style" => {
                        if let Some((id, style)) = current.take() {
                            styles.by_id.entry(id).or_insert(style);
                        }
                    }
                    "w:pPr" | "w:rPr" => part = In::Other,
                    _ => {}
                }
                continue;
            }
            Event::Eof => return Ok(styles),
            _ => continue,
        };
        if skip > 0 {
            skip += usize::from(is_start);
            continue;
        }
        let name = xml::qname(e);
        if name.ends_with("Change") {
            skip = usize::from(is_start);
            continue;
        }
        if name == "w:style" {
            let id = xml::attr(e, "w:styleId").unwrap_or_default();
            let paragraph = xml::attr(e, "w:type").is_none_or(|t| t == "paragraph");
            if paragraph && xml::attr(e, "w:default").is_some_and(|d| d == "1" || d == "true") {
                styles.default_paragraph.get_or_insert_with(|| id.clone());
            }
            current = is_start.then(|| {
                (
                    id,
                    Style {
                        paragraph,
                        ..Style::default()
                    },
                )
            });
            continue;
        }
        let Some((_, style)) = current.as_mut() else {
            continue;
        };
        match name {
            "w:name" => style.named = named(&xml::attr(e, "w:val").unwrap_or_default()),
            "w:basedOn" => style.based_on = xml::attr(e, "w:val"),
            "w:pPr" if is_start => part = In::Para,
            "w:rPr" if is_start => part = In::Run,
            "w:outlineLvl" if part == In::Para => style.outline = level_attr(e),
            "w:numId" if part == In::Para => {
                style.num_id = xml::attr(e, "w:val").and_then(|v| v.trim().parse().ok());
            }
            "w:ilvl" if part == In::Para => style.ilvl = level_attr(e),
            "w:pageBreakBefore" if part == In::Para => style.page_break_before = Some(on_off(e)),
            _ if part == In::Run => {
                style.toggles.read(e);
            }
            _ => {}
        }
    }
}

/// What a list level shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListKind {
    Unordered,
    Ordered,
    /// No visible marker: a plain paragraph.
    Plain,
}

/// The definitions of the nine list levels.
type Levels = [Option<LevelDef>; LEVELS];

#[derive(Debug, Default, Clone, Copy)]
struct LevelDef {
    format: Option<ListKind>,
    /// `w:lvlText` is present and empty: nothing is shown.
    no_text: bool,
}

impl LevelDef {
    fn kind(self) -> ListKind {
        if self.no_text {
            return ListKind::Plain;
        }
        // A missing format is decimal.
        self.format.unwrap_or(ListKind::Ordered)
    }
}

/// List numbering: numbering instances and their abstract definitions.
#[derive(Debug, Default)]
pub(super) struct Numbering {
    abstracts: HashMap<u32, Levels>,
    /// abstractNumId -> the numbering style it takes its levels from.
    style_links: HashMap<u32, String>,
    /// numId -> (abstractNumId, level overrides).
    nums: HashMap<u32, (u32, Levels)>,
}

impl Numbering {
    /// The list kind of numbering `num_id` at `level`; `None` when the
    /// numbering does not exist (the paragraph is then plain).
    /// The numbering style a numbering's definition links to
    /// (`w:numStyleLink`), when the definition has no levels of its own.
    pub(super) fn style_link(&self, num_id: u32) -> Option<&str> {
        let (abstract_id, _) = self.nums.get(&num_id)?;
        let levels = self.abstracts.get(abstract_id);
        if levels.is_some_and(|l| l.iter().any(Option::is_some)) {
            return None;
        }
        self.style_links.get(abstract_id).map(String::as_str)
    }

    pub(super) fn kind(&self, num_id: u32, level: u8) -> Option<ListKind> {
        let (abstract_id, overrides) = self.nums.get(&num_id)?;
        let slot = usize::from(level);
        overrides
            .get(slot)
            .copied()
            .flatten()
            .or_else(|| {
                self.abstracts
                    .get(abstract_id)?
                    .get(slot)
                    .copied()
                    .flatten()
            })
            .map(LevelDef::kind)
    }
}

pub(super) fn read_numbering(text: &str, limits: &ParseLimits) -> Result<Numbering, DomainError> {
    let mut events = Events::new(text, limits, "DOCX");
    let mut numbering = Numbering::default();
    // The abstract definition or numbering instance being read.
    let mut abstract_def: Option<(u32, Levels)> = None;
    let mut num: Option<(u32, u32, Levels)> = None;
    let mut level: Option<(usize, LevelDef)> = None;
    let mut override_level: Option<usize> = None;
    loop {
        let event = events.next()?;
        let (e, is_start) = match &event {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                match e.name().into_inner() {
                    "w:abstractNum" => {
                        if let Some((id, levels)) = abstract_def.take() {
                            numbering.abstracts.entry(id).or_insert(levels);
                        }
                    }
                    "w:num" => {
                        if let Some((id, abstract_id, levels)) = num.take() {
                            numbering.nums.entry(id).or_insert((abstract_id, levels));
                        }
                    }
                    "w:lvl" => {
                        if let Some((slot, def)) = level.take() {
                            let target = match (&mut num, &mut abstract_def) {
                                (Some((_, _, levels)), _) | (None, Some((_, levels))) => {
                                    levels.get_mut(slot)
                                }
                                (None, None) => None,
                            };
                            if let Some(target) = target {
                                *target = Some(def);
                            }
                        }
                    }
                    "w:lvlOverride" => override_level = None,
                    _ => {}
                }
                continue;
            }
            Event::Eof => return Ok(numbering),
            _ => continue,
        };
        let id = |attr: &str| xml::attr(e, attr).and_then(|v| v.trim().parse::<u32>().ok());
        match xml::qname(e) {
            "w:abstractNum" if is_start => {
                abstract_def = id("w:abstractNumId").map(|i| (i, [None; LEVELS]));
            }
            "w:num" if is_start => num = id("w:numId").map(|i| (i, u32::MAX, [None; LEVELS])),
            "w:abstractNumId" => {
                if let (Some((_, abstract_id, _)), Some(v)) = (num.as_mut(), id("w:val")) {
                    *abstract_id = v;
                }
            }
            "w:lvlOverride" if is_start => {
                override_level = level_attr_named(e, "w:ilvl");
            }
            "w:lvl" if is_start => {
                // Inside an override, the override's level wins.
                let slot = override_level.or_else(|| level_attr_named(e, "w:ilvl"));
                level = slot.map(|s| (s, LevelDef::default()));
            }
            "w:numStyleLink" => {
                if let (Some((id, _)), Some(link)) = (abstract_def.as_ref(), xml::attr(e, "w:val"))
                {
                    numbering.style_links.entry(*id).or_insert(link);
                }
            }
            "w:numFmt" => {
                if let Some((_, def)) = level.as_mut() {
                    def.format = Some(match xml::attr(e, "w:val").as_deref().map(str::trim) {
                        Some("bullet") => ListKind::Unordered,
                        Some("none") => ListKind::Plain,
                        _ => ListKind::Ordered,
                    });
                }
            }
            "w:lvlText" => {
                if let Some((_, def)) = level.as_mut() {
                    def.no_text = xml::attr(e, "w:val").is_some_and(|v| v.is_empty());
                }
            }
            _ => {}
        }
    }
}

/// A level index from attribute `name`, within 0..=8.
fn level_attr_named(e: &BytesStart<'_>, name: &str) -> Option<usize> {
    xml::attr(e, name)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&l| l < LEVELS)
}
