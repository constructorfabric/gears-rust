//! `PowerPoint` presentations (`.pptx`).
//!
//! Slides are read in presentation order (`p:sldIdLst`). Each slide becomes a
//! level-2 heading (its title, or "Slide N"), then its shapes in `p:spTree`
//! order: text as paragraphs, or list items where a bullet is set on the
//! paragraph or inherited from the layout and master; tables; pictures with
//! alternative text; `SmartArt` text. Slides are separated by page breaks.
//! Speaker notes, comments, charts and slide furniture (date, footer, slide
//! number) are left out.

mod diagram;
mod slide;
mod styles;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use quick_xml::events::Event;

use crate::domain::error::DomainError;
use crate::domain::ir::{DocumentBuilder, Inline, ParsedBlock, ParsedDocument, ParsedSource};
use crate::domain::parser::FileParserBackend;

use super::bounded_read::read_bounded;
use super::limits::ParseLimits;
use super::ooxml::{self, Archive, Events, Relationships, xml};
use slide::{Budgets, SlideContext, read_slide};
use styles::{Inherited, PartStyles, read_part_styles};

/// Ceiling applied by [`PptxParser::new`], matching `FileParserConfig`'s
/// `max_file_size_mb` default; `Gear::init` passes the configured value.
const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

const SUPPORTED_EXTENSIONS: &[&str] = &["pptx"];

const CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";

/// PPTX parser.
pub struct PptxParser {
    max_bytes: u64,
    limits: ParseLimits,
}

impl PptxParser {
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
        source: ParsedSource,
        filename: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let limits = self.limits.clone();
        let deck = tokio::task::spawn_blocking(move || read_deck(bytes, &limits))
            .await
            .map_err(|e| DomainError::parse_error(format!("PPTX task failed: {e}")))??;

        let mut builder = DocumentBuilder::new(source)
            .content_type(CONTENT_TYPE)
            .blocks(deck.blocks);
        if let Some(title) = deck.title.as_deref().or(filename) {
            builder = builder.title(title);
        }
        if let Some(filename) = filename {
            builder = builder.original_filename(filename);
        }
        Ok(builder.build())
    }
}

impl Default for PptxParser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FileParserBackend for PptxParser {
    fn id(&self) -> &'static str {
        "pptx"
    }

    fn supported_extensions(&self) -> &'static [&'static str] {
        SUPPORTED_EXTENSIONS
    }

    async fn parse_local_path(
        &self,
        path: &Path,
        _resolved_content_type: Option<&str>,
    ) -> Result<ParsedDocument, DomainError> {
        let bytes = read_bounded(path, self.max_bytes).await?;
        let filename = path.file_name().and_then(|s| s.to_str());
        let source = ParsedSource::LocalPath(path.display().to_string());
        self.parse(Bytes::from(bytes), source, filename).await
    }

    async fn parse_bytes(
        &self,
        filename_hint: Option<&str>,
        _content_type: Option<&str>,
        bytes: Bytes,
    ) -> Result<ParsedDocument, DomainError> {
        let source = ParsedSource::Uploaded {
            original_name: filename_hint.unwrap_or("unknown").to_owned(),
        };
        self.parse(bytes, source, filename_hint).await
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
    let presentation = root
        .of_type("officeDocument")
        .find(|r| !r.external && archive.contains(&r.target))
        .map(|r| r.target.clone())
        .or_else(|| {
            archive
                .contains("ppt/presentation.xml")
                .then(|| "ppt/presentation.xml".to_owned())
        })
        .ok_or_else(|| parse_error("no presentation part"))?;
    let core = root
        .of_type("core-properties")
        .find(|r| !r.external)
        .map(|r| r.target.clone())
        .or_else(|| {
            archive
                .contains("docProps/core.xml")
                .then(|| "docProps/core.xml".to_owned())
        });
    let title = match core {
        Some(part) => ooxml::core_title(&mut archive, &part)?,
        None => None,
    };

    let presentation_xml = archive
        .read_xml(&presentation)?
        .ok_or_else(|| parse_error("no presentation part"))?;
    let slide_ids = tolerate(slide_ids(&presentation_xml, limits), "presentation")?;
    drop(presentation_xml);
    let rels = archive.read_rels(&presentation)?;
    let slides = slide_parts(&rels, &slide_ids);

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
            blocks.push(ParsedBlock::PageBreak);
        }
        let heading = if slide.title.is_empty() {
            format!("Slide {}", position + 1)
        } else {
            slide.title
        };
        // Headings count as text, so empty slides cannot add output for free.
        budgets.text.charge(heading.len())?;
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
    let mut in_list = false;
    loop {
        match events.next()? {
            Event::Start(e) if xml::qname(&e) == "p:sldIdLst" => in_list = true,
            Event::End(e) if e.name().into_inner() == "p:sldIdLst" => in_list = false,
            Event::Start(e) | Event::Empty(e) if in_list && xml::qname(&e) == "p:sldId" => {
                if let Some(id) = xml::prefixed_attr(&e, "id") {
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
        .filter(|(_, rel)| !rel.external && rel.rel_type.rsplit('/').next() == Some("slide"))
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
        let Some(layout) = internal(slide_rels, "slideLayout") else {
            return Ok(Inherited::default());
        };
        let key = layout.to_ascii_lowercase();
        let master = if let Some(master) = self.masters.get(&key) {
            master.clone()
        } else {
            let rels = tolerate(archive.read_rels(&layout), "layout relationships")?;
            let master = internal(&rels, "slideMaster");
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

fn internal(rels: &Relationships, kind: &str) -> Option<String> {
    rels.of_type(kind)
        .find(|r| !r.external)
        .map(|r| r.target.clone())
}
