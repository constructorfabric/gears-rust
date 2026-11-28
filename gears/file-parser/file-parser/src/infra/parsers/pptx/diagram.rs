//! `SmartArt` text.
//!
//! A `SmartArt` graphic frame on a slide points (`dgm:relIds r:dm`) at a data
//! part whose `dgm:pt` points hold the text the user typed. Each paragraph of
//! a content point becomes a paragraph, in data-part order. Layout, style and
//! colour parts hold no user text and are not read.

use std::collections::HashSet;

use quick_xml::events::Event;

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, ParsedBlock};
use crate::infra::parsers::limits::ParseLimits;
use crate::infra::parsers::ooxml::{Archive, Events, xml};

use super::slide::{Slide, clean_text};
use super::tolerate;
use crate::infra::parsers::limits::Budgets;

/// Reads the data parts `slide` refers to and puts their text where the
/// graphic frames were. Each part is read once per slide.
pub(super) fn splice(
    archive: &mut Archive,
    slide: &mut Slide,
    budgets: &mut Budgets,
) -> Result<(), DomainError> {
    if slide.diagrams.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for (at, part) in std::mem::take(&mut slide.diagrams) {
        if !seen.insert(part.to_ascii_lowercase()) {
            continue;
        }
        let Some(text) = tolerate(archive.read_xml(&part), "SmartArt")? else {
            continue;
        };
        let limits = archive.limits().clone();
        let blocks = tolerate(read_data(&text, &limits, budgets), "SmartArt")?;
        if !blocks.is_empty() {
            inserts.push((at, blocks));
        }
    }
    if inserts.is_empty() {
        return Ok(());
    }
    // One pass; the insert points are in ascending order.
    let old = std::mem::take(&mut slide.blocks);
    let mut old = old.into_iter();
    let mut taken = 0;
    for (at, blocks) in inserts {
        slide
            .blocks
            .extend(old.by_ref().take(at.saturating_sub(taken)));
        taken = taken.max(at);
        slide.blocks.extend(blocks);
    }
    slide.blocks.extend(old);
    Ok(())
}

/// The paragraphs of the content points (`node`, `asst`) of a data part.
fn read_data(
    text: &str,
    limits: &ParseLimits,
    budgets: &mut Budgets,
) -> Result<Vec<ParsedBlock>, DomainError> {
    let mut events = Events::new(text, limits, "PPTX");
    let mut blocks = Vec::new();
    // Inside a content point, its text body, a paragraph and `a:t`.
    let mut in_point = false;
    let mut in_body = false;
    let mut para: Option<String> = None;
    let mut in_text = false;
    loop {
        match events.next()? {
            Event::Start(e) => match xml::qname(&e) {
                "a:br" if para.is_some() => {
                    if let Some(p) = para.as_mut() {
                        budgets.text.charge(1)?;
                        p.push('\n');
                    }
                }
                "dgm:pt" => {
                    in_point =
                        xml::attr(&e, "type").is_none_or(|t| matches!(t.trim(), "node" | "asst"));
                }
                "dgm:t" if in_point => in_body = true,
                "a:p" if in_body => para = Some(String::new()),
                "a:t" if para.is_some() => in_text = true,
                _ => {}
            },
            Event::Empty(e) if para.is_some() && xml::qname(&e) == "a:br" => {
                if let Some(p) = para.as_mut() {
                    budgets.text.charge(1)?;
                    p.push('\n');
                }
            }
            Event::End(e) => match e.name().into_inner() {
                "dgm:pt" => in_point = false,
                "dgm:t" => in_body = false,
                "a:t" => in_text = false,
                "a:p" => {
                    in_text = false;
                    if let Some(p) = para.take() {
                        let p = p.trim();
                        if !p.is_empty() {
                            budgets.text.charge_block()?;
                            blocks.push(ParsedBlock::Paragraph {
                                inlines: vec![Inline::plain(p)],
                            });
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_text => {
                if let Some(p) = para.as_mut() {
                    let t = t.xml10_content();
                    budgets.text.charge(t.len())?;
                    p.push_str(&clean_text(&t));
                }
            }
            Event::GeneralRef(r) if in_text => {
                if let (Some(p), Some(t)) = (para.as_mut(), xml::resolve_ref(&r)) {
                    budgets.text.charge(t.len())?;
                    p.push_str(&clean_text(&t));
                }
            }
            Event::Eof => return Ok(blocks),
            _ => {}
        }
    }
}
