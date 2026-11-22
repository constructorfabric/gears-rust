//! Placeholders and the bullet styles a slide inherits from its layout and
//! master.
//!
//! A paragraph's bullet is the first one set along this chain: the
//! paragraph's own `a:pPr`, its shape's `a:lstStyle`, the matching layout
//! placeholder, the matching master placeholder, then the master's
//! `p:titleStyle` / `p:bodyStyle` / `p:otherStyle`. Only bullets are read;
//! fonts, sizes and colours do not reach the output.

use std::collections::HashMap;
use std::rc::Rc;

use quick_xml::events::{BytesStart, Event};

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::ParseLimits;
use crate::infra::parsers::ooxml::{Events, xml};

/// Paragraph levels a list style can describe (`a:lvl1pPr` .. `a:lvl9pPr`).
pub(super) const LEVELS: usize = 9;

/// The bullet setting of one paragraph level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bullet {
    /// `a:buNone`: explicitly no bullet.
    None,
    /// `a:buChar` or `a:buBlip`.
    Unordered,
    /// `a:buAutoNum`.
    Ordered,
}

impl Bullet {
    /// The bullet an element of a paragraph-properties element sets.
    pub(super) fn from_element(name: &str) -> Option<Self> {
        match name {
            "a:buNone" => Some(Self::None),
            "a:buChar" | "a:buBlip" => Some(Self::Unordered),
            "a:buAutoNum" => Some(Self::Ordered),
            _ => None,
        }
    }
}

/// Bullets by paragraph level; a level that sets none inherits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ListStyle([Option<Bullet>; LEVELS]);

impl ListStyle {
    pub(super) fn get(&self, level: usize) -> Option<Bullet> {
        self.0.get(level).copied().flatten()
    }

    pub(super) fn set(&mut self, level: usize, bullet: Bullet) {
        if let Some(slot) = self.0.get_mut(level) {
            *slot = Some(bullet);
        }
    }

    /// The level an `a:lvlNpPr` element describes (`a:lvl1pPr` -> 0).
    pub(super) fn level_of(name: &str) -> Option<usize> {
        let digit = name.strip_prefix("a:lvl")?.strip_suffix("pPr")?;
        match digit.as_bytes() {
            [d @ b'1'..=b'9'] => Some(usize::from(d - b'1')),
            _ => None,
        }
    }
}

/// What a placeholder holds (`<p:ph type="..">`), grouped by how it is
/// matched and styled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    /// `title`, `ctrTitle`.
    Title,
    /// `subTitle`.
    SubTitle,
    /// `body`, `obj` (the default) and content placeholders (`chart`, `tbl`,
    /// `pic`, ...).
    Body,
    /// `ftr`, `dt`, `sldNum`, `hdr`: footer, date, slide number and header,
    /// left out (the same on every slide).
    Furniture,
}

impl Kind {
    fn from_type(value: Option<&str>) -> Self {
        match value {
            Some("title" | "ctrTitle") => Self::Title,
            Some("subTitle") => Self::SubTitle,
            Some("ftr" | "dt" | "sldNum" | "hdr") => Self::Furniture,
            _ => Self::Body,
        }
    }

    /// The kind of master placeholder a layout placeholder of this kind
    /// inherits from: masters have no subtitle placeholder.
    fn on_master(self) -> Self {
        match self {
            Self::SubTitle => Self::Body,
            other => other,
        }
    }
}

/// A placeholder's identity: its kind and its index on the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placeholder {
    pub(super) kind: Kind,
    /// `idx`; absent (or 0) on title placeholders.
    pub(super) idx: Option<u32>,
    /// Whether `type` was written. A placeholder with only an `idx` matches
    /// its layout counterpart by index alone.
    typed: bool,
}

impl Placeholder {
    /// The placeholder `<p:ph>` describes.
    pub(super) fn from_element(e: &BytesStart<'_>) -> Self {
        let kind_attr = xml::attr(e, "type");
        Self {
            kind: Kind::from_type(kind_attr.as_deref()),
            idx: xml::attr(e, "idx")
                .and_then(|v| v.trim().parse().ok())
                .filter(|&idx| idx != 0),
            typed: kind_attr.is_some(),
        }
    }
}

/// The placeholders of a layout or master, with their list styles, indexed
/// so a lookup costs the same however many placeholders a part declares.
/// The first placeholder with a given index or kind wins.
#[derive(Debug, Default)]
pub(super) struct Placeholders {
    by_idx: HashMap<u32, (Kind, ListStyle)>,
    by_kind: HashMap<Kind, ListStyle>,
}

impl Placeholders {
    pub(super) fn insert(&mut self, ph: Placeholder, style: ListStyle) {
        if let Some(idx) = ph.idx {
            self.by_idx.entry(idx).or_insert((ph.kind, style));
        }
        self.by_kind.entry(ph.kind).or_insert(style);
    }

