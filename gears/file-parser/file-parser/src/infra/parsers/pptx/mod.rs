//! `PowerPoint` presentations (`.pptx`).
//!
//! Slides are read in presentation order (`p:sldIdLst`). Each slide becomes a
//! level-2 heading (its title, or "Slide N"), then its shapes in `p:spTree`
//! order: text as paragraphs, or list items where a bullet is set on the
//! paragraph or inherited from the layout and master; tables; pictures with
//! alternative text; `SmartArt` text. Slides are separated by page breaks.
//! Speaker notes, comments, charts and slide furniture (footer, date and
//! slide-number placeholders) are left out.

mod diagram;
mod slide;
mod styles;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use bytes::Bytes;
use quick_xml::events::Event;

use crate::domain::error::{DomainError, ParseLimit};
use crate::domain::ir::{Inline, ParsedBlock};
use crate::domain::mime_table;

use super::backend::{DocumentFormat, DocumentParser, ReadDocument, run_blocking};
use super::limits::{Budgets, ParseLimits, limit_exceeded};
use super::ooxml::{self, Archive, Events, Relationships, xml};
use slide::{SlideContext, read_slide};
use styles::{Inherited, PartStyles, read_part_styles};

/// PPTX: slides and their shapes.
pub struct Pptx;

/// PPTX parser.
pub type PptxParser = DocumentParser<Pptx>;

impl DocumentFormat for Pptx {
    const ID: &'static str = "pptx";
    const EXTENSIONS: &'static [&'static str] = &["pptx"];

    async fn read(
        bytes: Bytes,
        _content_type: Option<String>,
        limits: ParseLimits,
    ) -> Result<ReadDocument, DomainError> {
        let deck = run_blocking("PPTX", move || read_deck(bytes, &limits)).await?;
        Ok(ReadDocument::new(mime_table::PPTX, deck.title, deck.blocks))
    }
}

fn parse_error(detail: impl std::fmt::Display) -> DomainError {
    DomainError::parse_error(format!("Failed to parse PPTX: {detail}"))
}

struct Deck {
    title: Option<String>,
    blocks: Vec<ParsedBlock>,
}

fn read_deck(bytes: Bytes, limits: &ParseLimits) -> Result<Deck, DomainError> {
    let mut archive = Archive::open(bytes, "PPTX", limits)?;
    let root = archive.read_rels("")?;
    let presentation = ooxml::main_part(&archive, &root, &["ppt/presentation.xml"])
        .ok_or_else(|| parse_error("no presentation part"))?;
    let title = ooxml::package_title(&mut archive, &root)?;

    let presentation_xml = archive
        .read_xml(&presentation)?
        .ok_or_else(|| parse_error("no presentation part"))?;
    let slide_ids = tolerate(slide_ids(&presentation_xml, limits), "presentation")?;
    drop(presentation_xml);
    let rels = archive.read_rels(&presentation)?;
    let slides = slide_parts(&rels, &slide_ids);
    if slides.len() > limits.max_sheets {
        return Err(limit_exceeded(
            ParseLimit::MaxSheets,
            format!("{} slides, limit {}", slides.len(), limits.max_sheets),
        ));
    }

    let mut styles = StyleCache::default();
    let mut budgets = Budgets::new(limits);
    let mut blocks = Vec::new();
    for (position, part) in &slides {
        let Some(text) = tolerate(archive.read_xml(part), "slide")? else {
            continue;
        };
        let slide_rels = tolerate(archive.read_rels(part), "slide relationships")?;
        let inherited = styles.inherited(&mut archive, &slide_rels)?;
        let ctx = SlideContext {
            inherited: &inherited,
            rels: &slide_rels,
            limits,
        };
        let mut slide = tolerate(read_slide(&text, &ctx, &mut budgets), "slide")?;
        drop(text);
        diagram::splice(&mut archive, &mut slide, &mut budgets)?;

        if !blocks.is_empty() {
            budgets.text.charge_block()?;
            blocks.push(ParsedBlock::PageBreak);
        }
        let heading = if slide.title.is_empty() {
            format!("Slide {}", position + 1)
        } else {
            slide.title
        };
        // Headings count as text, so empty slides cannot add output for free.
        budgets.text.charge(heading.len())?;
        budgets.text.charge_block()?;
        blocks.push(ParsedBlock::Heading {
            level: 2,
            inlines: vec![Inline::plain(heading)],
        });
        blocks.append(&mut slide.blocks);
    }
    Ok(Deck { title, blocks })
}

