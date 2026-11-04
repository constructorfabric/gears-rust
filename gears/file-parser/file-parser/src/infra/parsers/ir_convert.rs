//! Utilities for converting Kreuzberg extraction results into our IR types.

use kreuzberg::ExtractionResult;
use kreuzberg::types::document_structure::{DocumentNode, DocumentStructure, NodeContent};

use crate::domain::ir::{Inline, ParsedBlock, TableBlock, TableCell, TableRow};

/// Convert a Kreuzberg `ExtractionResult` into a flat list of `ParsedBlock`s.
///
/// Prefers the rich `DocumentStructure` when available; falls back to splitting
/// the plain-text `content` into paragraphs. If the `DocumentStructure` exists
/// but produces zero blocks (e.g. all nodes are metadata-only), the plain-text
/// fallback is used so content is never silently dropped.
#[must_use]
pub fn result_to_blocks(result: &ExtractionResult) -> Vec<ParsedBlock> {
    let mut blocks = if let Some(doc) = &result.document {
        let blocks = document_structure_to_blocks(doc);
        if blocks.is_empty() {
            text_to_blocks(&result.content)
        } else {
            blocks
        }
    } else {
        text_to_blocks(&result.content)
    };

    // Append any tables from result.tables that are not already represented as
    // ParsedBlock::Table nodes in the document structure.
    if !result.tables.is_empty() && !blocks.iter().any(|b| matches!(b, ParsedBlock::Table(_))) {
        blocks.extend(result.tables.iter().filter_map(kreuzberg_table_to_block));
    }

    blocks
}

/// Walk only the body-layer root nodes of a `DocumentStructure` and emit IR blocks.
#[must_use]
pub fn document_structure_to_blocks(doc: &DocumentStructure) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    for (_idx, node) in doc.body_roots() {
        collect_blocks_from_node(node, doc, &mut blocks);
    }
    blocks
}

/// Clamp a heading level to the valid IR range (1–6).
fn clamp_heading_level(level: u8) -> u8 {
    level.clamp(1, 6)
}

/// Emit a non-empty trimmed text string as a `ParsedBlock::Paragraph`.
fn push_paragraph(text: &str, out: &mut Vec<ParsedBlock>) {
    let t = text.trim();
    if !t.is_empty() {
        out.push(ParsedBlock::Paragraph {
            inlines: vec![Inline::plain(t)],
        });
    }
}

/// Recursively convert a single `DocumentNode` (and any children) into IR blocks.
fn collect_blocks_from_node(
    node: &DocumentNode,
    doc: &DocumentStructure,
    out: &mut Vec<ParsedBlock>,
) {
    match &node.content {
        NodeContent::Title { text } => {
            let t = text.trim();
            if !t.is_empty() {
                out.push(ParsedBlock::Heading {
                    level: 1,
                    inlines: vec![Inline::plain(t)],
                });
            }
        }
        NodeContent::Heading { level, text } => {
            let t = text.trim();
            if !t.is_empty() {
                out.push(ParsedBlock::Heading {
                    level: clamp_heading_level(*level),
                    inlines: vec![Inline::plain(t)],
                });
            }
        }
        // Plain text nodes — all emit as paragraphs
        NodeContent::Paragraph { text }
        | NodeContent::Formula { text }
        | NodeContent::Footnote { text }
        | NodeContent::Citation { text, .. }
        | NodeContent::RawBlock { content: text, .. } => {
            push_paragraph(text, out);
        }
        NodeContent::List { ordered } => {
            for child_idx in &node.children {
                if let Some(child) = doc.get(*child_idx) {
                    collect_list_item(child, doc, *ordered, 0, out);
                }
            }
        }
        // Bare ListItem outside a List container — treat as unordered, level 0
        NodeContent::ListItem { text } => {
            let t = text.trim();
            if !t.is_empty() {
                out.push(ParsedBlock::ListItem {
                    level: 0,
                    ordered: false,
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain(t)],
                    }],
                });
            }
        }
        NodeContent::Table { grid } => collect_table_node(grid, out),
        NodeContent::Code { text, language } => {
            if !text.is_empty() {
                out.push(ParsedBlock::CodeBlock {
                    language: language.clone(),
                    code: text.clone(),
                });
            }
        }
        // Quote and Admonition both map to a block-quote container
        NodeContent::Quote | NodeContent::Admonition { .. } => {
            let mut inner = Vec::new();
            for child_idx in &node.children {
                if let Some(child) = doc.get(*child_idx) {
                    collect_blocks_from_node(child, doc, &mut inner);
                }
            }
            if !inner.is_empty() {
                out.push(ParsedBlock::Quote { blocks: inner });
            }
        }
        NodeContent::PageBreak => {
            out.push(ParsedBlock::PageBreak);
        }
        NodeContent::Image {
            description, src, ..
        } => {
            // Images without an addressable source (e.g. embedded DOCX images) would
            // render as an empty `![]()` link; drop them, matching the pre-kreuzberg
            // DOCX behaviour of dropping embedded images.
            if src.as_deref().is_some_and(|s| !s.trim().is_empty()) {
                out.push(ParsedBlock::Image {
                    alt: description.clone(),
                    title: None,
                    src: src.clone(),
                });
            } else {
                tracing::debug!(
                    has_description = description.is_some(),
                    "dropping image node without a source"
                );
            }
        }
        // Container nodes: slide, group — recurse into children
        NodeContent::Slide { title, number } => {
            collect_slide_node(title.as_deref(), *number, node, doc, out);
        }
        NodeContent::Group {
            heading_level,
            heading_text,
            ..
        } => collect_group_node(*heading_level, heading_text.as_deref(), node, doc, out),
        NodeContent::DefinitionItem { term, definition } => {
            out.push(ParsedBlock::Paragraph {
                inlines: vec![Inline::plain(format!("{term}: {definition}"))],
            });
        }
        // Metadata blocks — skip (not relevant for content IR)
        NodeContent::MetadataBlock { .. } | NodeContent::DefinitionList => {}
    }
}

