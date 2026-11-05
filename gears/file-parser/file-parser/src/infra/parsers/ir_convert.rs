//! Utilities for converting Kreuzberg extraction results into our IR types.

use kreuzberg::ExtractionResult;
use kreuzberg::types::document_structure::{
    AnnotationKind, DocumentNode, DocumentStructure, NodeContent, TextAnnotation,
};

use crate::domain::ir::{Inline, InlineStyle, ParsedBlock, TableBlock, TableCell, TableRow};

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

/// Build inlines from a node's raw text and its Kreuzberg `TextAnnotation`s.
///
/// The text is trimmed (annotation byte offsets refer to the untrimmed text and are
/// shifted accordingly). Only bold / italic / underline / strikethrough are mapped;
/// other annotation kinds are ignored. Overlapping annotations are merged into one
/// style per segment and adjacent segments with an identical style are coalesced.
/// Offsets that are out of range or not on a `char` boundary are tolerated, never a panic.
/// Without relevant annotations the result is exactly one plain inline.
fn inlines_from_text(raw: &str, annotations: &[TextAnnotation]) -> Vec<Inline> {
    let text = raw.trim();
    let lead = raw.len() - raw.trim_start().len();

    // (start, end, style flags) in trimmed-text coordinates.
    let ranges: Vec<(usize, usize, InlineStyle)> = annotations
        .iter()
        .filter_map(|a| {
            let mut style = InlineStyle::default();
            match a.kind {
                AnnotationKind::Bold => style.bold = true,
                AnnotationKind::Italic => style.italic = true,
                AnnotationKind::Underline => style.underline = true,
                AnnotationKind::Strikethrough => style.strike = true,
                _ => return None,
            }
            let start = (a.start as usize).saturating_sub(lead).min(text.len());
            let end = (a.end as usize).saturating_sub(lead).min(text.len());
            (start < end).then_some((start, end, style))
        })
        .collect();
    if ranges.is_empty() {
        return vec![Inline::plain(text)];
    }

    let mut bounds: Vec<usize> = vec![0, text.len()];
    for (s, e, _) in &ranges {
        bounds.extend([*s, *e]);
    }
    bounds.retain(|b| text.is_char_boundary(*b));
    bounds.sort_unstable();
    bounds.dedup();

    let mut out: Vec<(String, InlineStyle)> = Vec::new();
    for w in bounds.windows(2) {
        let (from, to) = (w[0], w[1]);
        let mut style = InlineStyle::default();
        for (s, e, st) in &ranges {
            if *s <= from && to <= *e {
                style.bold |= st.bold;
                style.italic |= st.italic;
                style.underline |= st.underline;
                style.strike |= st.strike;
            }
        }
        match out.last_mut() {
            Some((prev, prev_style)) if *prev_style == style => prev.push_str(&text[from..to]),
            _ => out.push((text[from..to].to_owned(), style)),
        }
    }
    out.into_iter()
        .map(|(t, style)| {
            if style == InlineStyle::default() {
                Inline::plain(t)
            } else {
                Inline::styled(t, style)
            }
        })
        .collect()
}