/// Relationship ids of the slides, in `p:sldIdLst` order.
fn slide_ids(text: &str, limits: &ParseLimits) -> Result<Vec<String>, DomainError> {
    let mut events = Events::new(text, limits, "PPTX");
    let mut ids = Vec::new();
    let max_ids = limits.max_sheets.saturating_mul(2);
    let mut in_list = false;
    loop {
        match events.next()? {
            Event::Start(e) if xml::qname(&e) == "p:sldIdLst" => in_list = true,
            // Nothing after the list is needed, so a later error cannot lose it.
            Event::End(e) if e.name().into_inner() == "p:sldIdLst" => return Ok(ids),
            Event::Start(e) | Event::Empty(e) if in_list && xml::qname(&e) == "p:sldId" => {
                if let Some(id) = xml::prefixed_attr(&e, "id") {
                    // Duplicate and dangling ids are dropped later, so the
                    // list may run somewhat past the slide limit, not to
                    // millions of entries.
                    if ids.len() >= max_ids {
                        return Err(limit_exceeded(
                            ParseLimit::MaxSheets,
                            format!("slide list longer than {max_ids} entries"),
                        ));
                    }
                    ids.push(id);
                }
            }
            Event::Eof => return Ok(ids),
            _ => {}
        }
    }
}

/// The slide parts to read, in order, each once, with their position in the
/// slide list ("Slide N" uses it). Part names compare ASCII
/// case-insensitively, as the archive looks them up. A presentation without
/// a usable slide list falls back to its slide relationships in document
/// order.
fn slide_parts(rels: &Relationships, ids: &[String]) -> Vec<(usize, String)> {
    let mut seen = HashSet::new();
    let mut fresh = |target: &str| seen.insert(target.to_ascii_lowercase());
    let listed: Vec<(usize, String)> = ids
        .iter()
        .enumerate()
        .filter_map(|(position, id)| rels.get(id).map(|rel| (position, rel)))
        .filter(|(_, rel)| !rel.external && rel.is_type("slide"))
        .filter(|(_, rel)| fresh(&rel.target))
        .map(|(position, rel)| (position, rel.target.clone()))
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    rels.of_type("slide")
        .filter(|rel| !rel.external && fresh(&rel.target))
        .map(|rel| rel.target.clone())
        .enumerate()
        .collect()
}

/// `result`, or an empty value when an optional part is malformed.
pub(super) fn tolerate<T: Default>(
    result: Result<T, DomainError>,
    what: &str,
) -> Result<T, DomainError> {
    ooxml::tolerate(result, "PPTX", what)
}

/// Layout and master styles, each part read once however many slides use it.
#[derive(Default)]
struct StyleCache {
    parts: HashMap<String, Rc<PartStyles>>,
    /// Layout part -> its master part.
    masters: HashMap<String, Option<String>>,
}

impl StyleCache {
    fn inherited(
        &mut self,
        archive: &mut Archive,
        slide_rels: &Relationships,
    ) -> Result<Inherited, DomainError> {
        let Some(layout) = slide_rels.internal("slideLayout") else {
            return Ok(Inherited::default());
        };
        let key = layout.to_ascii_lowercase();
        let master = if let Some(master) = self.masters.get(&key) {
            master.clone()
        } else {
            let rels = tolerate(archive.read_rels(&layout), "layout relationships")?;
            let master = rels.internal("slideMaster");
            self.masters.insert(key, master.clone());
            master
        };
        Ok(Inherited {
            layout: self.part(archive, &layout)?,
            master: match master {
                Some(master) => self.part(archive, &master)?,
                None => Rc::default(),
            },
        })
    }

    fn part(&mut self, archive: &mut Archive, part: &str) -> Result<Rc<PartStyles>, DomainError> {
        let key = part.to_ascii_lowercase();
        if let Some(styles) = self.parts.get(&key) {
            return Ok(Rc::clone(styles));
        }
        let styles = match tolerate(archive.read_xml(part), "layout")? {
            Some(text) => Rc::new(tolerate(
                read_part_styles(&text, archive.limits()),
                "layout",
            )?),
            None => Rc::default(),
        };
        self.parts.insert(key, Rc::clone(&styles));
        Ok(styles)
    }
}