/// Convert a `NodeContent::Table` grid into a `ParsedBlock::Table`.
fn collect_table_node(
    grid: &kreuzberg::types::document_structure::TableGrid,
    out: &mut Vec<ParsedBlock>,
) {
    let rows_count = grid.rows as usize;
    let cols_count = grid.cols as usize;

    if rows_count == 0 || cols_count == 0 {
        return;
    }

    // Build a 2-D matrix initialised with empty strings, then fill from the flat
    // cells vec (which is in row-major order but may have spans).
    let mut matrix: Vec<Vec<String>> = vec![vec![String::new(); cols_count]; rows_count];
    let mut header_rows: Vec<bool> = vec![false; rows_count];

    for cell in &grid.cells {
        let r = cell.row as usize;
        let c = cell.col as usize;
        if r < rows_count && c < cols_count {
            matrix[r][c].clone_from(&cell.content);
            if cell.is_header {
                header_rows[r] = true;
            }
        }
    }

    let rows = matrix
        .into_iter()
        .enumerate()
        .map(|(row_idx, row_cells)| {
            let is_header = header_rows[row_idx] || row_idx == 0;
            let cells = row_cells
                .into_iter()
                .map(|text| TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain(text)],
                    }],
                })
                .collect();
            TableRow { is_header, cells }
        })
        .collect();

    out.push(ParsedBlock::Table(TableBlock { rows }));
}

/// Emit a heading for a PPTX slide then recurse into its children.
fn collect_slide_node(
    title: Option<&str>,
    number: u32,
    node: &DocumentNode,
    doc: &DocumentStructure,
    out: &mut Vec<ParsedBlock>,
) {
    let slide_title = title
        .filter(|t| !t.is_empty())
        .map_or_else(|| format!("Slide {number}"), str::to_owned);
    out.push(ParsedBlock::Heading {
        level: 2,
        inlines: vec![Inline::plain(slide_title)],
    });
    for child_idx in &node.children {
        if let Some(child) = doc.get(*child_idx) {
            collect_blocks_from_node(child, doc, out);
        }
    }
}