    /// The layout placeholder a slide placeholder `ph` inherits from: same
    /// index first, then same kind. Returns its kind and list style.
    pub(super) fn find(&self, ph: &Placeholder) -> Option<(Kind, &ListStyle)> {
        let by_idx = ph
            .idx
            .and_then(|idx| self.by_idx.get(&idx))
            .map(|(kind, style)| (*kind, style));
        by_idx.or_else(|| {
            (ph.typed || ph.idx.is_none())
                .then(|| self.find_kind(ph.kind).map(|style| (ph.kind, style)))
                .flatten()
        })
    }

    /// The placeholder of `kind`; masters are matched by kind only.
    pub(super) fn find_kind(&self, kind: Kind) -> Option<&ListStyle> {
        self.by_kind.get(&kind)
    }
}

/// What a slide inherits from its layout and master.
#[derive(Debug, Default)]
pub(super) struct Inherited {
    pub(super) layout: Rc<PartStyles>,
    pub(super) master: Rc<PartStyles>,
}

impl Inherited {
    /// What placeholder `ph` holds: its own `type`, or, when it has only an
    /// index, the kind of the layout placeholder it inherits from.
    pub(super) fn kind(&self, ph: &Placeholder) -> Kind {
        if ph.typed {
            return ph.kind;
        }
        self.layout
            .placeholders
            .find(ph)
            .map_or(ph.kind, |(kind, _)| kind)
    }

    /// The inherited bullet for a paragraph at `level` in a shape that is
    /// placeholder `ph` (or no placeholder).
    pub(super) fn bullet(&self, ph: Option<&Placeholder>, level: usize) -> Option<Bullet> {
        let text = &self.master.text;
        let Some(ph) = ph else {
            return text.other.get(level);
        };
        let layout = self.layout.placeholders.find(ph);
        // The layout placeholder's kind decides the master placeholder: a
        // slide placeholder often carries only an index.
        let kind = layout.map_or(ph.kind, |(kind, _)| kind);
        layout
            .and_then(|(_, style)| style.get(level))
            .or_else(|| {
                self.master
                    .placeholders
                    .find_kind(kind.on_master())
                    .and_then(|style| style.get(level))
            })
            .or_else(|| match kind {
                Kind::Title => text.title.get(level),
                Kind::Body | Kind::SubTitle => text.body.get(level),
                Kind::Furniture => text.other.get(level),
            })
    }
}

/// The master's `p:txStyles`.
#[derive(Debug, Default)]
pub(super) struct TextStyles {
    pub(super) title: ListStyle,
    pub(super) body: ListStyle,
    pub(super) other: ListStyle,
}

/// Placeholders and text styles read from a layout or master part.
#[derive(Debug, Default)]
pub(super) struct PartStyles {
    pub(super) placeholders: Placeholders,
    pub(super) text: TextStyles,
}

/// Reads the placeholders (with their `a:lstStyle`) and, on a master, the
/// `p:txStyles` of a layout or master part. Placeholder prompt text ("Click
/// to edit Master title style") is never read.
pub(super) fn read_part_styles(
    text: &str,
    limits: &ParseLimits,
) -> Result<PartStyles, DomainError> {
    let mut events = Events::new(text, limits, "PPTX");
    let mut out = PartStyles::default();
    // Placeholder of the shape being read, once its `p:ph` is seen.
    let mut shape: Option<Option<Placeholder>> = None;
    let mut shape_style = ListStyle::default();
    // The list style being filled and the level element it is inside.
    let mut target: Option<Target> = None;
    let mut level: Option<usize> = None;
    loop {
        let event = events.next()?;
        let (e, is_start) = match &event {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) => {
                match e.name().into_inner() {
                    "p:sp" => {
                        if let Some(Some(ph)) = shape.take() {
                            out.placeholders.insert(ph, shape_style);
                        }
                        shape = None;
                    }
                    "a:lstStyle" | "p:titleStyle" | "p:bodyStyle" | "p:otherStyle" => {
                        target = None;
                    }
                    name if ListStyle::level_of(name).is_some() => level = None,
                    _ => {}
                }
                continue;
            }
            Event::Eof => return Ok(out),
            _ => continue,
        };
        let name = xml::qname(e);
        match name {
            "p:sp" if is_start => {
                shape = Some(None);
                shape_style = ListStyle::default();
            }
            "p:ph" => {
                if let Some(ph) = shape.as_mut() {
                    *ph = Some(Placeholder::from_element(e));
                }
            }
            "a:lstStyle" if is_start && shape.is_some() => target = Some(Target::Shape),
            "p:titleStyle" if is_start => target = Some(Target::Title),
            "p:bodyStyle" if is_start => target = Some(Target::Body),
            "p:otherStyle" if is_start => target = Some(Target::Other),
            _ if target.is_some() => {
                if let Some(l) = ListStyle::level_of(name) {
                    level = is_start.then_some(l);
                } else if let (Some(l), Some(bullet)) = (level, Bullet::from_element(name)) {
                    let style = match target {
                        Some(Target::Shape) => &mut shape_style,
                        Some(Target::Title) => &mut out.text.title,
                        Some(Target::Body) => &mut out.text.body,
                        Some(Target::Other) | None => &mut out.text.other,
                    };
                    style.set(l, bullet);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Target {
    Shape,
    Title,
    Body,
    Other,
}