/// Emit a non-empty trimmed text string as a `ParsedBlock::Paragraph`.
fn push_paragraph(text: &str, annotations: &[TextAnnotation], out: &mut Vec<ParsedBlock>) {
    if !text.trim().is_empty() {
        out.push(ParsedBlock::Paragraph {
            inlines: inlines_from_text(text, annotations),
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
            if !text.trim().is_empty() {
                out.push(ParsedBlock::Heading {
                    level: 1,
                    inlines: inlines_from_text(text, &node.annotations),
                });
            }
        }
        NodeContent::Heading { level, text } => {
            if !text.trim().is_empty() {
                out.push(ParsedBlock::Heading {
                    level: clamp_heading_level(*level),
                    inlines: inlines_from_text(text, &node.annotations),
                });
            }
        }
        // Plain text nodes — all emit as paragraphs
        NodeContent::Paragraph { text }
        | NodeContent::Formula { text }
        | NodeContent::Footnote { text }
        | NodeContent::Citation { text, .. }
        | NodeContent::RawBlock { content: text, .. } => {
            push_paragraph(text, &node.annotations, out);
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
            if !text.trim().is_empty() {
                out.push(ParsedBlock::ListItem {
                    level: 0,
                    ordered: false,
                    blocks: vec![ParsedBlock::Paragraph {
                        inlines: inlines_from_text(text, &node.annotations),
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
    if let NodeContent::ListItem { text } = &node.content
        && !text.trim().is_empty()
    {
        out.push(ParsedBlock::ListItem {
            level,
            ordered,
            blocks: vec![ParsedBlock::Paragraph {
                inlines: inlines_from_text(text, &node.annotations),
            }],
        });
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
#[allow(clippy::non_ascii_literal)]
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
    fn group_heading_deduped_even_when_child_level_differs() {
        let doc = group_doc(NodeContent::Heading {
            level: 3,
            text: "X".to_owned(),
        });
        let blocks = blocks_of(&doc);
        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, ParsedBlock::Heading { .. }))
            .collect();
        assert_eq!(headings.len(), 1, "got {blocks:?}");
        assert_eq!(heading_of(headings[0]), (3, "X".to_owned()));
    }

    #[test]
    fn group_with_blank_heading_text_emits_no_group_heading() {
        let mut doc = DocumentStructure::new();
        let group = doc.push_node(node(NodeContent::Group {
            label: None,
            heading_level: Some(2),
            heading_text: Some("  ".to_owned()),
        }));
        let body = doc.push_node(node(NodeContent::Paragraph {
            text: "body".to_owned(),
        }));
        doc.add_child(group, body);
        let blocks = blocks_of(&doc);
        assert!(
            matches!(blocks.as_slice(), [ParsedBlock::Paragraph { .. }]),
            "got {blocks:?}"
        );
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

    // ---- inline style annotations ----

    fn ann(start: u32, end: u32, kind: AnnotationKind) -> TextAnnotation {
        TextAnnotation { start, end, kind }
    }

    fn st(flags: &str) -> InlineStyle {
        InlineStyle {
            bold: flags.contains('b'),
            italic: flags.contains('i'),
            underline: flags.contains('u'),
            strike: flags.contains('s'),
            code: false,
        }
    }

    fn txt(t: &str, style: InlineStyle) -> Inline {
        Inline::styled(t, style)
    }

    #[test]
    fn no_annotations_gives_single_plain_inline() {
        assert_eq!(
            inlines_from_text("  hello  ", &[]),
            vec![Inline::plain("hello")]
        );
    }

    #[test]
    fn irrelevant_annotation_kinds_are_ignored() {
        let a = [
            ann(0, 5, AnnotationKind::Highlight),
            ann(0, 5, AnnotationKind::Code),
        ];
        assert_eq!(inlines_from_text("hello", &a), vec![Inline::plain("hello")]);
    }

    #[test]
    fn each_single_style_maps() {
        for (kind, style) in [
            (AnnotationKind::Bold, st("b")),
            (AnnotationKind::Italic, st("i")),
            (AnnotationKind::Underline, st("u")),
            (AnnotationKind::Strikethrough, st("s")),
        ] {
            assert_eq!(
                inlines_from_text("ab cd", &[ann(3, 5, kind)]),
                vec![Inline::plain("ab "), txt("cd", style)]
            );
        }
    }

    #[test]
    fn whole_text_annotation_gives_one_styled_inline() {
        assert_eq!(
            inlines_from_text("hello", &[ann(0, 5, AnnotationKind::Bold)]),
            vec![txt("hello", st("b"))]
        );
    }

    #[test]
    fn bold_and_italic_on_same_range_merge() {
        let a = [
            ann(0, 3, AnnotationKind::Bold),
            ann(0, 3, AnnotationKind::Italic),
        ];
        assert_eq!(inlines_from_text("abc", &a), vec![txt("abc", st("bi"))]);
    }

    #[test]
    fn partial_overlap_gives_three_segments() {
        let a = [
            ann(0, 5, AnnotationKind::Bold),
            ann(3, 8, AnnotationKind::Italic),
        ];
        assert_eq!(
            inlines_from_text("abcdefgh", &a),
            vec![
                txt("abc", st("b")),
                txt("de", st("bi")),
                txt("fgh", st("i")),
            ]
        );
    }

    #[test]
    fn adjacent_same_style_segments_merge() {
        let a = [
            ann(0, 2, AnnotationKind::Bold),
            ann(2, 4, AnnotationKind::Bold),
        ];
        assert_eq!(inlines_from_text("abcd", &a), vec![txt("abcd", st("b"))]);
    }

    #[test]
    fn out_of_range_end_is_clamped() {
        assert_eq!(
            inlines_from_text("abc", &[ann(1, 99, AnnotationKind::Bold)]),
            vec![Inline::plain("a"), txt("bc", st("b"))]
        );
    }

    #[test]
    fn start_past_end_is_dropped() {
        assert_eq!(
            inlines_from_text(
                "abc",
                &[
                    ann(5, 9, AnnotationKind::Bold),
                    ann(2, 2, AnnotationKind::Italic)
                ]
            ),
            vec![Inline::plain("abc")]
        );
    }

    #[test]
    fn multibyte_offsets_are_byte_based() {
        // "Привет мир": "Привет" = 12 bytes, space = 1, "мир" = 6 bytes
        assert_eq!(
            inlines_from_text("Привет мир", &[ann(0, 12, AnnotationKind::Bold)]),
            vec![txt("Привет", st("b")), Inline::plain(" мир")]
        );
        assert_eq!(
            inlines_from_text("日本語x", &[ann(3, 6, AnnotationKind::Italic)]),
            vec![
                Inline::plain("日"),
                txt("本", st("i")),
                Inline::plain("語x")
            ]
        );
    }

    #[test]
    fn non_char_boundary_offset_does_not_panic() {
        // byte 1 is inside the 2-byte 'П'; must not panic, text stays intact.
        let out = inlines_from_text("Привет", &[ann(1, 12, AnnotationKind::Bold)]);
        let joined: String = out
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. } => text.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(joined, "Привет");
    }

    #[test]
    fn offsets_are_shifted_by_trimmed_leading_whitespace() {
        // raw = "  ab cd  ": "cd" sits at bytes 5..7 of the UNtrimmed text
        assert_eq!(
            inlines_from_text("  ab cd  ", &[ann(5, 7, AnnotationKind::Bold)]),
            vec![Inline::plain("ab "), txt("cd", st("b"))]
        );
        // annotation that starts inside the trimmed leading whitespace
        assert_eq!(
            inlines_from_text("  ab", &[ann(0, 3, AnnotationKind::Bold)]),
            vec![txt("a", st("b")), Inline::plain("b")]
        );
    }

    fn styled_para(kind: AnnotationKind, text: &str) -> DocumentNode {
        let mut n = node(NodeContent::Paragraph {
            text: text.to_owned(),
        });
        n.annotations = vec![ann(0, 1, kind)];
        n
    }

    fn first_inlines(block: &ParsedBlock) -> &Vec<Inline> {
        match block {
            ParsedBlock::Paragraph { inlines } | ParsedBlock::Heading { inlines, .. } => inlines,
            ParsedBlock::ListItem { blocks, .. } => first_inlines(&blocks[0]),
            other => panic!("unexpected block {other:?}"),
        }
    }

    fn convert(n: DocumentNode) -> Vec<ParsedBlock> {
        let mut doc = DocumentStructure::new();
        doc.push_node(n);
        blocks_of(&doc)
    }

    #[test]
    fn annotation_on_paragraph_heading_title_and_bare_list_item() {
        let bold = st("b");
        let expect = vec![txt("a", bold), Inline::plain("bc")];

        let b = convert(styled_para(AnnotationKind::Bold, "abc"));
        assert_eq!(first_inlines(&b[0]), &expect);

        let mut h = node(NodeContent::Heading {
            level: 2,
            text: "abc".to_owned(),
        });
        h.annotations = vec![ann(0, 1, AnnotationKind::Bold)];
        let b = convert(h);
        assert_eq!(first_inlines(&b[0]), &expect);

        let mut t = node(NodeContent::Title {
            text: "abc".to_owned(),
        });
        t.annotations = vec![ann(0, 1, AnnotationKind::Bold)];
        let b = convert(t);
        assert_eq!(first_inlines(&b[0]), &expect);

        let mut li = node(NodeContent::ListItem {
            text: "abc".to_owned(),
        });
        li.annotations = vec![ann(0, 1, AnnotationKind::Bold)];
        let b = convert(li);
        assert_eq!(first_inlines(&b[0]), &expect);
    }

    #[test]
    fn annotation_on_list_item_inside_list() {
        let mut doc = DocumentStructure::new();
        let list = doc.push_node(node(NodeContent::List { ordered: false }));
        let mut li = node(NodeContent::ListItem {
            text: "abc".to_owned(),
        });
        li.annotations = vec![ann(0, 1, AnnotationKind::Italic)];
        let item = doc.push_node(li);
        doc.add_child(list, item);
        let b = blocks_of(&doc);
        assert_eq!(
            first_inlines(&b[0]),
            &vec![txt("a", st("i")), Inline::plain("bc")]
        );
    }
}