/// Emit an optional heading for a grouped section then recurse into children.
///
/// The group heading is skipped when the first child is a `Heading`/`Title`
/// node carrying the same text (as DOCX produces), so it is emitted only once.
fn collect_group_node(
    heading_level: Option<u8>,
    heading_text: Option<&str>,
    node: &DocumentNode,
    doc: &DocumentStructure,
    out: &mut Vec<ParsedBlock>,
) {
    if let (Some(lvl), Some(txt)) = (heading_level, heading_text) {
        let t = txt.trim();
        let first_child_repeats = node
            .children
            .first()
            .and_then(|idx| doc.get(*idx))
            .is_some_and(|child| match &child.content {
                NodeContent::Heading { text, .. } | NodeContent::Title { text } => text.trim() == t,
                _ => false,
            });
        if !t.is_empty() && !first_child_repeats {
            out.push(ParsedBlock::Heading {
                level: clamp_heading_level(lvl),
                inlines: vec![Inline::plain(t)],
            });
        }
    }
    for child_idx in &node.children {
        if let Some(child) = doc.get(*child_idx) {
            collect_blocks_from_node(child, doc, out);
        }
    }
}

/// Recursively collect list items from a `ListItem` node, handling nested lists.
fn collect_list_item(
    node: &DocumentNode,
    doc: &DocumentStructure,
    ordered: bool,
    level: u8,
    out: &mut Vec<ParsedBlock>,
) {
    if let NodeContent::ListItem { text } = &node.content {
        let t = text.trim();
        if !t.is_empty() {
            out.push(ParsedBlock::ListItem {
                level,
                ordered,
                blocks: vec![ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain(t)],
                }],
            });
        }
    }

    // Handle nested lists that are children of this item
    for child_idx in &node.children {
        if let Some(child) = doc.get(*child_idx)
            && let NodeContent::List {
                ordered: inner_ordered,
            } = &child.content
        {
            for inner_idx in &child.children {
                if let Some(inner) = doc.get(*inner_idx) {
                    collect_list_item(inner, doc, *inner_ordered, level.saturating_add(1), out);
                }
            }
        }
    }
}

/// Fallback: split plain text (optionally with form-feed page breaks) into
/// `ParsedBlock::Paragraph` / `ParsedBlock::PageBreak` blocks.
///
/// CRLF line endings are normalised to LF before splitting so that
/// Windows-style paragraph breaks (`\r\n\r\n`) are handled correctly.
#[must_use]
pub fn text_to_blocks(text: &str) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();

    // Normalise Windows line endings so \r\n\r\n is treated as a paragraph break.
    let normalised;
    let text = if text.contains('\r') {
        normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        normalised.as_str()
    } else {
        text
    };

    let pages: Vec<&str> = text.split('\x0C').collect();
    for (page_idx, page) in pages.iter().enumerate() {
        for para in page.split("\n\n") {
            let t = para.trim();
            if !t.is_empty() {
                blocks.push(ParsedBlock::Paragraph {
                    inlines: vec![Inline::plain(t)],
                });
            }
        }
        if page_idx < pages.len() - 1 {
            blocks.push(ParsedBlock::PageBreak);
        }
    }

    blocks
}

/// Convert a flat `kreuzberg::Table` (from `result.tables`) into a `ParsedBlock::Table`.
#[must_use]
pub fn kreuzberg_table_to_block(table: &kreuzberg::Table) -> Option<ParsedBlock> {
    if table.cells.is_empty() {
        return None;
    }
    let rows = table
        .cells
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let cells = row
                .iter()
                .map(|text| TableCell {
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: vec![Inline::plain(text.as_str())],
                    }],
                })
                .collect();
            TableRow {
                is_header: row_idx == 0,
                cells,
            }
        })
        .collect();
    Some(ParsedBlock::Table(TableBlock { rows }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kreuzberg::types::document_structure::{ContentLayer, NodeId};

    fn node(content: NodeContent) -> DocumentNode {
        DocumentNode {
            id: NodeId::new("n"),
            content,
            parent: None,
            children: Vec::new(),
            content_layer: ContentLayer::default(),
            page: None,
            page_end: None,
            bbox: None,
            annotations: Vec::new(),
            attributes: None,
        }
    }

    fn group_doc(first_child: NodeContent) -> DocumentStructure {
        let mut doc = DocumentStructure::new();
        let group = doc.push_node(node(NodeContent::Group {
            label: None,
            heading_level: Some(2),
            heading_text: Some("X".to_owned()),
        }));
        let first = doc.push_node(node(first_child));
        let body = doc.push_node(node(NodeContent::Paragraph {
            text: "body".to_owned(),
        }));
        doc.add_child(group, first);
        doc.add_child(group, body);
        doc
    }

    fn blocks_of(doc: &DocumentStructure) -> Vec<ParsedBlock> {
        let mut out = Vec::new();
        collect_blocks_from_node(&doc.nodes[0], doc, &mut out);
        out
    }

    #[test]
    fn group_heading_not_repeated_when_first_child_is_same_heading() {
        let doc = group_doc(NodeContent::Heading {
            level: 2,
            text: " X ".to_owned(),
        });
        let blocks = blocks_of(&doc);
        assert!(
            matches!(
                blocks.as_slice(),
                [ParsedBlock::Heading { .. }, ParsedBlock::Paragraph { .. }]
            ),
            "got {blocks:?}"
        );
        assert_eq!(heading_of(&blocks[0]), (2, "X".to_owned()));
    }

    fn heading_of(block: &ParsedBlock) -> (u8, String) {
        let ParsedBlock::Heading { level, inlines } = block else {
            panic!("expected heading, got {block:?}");
        };
        let text = inlines
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. }
                | Inline::Link { text, .. }
                | Inline::Code { text, .. } => text.as_str(),
            })
            .collect();
        (*level, text)
    }

    #[test]
    fn group_heading_not_repeated_when_first_child_is_title() {
        let doc = group_doc(NodeContent::Title {
            text: "X".to_owned(),
        });
        let blocks = blocks_of(&doc);
        let headings = blocks
            .iter()
            .filter(|b| matches!(b, ParsedBlock::Heading { .. }))
            .count();
        assert_eq!(headings, 1, "got {blocks:?}");
    }

    #[test]
    fn group_heading_and_different_first_child_heading_both_emitted() {
        let doc = group_doc(NodeContent::Heading {
            level: 2,
            text: "Y".to_owned(),
        });
        let blocks = blocks_of(&doc);
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ParsedBlock::Heading { .. },
                    ParsedBlock::Heading { .. },
                    ParsedBlock::Paragraph { .. }
                ]
            ),
            "got {blocks:?}"
        );
        assert_eq!(heading_of(&blocks[0]).1, "X");
        assert_eq!(heading_of(&blocks[1]).1, "Y");
    }

    #[test]
    fn group_without_children_emits_single_heading() {
        let mut doc = DocumentStructure::new();
        doc.push_node(node(NodeContent::Group {
            label: None,
            heading_level: Some(2),
            heading_text: Some("X".to_owned()),
        }));
        let blocks = blocks_of(&doc);
        assert_eq!(blocks.len(), 1, "got {blocks:?}");
        assert_eq!(heading_of(&blocks[0]), (2, "X".to_owned()));
    }

    #[test]
    fn group_heading_emitted_when_first_child_is_paragraph() {
        let doc = group_doc(NodeContent::Paragraph {
            text: "X".to_owned(),
        });
        let blocks = blocks_of(&doc);
        assert!(
            matches!(
                blocks.as_slice(),
                [
                    ParsedBlock::Heading { .. },
                    ParsedBlock::Paragraph { .. },
                    ParsedBlock::Paragraph { .. }
                ]
            ),
            "got {blocks:?}"
        );
    }

    fn image_blocks(src: Option<&str>) -> Vec<ParsedBlock> {
        let mut doc = DocumentStructure::new();
        doc.push_node(node(NodeContent::Image {
            description: Some("alt".to_owned()),
            image_index: None,
            src: src.map(str::to_owned),
        }));
        blocks_of(&doc)
    }

    #[test]
    fn image_without_src_is_dropped() {
        assert!(image_blocks(None).is_empty());
    }

    #[test]
    fn image_with_blank_src_is_dropped() {
        assert!(image_blocks(Some("   ")).is_empty());
    }

    #[test]
    fn image_with_src_is_kept() {
        let blocks = image_blocks(Some("pic.png"));
        assert!(
            matches!(
                blocks.as_slice(),
                [ParsedBlock::Image { src: Some(s), alt: Some(a), .. }]
                    if s == "pic.png" && a == "alt"
            ),
            "got {blocks:?}"
        );
    }
}
