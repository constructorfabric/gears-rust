use std::fmt::Write as _;
use std::time::Duration;

use super::engine::{Deadline, Direction, Glyph, PDFIUM_LOCK, clean_title, guarded};
use super::layout::{Line, page_lines};
use super::structure::{self, Page};
use super::*;
use crate::domain::ir::{Inline, InlineStyle};

// --- synthetic glyphs ---------------------------------------------------------

/// Glyphs for `text` on a baseline at `y`, 12 pt, 6 pt advance, starting at
/// `x`. Spaces in `text` become space hints on the next glyph, as pdfium
/// reports them.
fn word_glyphs(text: &str, x: f32, y: f32, seq_from: u32) -> Vec<Glyph> {
    glyphs_with(text, x, y, seq_from, 12.0, Direction::Deg0)
}

fn glyphs_with(
    text: &str,
    x: f32,
    y: f32,
    seq_from: u32,
    size: f32,
    direction: Direction,
) -> Vec<Glyph> {
    let advance = size / 2.0;
    let mut out = Vec::new();
    let mut pen = x;
    let mut space = false;
    let mut seq = seq_from;
    for ch in text.chars() {
        if ch == ' ' {
            space = true;
            pen += advance;
            continue;
        }
        out.push(glyph(ch, pen, y, size, direction, seq, space));
        space = false;
        seq += 1;
        pen += advance;
    }
    out
}

/// A glyph whose upright frame is `direction`'s: `along` runs with the text,
/// `y` is the baseline in that frame.
fn glyph(
    ch: char,
    along: f32,
    y: f32,
    size: f32,
    direction: Direction,
    seq: u32,
    space_before: bool,
) -> Glyph {
    let advance = size / 2.0;
    // Page coordinates that map back to (along, y) in the upright frame.
    let (to_page, box_page) = match direction {
        Direction::Deg0 | Direction::Other => ((along, y), [along, y, along + advance, y + size]),
        Direction::Deg90 => ((-y, along), [-y - size, along, -y, along + advance]),
        Direction::Deg180 => ((-along, -y), [-along - advance, -y - size, -along, -y]),
        Direction::Deg270 => ((y, -along), [y, -along - advance, y + size, -along]),
    };
    Glyph {
        ch,
        x: to_page.0,
        y: to_page.1,
        bounds: box_page,
        size,
        direction,
        angle: match direction {
            Direction::Deg0 | Direction::Other => 0.0,
            Direction::Deg90 => 90.0,
            Direction::Deg180 => 180.0,
            Direction::Deg270 => 270.0,
        },
        seq,
        space_before,
        hyphen: false,
        bold: false,
        italic: false,
    }
}

fn lines_of(glyphs: Vec<Glyph>) -> Vec<Line> {
    page_lines(glyphs, &mut TextBudget::new(&ParseLimits::default())).expect("lines")
}

/// The blocks of a one-page document made of `lines`.
fn paragraphs(lines: Vec<Line>) -> Vec<ParsedBlock> {
    structure::blocks(vec![Page { lines, area: None }])
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

fn para(text: &str) -> ParsedBlock {
    ParsedBlock::Paragraph {
        inlines: vec![Inline::plain(text)],
    }
}

// --- layout -------------------------------------------------------------------

#[test]
fn rows_read_top_to_bottom_left_to_right() {
    let mut glyphs = word_glyphs("second", 72.0, 686.0, 10);
    glyphs.extend(word_glyphs("first line", 72.0, 700.0, 0));
    assert_eq!(texts(&lines_of(glyphs)), ["first line", "second"]);
}

#[test]
fn content_spaces_decide_between_content_neighbours() {
    // Narrow glyphs drawn far apart but adjacent in content with no space:
    // one word. A space hint: two words, however tight.
    let mut glyphs = word_glyphs("ab", 72.0, 700.0, 0);
    glyphs[1].x += 2.5;
    glyphs[1].bounds[0] += 2.5;
    glyphs[1].bounds[2] += 2.5;
    let mut tight = word_glyphs("c d", 72.0, 650.0, 2);
    tight[1].x -= 6.0;
    tight[1].bounds[0] -= 6.0;
    tight[1].bounds[2] -= 6.0;
    glyphs.extend(tight);
    assert_eq!(texts(&lines_of(glyphs)), ["ab", "c d"]);
}

#[test]
fn wide_gaps_split_a_row_into_lines() {
    let mut glyphs = word_glyphs("left", 72.0, 700.0, 0);
    glyphs.extend(word_glyphs("right", 300.0, 700.0, 4));
    let lines = lines_of(glyphs);
    assert_eq!(texts(&lines), ["left", "right"]);
    // Two lines on one baseline are separate paragraphs.
    assert_eq!(paragraphs(lines), [para("left"), para("right")]);
}

#[test]
fn right_to_left_runs_are_put_in_logical_order() {
    // "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd}" drawn visually: Latin first, then the Hebrew glyphs
    // left to right, i.e. the word reversed; content order is logical.
    let mut glyphs = word_glyphs("Hebrew:", 72.0, 700.0, 0);
    let visual = ['\u{5dd}', '\u{5d5}', '\u{5dc}', '\u{5e9}'];
    for (i, ch) in visual.iter().enumerate() {
        let seq = 7 + u32::try_from(visual.len() - 1 - i).unwrap();
        let space = seq == 7;
        glyphs.push(glyph(
            *ch,
            120.0 + 6.0 * i.to_f32(),
            700.0,
            12.0,
            Direction::Deg0,
            seq,
            space,
        ));
    }
    assert_eq!(
        texts(&lines_of(glyphs)),
        ["Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd}"]
    );
}

#[test]
fn numbers_and_brackets_inside_rtl_runs() {
    // An all-RTL line drawn as "(\u{5d1}) 12 \u{5d0}" reads "\u{5d0} 12 (\u{5d1})": reversed,
    // with the number kept and the brackets mirrored.
    let visual = ['(', '\u{5d1}', ')', ' ', '1', '2', ' ', '\u{5d0}'];
    let mut glyphs = Vec::new();
    let mut seq = 0;
    for (i, ch) in visual.iter().enumerate() {
        if *ch == ' ' {
            continue;
        }
        glyphs.push(glyph(
            *ch,
            72.0 + 6.0 * i.to_f32(),
            700.0,
            12.0,
            Direction::Deg0,
            100 + seq,
            false,
        ));
        seq += 2;
    }
    let text = texts(&lines_of(glyphs)).join("");
    assert_eq!(text, "\u{5d0} 12 (\u{5d1})");
}

#[test]
fn cjk_uses_no_geometric_spaces_but_keeps_content_spaces() {
    let mut glyphs = Vec::new();
    for (i, ch) in "\u{4f60}\u{597d}".chars().enumerate() {
        glyphs.push(glyph(
            ch,
            72.0 + 20.0 * i.to_f32(),
            700.0,
            12.0,
            Direction::Deg0,
            2 * u32::try_from(i).unwrap(),
            false,
        ));
    }
    glyphs.push(glyph(
        '\u{4e16}',
        110.0,
        700.0,
        12.0,
        Direction::Deg0,
        3,
        true,
    ));
    assert_eq!(texts(&lines_of(glyphs)), ["\u{4f60}\u{597d} \u{4e16}"]);
}

#[test]
fn ligatures_expand_and_soft_hyphens_vanish() {
    let glyphs = word_glyphs("\u{fb01}nance co\u{ad}op", 72.0, 700.0, 0);
    assert_eq!(texts(&lines_of(glyphs)), ["finance coop"]);
}

#[test]
fn rotated_text_reads_in_its_own_direction() {
    for direction in [Direction::Deg90, Direction::Deg180, Direction::Deg270] {
        let mut glyphs = glyphs_with("up the side", 100.0, 50.0, 0, 12.0, direction);
        glyphs.extend(glyphs_with("next line", 100.0, 36.0, 20, 12.0, direction));
        assert_eq!(
            texts(&lines_of(glyphs)),
            ["up the side", "next line"],
            "{direction:?}"
        );
    }
}

#[test]
fn rotated_text_comes_after_upright_text() {
    let mut glyphs = glyphs_with("STAMP", 100.0, 20.0, 0, 12.0, Direction::Deg90);
    glyphs.extend(word_glyphs("body", 72.0, 700.0, 5));
    assert_eq!(texts(&lines_of(glyphs)), ["body", "STAMP"]);
}

#[test]
fn odd_angles_read_in_content_order() {
    let mut glyphs = glyphs_with("tilted", 0.0, 0.0, 0, 12.0, Direction::Other);
    // Scatter the positions; content order still gives the word.
    for (i, g) in glyphs.iter_mut().enumerate() {
        g.x = 500.0 - 40.0 * i.to_f32();
        g.y = 40.0 * i.to_f32();
    }
    assert_eq!(texts(&lines_of(glyphs)), ["tilted"]);
}

#[test]
fn paragraphs_follow_the_usual_line_pitch() {
    // Double-spaced body (24 pt pitch on 12 pt text) stays one paragraph; a
    // wider gap starts the next.
    let mut glyphs = Vec::new();
    let mut seq = 0;
    for (i, y) in [700.0, 676.0, 652.0, 600.0, 576.0].iter().enumerate() {
        let text = format!("line{i}");
        glyphs.extend(word_glyphs(&text, 72.0, *y, seq));
        seq += 10;
    }
    let blocks = paragraphs(lines_of(glyphs));
    assert_eq!(blocks, [para("line0\nline1\nline2"), para("line3\nline4")]);
}

#[test]
fn a_size_change_starts_a_paragraph() {
    let mut glyphs = glyphs_with("Title", 72.0, 720.0, 0, 24.0, Direction::Deg0);
    glyphs.extend(word_glyphs("body one", 72.0, 700.0, 10));
    glyphs.extend(word_glyphs("body two", 72.0, 686.0, 20));
    assert_eq!(
        paragraphs(lines_of(glyphs)),
        [heading(1, "Title"), para("body one\nbody two")]
    );
}

#[test]
fn styles_become_runs() {
    let mut glyphs = word_glyphs("bold plain", 72.0, 700.0, 0);
    for g in glyphs.iter_mut().take(4) {
        g.bold = true;
    }
    let bold = InlineStyle {
        bold: true,
        ..InlineStyle::default()
    };
    assert_eq!(
        paragraphs(lines_of(glyphs)),
        [ParsedBlock::Paragraph {
            inlines: vec![Inline::styled("bold", bold), Inline::plain(" plain")]
        }]
    );
}

#[test]
fn line_size_is_the_most_common() {
    let mut glyphs = word_glyphs("abc", 72.0, 700.0, 0);
    glyphs.extend(glyphs_with("X", 90.0, 700.0, 3, 30.0, Direction::Deg0));
    let lines = lines_of(glyphs);
    assert_eq!(lines.len(), 1);
    assert!((lines[0].size - 12.0).abs() < 0.01, "{}", lines[0].size);
}

#[test]
fn page_text_is_charged_to_the_budget() {
    let limits = ParseLimits {
        max_text_bytes: 5,
        ..ParseLimits::default()
    };
    let err = page_lines(
        word_glyphs("too long", 72.0, 700.0, 0),
        &mut TextBudget::new(&limits),
    )
    .expect_err("over budget");
    assert!(err.to_string().contains("max_text_bytes"), "{err}");
}

#[test]
fn degenerate_geometry_does_not_panic() {
    let mut glyphs = word_glyphs("abc", 72.0, 700.0, 0);
    glyphs[1].x = f32::MAX;
    glyphs[1].bounds = [f32::MIN, f32::MIN, f32::MAX, f32::MAX];
    glyphs[2].seq = glyphs[0].seq;
    drop(lines_of(glyphs));
}

#[test]
fn titles_drop_placeholders() {
    assert_eq!(
        clean_title("  Real \n Title  ").as_deref(),
        Some("Real Title")
    );
    assert_eq!(clean_title("Microsoft Word - report.docx"), None);
    assert_eq!(clean_title("untitled"), None);
    assert_eq!(clean_title("scan_0001.pdf"), None);
    assert_eq!(clean_title(" \0 "), None);
    assert_eq!(clean_title(&"x".repeat(2000)).map(|t| t.len()), Some(512));
}

// --- pdfium, on PDFs written by the tests ----------------------------------------

/// A minimal PDF: one page per content stream, Helvetica as F1 and
/// Helvetica-Bold as F2.
fn pdf(pages: &[(&str, &str)], title: Option<&[u8]>) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
        Vec::new(), // the page tree, filled in below
    ];
    let mut kids = String::new();
    for (content, extra) in pages {
        objects.push(
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        );
        let contents = objects.len();
        objects.push(
            format!(
                "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 1 0 R /F2 2 0 R >> >> /Contents {contents} 0 R {extra} >>"
            )
            .into_bytes(),
        );
        write!(kids, "{} 0 R ", objects.len()).unwrap();
    }
    objects[2] = format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes();
    objects.push(b"<< /Type /Catalog /Pages 3 0 R >>".to_vec());
    let catalog = objects.len();
    let info = title.map(|t| {
        let mut o = b"<< /Title (".to_vec();
        o.extend_from_slice(t);
        o.extend_from_slice(b") >>");
        objects.push(o);
        objects.len()
    });
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        writeln!(tail, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        tail,
        "trailer\n<< /Size {} /Root {catalog} 0 R",
        objects.len() + 1
    )
    .unwrap();
    if let Some(info) = info {
        write!(tail, " /Info {info} 0 R").unwrap();
    }
    write!(tail, " >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
    out.extend_from_slice(tail.as_bytes());
    out
}

fn read(bytes: &[u8]) -> Pdf {
    read_pdf(bytes, &ParseLimits::default()).expect("PDF reads")
}

fn read_err(bytes: &[u8], limits: &ParseLimits) -> String {
    match read_pdf(bytes, limits) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

fn block_texts(blocks: &[ParsedBlock]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| match b {
            ParsedBlock::Paragraph { inlines } | ParsedBlock::Heading { inlines, .. } => inlines
                .iter()
                .map(|i| match i {
                    Inline::Text { text, .. }
                    | Inline::Link { text, .. }
                    | Inline::Code { text, .. } => text.as_str(),
                })
                .collect(),
            ParsedBlock::PageBreak => "<break>".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn text_title_and_bold_from_a_real_pdf() {
    let content = "BT /F1 12 Tf 72 700 Td (plain ) Tj /F2 12 Tf (bold) Tj ET";
    let doc = read(&pdf(&[(content, "")], Some(b"  Real Title  ")));
    assert_eq!(doc.title.as_deref(), Some("Real Title"));
    let bold = InlineStyle {
        bold: true,
        ..InlineStyle::default()
    };
    // Helvetica-Bold reports no weight; the font name makes it bold.
    assert_eq!(
        doc.blocks,
        [ParsedBlock::Paragraph {
            inlines: vec![Inline::plain("plain "), Inline::styled("bold", bold)]
        }]
    );
}

#[test]
fn every_page_is_separated_empty_ones_included() {
    let page = "BT /F1 12 Tf 72 700 Td (text) Tj ET";
    let doc = read(&pdf(&[("", ""), (page, ""), ("", "")], None));
    assert_eq!(block_texts(&doc.blocks), ["<break>", "text", "<break>"]);
}

#[test]
fn rotated_pages_and_rotated_text_are_read() {
    let rotated_page = read(&pdf(
        &[(
            "BT /F1 12 Tf 72 700 Td (Rotated page text) Tj ET",
            "/Rotate 90",
        )],
        None,
    ));
    assert_eq!(block_texts(&rotated_page.blocks), ["Rotated page text"]);
    let content = "BT /F1 12 Tf 72 700 Td (Body) Tj ET BT /F1 12 Tf 0 1 -1 0 300 300 Tm (Vertical text) Tj ET BT /F1 12 Tf -1 0 0 -1 300 500 Tm (Upside down) Tj ET";
    let doc = read(&pdf(&[(content, "")], None));
    let texts = block_texts(&doc.blocks);
    assert_eq!(texts[0], "Body");
    assert!(texts.contains(&"Vertical text".to_owned()), "{texts:?}");
    assert!(texts.contains(&"Upside down".to_owned()), "{texts:?}");
}

#[test]
fn invisible_text_is_kept_and_tiny_or_offpage_text_dropped() {
    let content = "BT 3 Tr /F1 12 Tf 72 700 Td (Invisible OCR layer) Tj ET BT 0 Tr /F1 0.5 Tf 72 680 Td (tiny text) Tj ET BT /F1 12 Tf 2000 2000 Td (far away) Tj ET";
    let doc = read(&pdf(&[(content, "")], None));
    assert_eq!(block_texts(&doc.blocks), ["Invisible OCR layer"]);
}

#[test]
fn placeholder_titles_fall_back() {
    let doc = read(&pdf(&[("", "")], Some(b"Microsoft Word - report.docx")));
    assert_eq!(doc.title, None);
    let utf16 = read(&pdf(&[("", "")], Some(b"\xfe\xff\x04\x1f\x04\x40\x04\x38")));
    assert_eq!(utf16.title.as_deref(), Some("\u{41f}\u{440}\u{438}"));
}

#[test]
fn broken_inputs_are_errors() {
    let full = pdf(&[("BT /F1 12 Tf 72 700 Td (x) Tj ET", "")], None);
    for bytes in [
        &b""[..],
        b"garbage, not a pdf",
        b"%PDF-1.7\n%%EOF\n",
        &full[..300],
    ] {
        let err = read_err(bytes, &ParseLimits::default());
        assert!(err.contains("PDF"), "{err}");
    }
}

#[test]
fn page_limit_is_checked_before_reading_pages() {
    let pages: Vec<(&str, &str)> = (0..6).map(|_| ("", "")).collect();
    let limits = ParseLimits {
        max_pdf_pages: 5,
        ..ParseLimits::default()
    };
    let err = read_err(&pdf(&pages, None), &limits);
    assert!(err.contains("max_pdf_pages"), "{err}");
}

#[test]
fn char_limit_per_page() {
    let limits = ParseLimits {
        max_pdf_chars_per_page: 5,
        ..ParseLimits::default()
    };
    let err = read_err(
        &pdf(
            &[("BT /F1 12 Tf 72 700 Td (more than five) Tj ET", "")],
            None,
        ),
        &limits,
    );
    assert!(err.contains("max_pdf_chars_per_page"), "{err}");
}

#[test]
fn concurrent_documents_are_serialised_safely() {
    // Every thread is started before any is joined.
    #[allow(clippy::needless_collect)]
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                let content = format!("BT /F1 12 Tf 72 700 Td (document {i}) Tj ET");
                let doc = read(&pdf(
                    &[(content.as_str(), ""), (content.as_str(), "")],
                    None,
                ));
                block_texts(&doc.blocks)
            })
        })
        .collect();
    for (i, handle) in handles.into_iter().enumerate() {
        let texts = handle.join().expect("no panic");
        let line = format!("document {i}");
        assert_eq!(texts, [line.clone(), "<break>".to_owned(), line]);
    }
}

/// Small test indices as `f32`.
trait ToF32 {
    fn to_f32(self) -> f32;
}

impl ToF32 for usize {
    fn to_f32(self) -> f32 {
        f32::from(u16::try_from(self).expect("small index"))
    }
}

// --- review fixes ---------------------------------------------------------------

/// `glyphs` (upright, at `y`) turned by `degrees` around the origin, as a
/// tilted page draws them.
fn tilted(text: &str, x: f32, y: f32, seq_from: u32, degrees: f32) -> Vec<Glyph> {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let turn = |px: f32, py: f32| (px * cos - py * sin, px * sin + py * cos);
    word_glyphs(text, x, y, seq_from)
        .into_iter()
        .map(|mut g| {
            (g.x, g.y) = turn(g.x, g.y);
            let [x0, y0, x1, y1] = g.bounds;
            let corners = [turn(x0, y0), turn(x0, y1), turn(x1, y0), turn(x1, y1)];
            g.bounds = [
                corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min),
                corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min),
                corners
                    .iter()
                    .map(|c| c.0)
                    .fold(f32::NEG_INFINITY, f32::max),
                corners
                    .iter()
                    .map(|c| c.1)
                    .fold(f32::NEG_INFINITY, f32::max),
            ];
            g.angle = degrees.rem_euclid(360.0);
            g.direction = Direction::from_degrees(degrees);
            g
        })
        .collect()
}

#[test]
fn slightly_skewed_text_keeps_its_lines() {
    let first = "The quick brown fox jumps over the lazy dog again and again";
    let second = "and then a second line follows it closely below the first";
    let mut glyphs = tilted(first, 72.0, 700.0, 0, 2.0);
    glyphs.extend(tilted(second, 72.0, 686.0, 100, 2.0));
    assert_eq!(texts(&lines_of(glyphs)), [first, second]);
}

/// Glyphs for a visual sequence of characters; content positions come from
/// `logical`, the index of each glyph in reading order.
fn visual_line(visual: &str, logical: &[u32]) -> Vec<Glyph> {
    visual
        .chars()
        .zip(logical)
        .enumerate()
        .filter(|(_, (ch, _))| *ch != ' ')
        .map(|(i, (ch, seq))| {
            glyph(
                ch,
                72.0 + 6.0 * i.to_f32(),
                700.0,
                12.0,
                Direction::Deg0,
                *seq,
                false,
            )
        })
        .collect()
}

#[test]
fn numbers_with_separators_stay_whole_in_rtl_lines() {
    // "\u{5d0} 3.14" drawn right to left: the number shows left of the letter.
    let glyphs = visual_line("3.14 \u{5d0}", &[1, 2, 3, 4, 99, 0]);
    assert_eq!(texts(&lines_of(glyphs)).join(""), "\u{5d0} 3.14");
    let date = visual_line(
        "12/05/2024 \u{5d0}",
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 99, 0],
    );
    assert_eq!(texts(&lines_of(date)).join(""), "\u{5d0} 12/05/2024");
}

#[test]
fn arabic_indic_digits_keep_their_order() {
    // "\u{644} \u{661}\u{662}\u{663}": the number reads 123 left to right.
    let glyphs = visual_line("\u{661}\u{662}\u{663} \u{644}", &[1, 2, 3, 99, 0]);
    assert_eq!(
        texts(&lines_of(glyphs)).join(""),
        "\u{644} \u{661}\u{662}\u{663}"
    );
}

#[test]
fn rtl_line_with_a_latin_word_keeps_word_order() {
    // On a page that mostly reads right to left, logical
    // "\u{5e9}\u{5dc}\u{5d5}\u{5dd} world" is drawn "world \u{5dd}\u{5d5}\u{5dc}\u{5e9}".
    let mut glyphs = visual_line(
        "world \u{5dd}\u{5d5}\u{5dc}\u{5e9}",
        &[5, 6, 7, 8, 9, 99, 3, 2, 1, 0],
    );
    if let Some(w) = glyphs.iter_mut().find(|g| g.seq == 5) {
        w.space_before = true;
    }
    let hebrew_line: Vec<Glyph> = "\u{5d0}\u{5d1}\u{5d2}\u{5d3}\u{5d4}\u{5d5}"
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            glyph(
                ch,
                72.0 + 6.0 * i.to_f32(),
                650.0,
                12.0,
                Direction::Deg0,
                200,
                false,
            )
        })
        .collect();
    glyphs.extend(hebrew_line);
    assert_eq!(
        texts(&lines_of(glyphs))[0],
        "\u{5e9}\u{5dc}\u{5d5}\u{5dd} world"
    );
}

#[test]
fn rtl_runs_in_a_latin_page_keep_the_latin_order() {
    // "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd}" in a left-to-right document, whatever
    // order pdfium lists the characters in.
    let glyphs = visual_line(
        "Hebrew: \u{5dd}\u{5d5}\u{5dc}\u{5e9}",
        &[4, 5, 6, 7, 8, 9, 10, 99, 3, 2, 1, 0],
    );
    assert_eq!(
        texts(&lines_of(glyphs)).join(""),
        "Hebrew: \u{5e9}\u{5dc}\u{5d5}\u{5dd}"
    );
}

#[test]
fn a_drop_cap_does_not_join_the_line_above() {
    let mut glyphs = word_glyphs("line one text", 100.0, 700.0, 0);
    glyphs.extend(word_glyphs("line two text", 100.0, 686.0, 20));
    glyphs.extend(glyphs_with("T", 60.0, 686.0, 19, 40.0, Direction::Deg0));
    let lines = lines_of(glyphs);
    assert_eq!(lines[0].text(), "line one text");
}

#[test]
fn a_dropped_glyph_breaks_content_adjacency() {
    // seq 0 and 2: something between them was dropped, so the gap decides.
    let mut glyphs = word_glyphs("a", 72.0, 700.0, 0);
    glyphs.extend(word_glyphs("b", 90.0, 700.0, 2));
    assert_eq!(texts(&lines_of(glyphs)), ["a b"]);
}

#[test]
fn engine_busy_is_an_internal_error() {
    let limits = ParseLimits {
        pdf_timeout: Duration::from_millis(50),
        ..ParseLimits::default()
    };
    let bytes = pdf(&[("BT /F1 12 Tf 72 700 Td (x) Tj ET", "")], None);
    let held = PDFIUM_LOCK.lock();
    let err = read_pdf(&bytes, &limits).err().map(|e| e.to_string());
    drop(held);
    let err = err.expect("busy while the lock is held");
    assert!(err.contains("busy"), "{err}");
    assert!(
        err.starts_with("IO error"),
        "an engine failure, not the document's: {err}"
    );
}

#[test]
fn panics_become_engine_errors() {
    let result: Result<(), DomainError> = guarded(|| panic!("boom"));
    let err = result.expect_err("panic is caught").to_string();
    assert!(err.contains("PDF engine"), "{err}");
}

#[test]
fn deadline_expires() {
    let limits = ParseLimits {
        pdf_timeout: Duration::from_millis(1),
        ..ParseLimits::default()
    };
    let deadline = Deadline::new(&limits);
    assert!(deadline.check().is_ok());
    std::thread::sleep(Duration::from_millis(5));
    let err = deadline.check().expect_err("expired").to_string();
    assert!(err.contains("pdf_timeout"), "{err}");
}

#[test]
fn skewed_text_in_a_real_pdf() {
    // A 2 degree text matrix, as on a slightly tilted scan's text layer.
    let mut content = String::new();
    for (i, line) in [
        "first line of skewed text here",
        "second line of skewed text here",
    ]
    .iter()
    .enumerate()
    {
        let y = 700 - 14 * i;
        write!(
            content,
            "BT /F1 12 Tf 0.99939 0.0349 -0.0349 0.99939 72 {y} Tm ({line}) Tj ET "
        )
        .unwrap();
    }
    let doc = read(&pdf(&[(content.as_str(), "")], None));
    assert_eq!(
        block_texts(&doc.blocks),
        ["first line of skewed text here\nsecond line of skewed text here"]
    );
}

// --- structure ---------------------------------------------------------------------

fn heading(level: u8, text: &str) -> ParsedBlock {
    ParsedBlock::Heading {
        level,
        inlines: vec![Inline::plain(text)],
    }
}

fn item(level: u8, ordered: bool, text: &str) -> ParsedBlock {
    ParsedBlock::ListItem {
        level,
        ordered,
        blocks: vec![para(text)],
    }
}

/// A page of lines: each `(text, x, size)` on its own line, 14 pt apart
/// below 700 (more for larger text).
fn page_of(rows: &[(&str, f32, f32)]) -> Vec<Line> {
    let mut glyphs = Vec::new();
    let mut y = 700.0;
    for (i, (text, x, size)) in rows.iter().enumerate() {
        let seq = 1000 * u32::try_from(i).unwrap();
        glyphs.extend(glyphs_with(text, *x, y, seq, *size, Direction::Deg0));
        y -= 14.0_f32.max(size * 1.4);
    }
    lines_of(glyphs)
}

#[test]
fn headings_follow_size_rank() {
    let blocks = paragraphs(page_of(&[
        ("Big Title", 72.0, 24.0),
        ("Section One", 72.0, 16.0),
        ("Body text that is long enough.", 72.0, 12.0),
        ("More body text here.", 72.0, 12.0),
        ("Section Two", 72.0, 16.0),
        ("Final body line.", 72.0, 12.0),
    ]));
    assert_eq!(
        blocks,
        [
            heading(1, "Big Title"),
            heading(2, "Section One"),
            para("Body text that is long enough.\nMore body text here."),
            heading(2, "Section Two"),
            para("Final body line."),
        ]
    );
}

#[test]
fn large_text_that_reads_like_prose_is_not_a_heading() {
    let blocks = paragraphs(page_of(&[
        ("This sentence ends.", 72.0, 16.0),
        ("lowercase start", 72.0, 16.0),
        ("12 34 56", 72.0, 16.0),
        ("body body body body", 72.0, 12.0),
        ("body body body body", 72.0, 12.0),
        ("body body body body", 72.0, 12.0),
    ]));
    assert!(
        blocks
            .iter()
            .all(|b| !matches!(b, ParsedBlock::Heading { .. })),
        "{blocks:?}"
    );
}

#[test]
fn a_heading_may_span_lines() {
    let mut glyphs = glyphs_with("A Long Heading", 72.0, 700.0, 0, 18.0, Direction::Deg0);
    glyphs.extend(glyphs_with(
        "Over Two Lines",
        72.0,
        680.0,
        100,
        18.0,
        Direction::Deg0,
    ));
    glyphs.extend(word_glyphs(
        "Body text that carries the page.",
        72.0,
        650.0,
        200,
    ));
    glyphs.extend(word_glyphs(
        "Second body line of the page.",
        72.0,
        636.0,
        300,
    ));
    assert_eq!(
        paragraphs(lines_of(glyphs)),
        [
            heading(1, "A Long Heading Over Two Lines"),
            para("Body text that carries the page.\nSecond body line of the page.")
        ]
    );
}

#[test]
fn list_markers() {
    let blocks = paragraphs(page_of(&[
        ("Intro:", 72.0, 12.0),
        ("\u{2022} bullet one", 72.0, 12.0),
        ("- dash item", 72.0, 12.0),
        ("1. first", 72.0, 12.0),
        ("(2) second", 72.0, 12.0),
        ("iv. roman", 72.0, 12.0),
        ("a) letter", 72.0, 12.0),
    ]));
    assert_eq!(
        blocks,
        [
            para("Intro:"),
            item(0, false, "bullet one"),
            item(0, false, "dash item"),
            item(0, true, "first"),
            item(0, true, "second"),
            item(0, true, "roman"),
            item(0, true, "letter"),
        ]
    );
}

#[test]
fn years_bylines_and_rules_are_not_list_items() {
    for text in [
        "2020. was a year",
        "A. Smith wrote this",
        "1.5 percent",
        "-dash",
        "word \u{2022} inside",
    ] {
        let blocks = paragraphs(page_of(&[(text, 72.0, 12.0)]));
        assert_eq!(blocks, [para(text)], "{text}");
    }
}

#[test]
fn list_levels_follow_indent_and_items_continue() {
    let blocks = paragraphs(page_of(&[
        ("\u{2022} top item", 72.0, 12.0),
        ("continues here", 84.0, 12.0),
        ("\u{2022} nested item", 92.0, 12.0),
        ("\u{2022} back to top", 72.0, 12.0),
    ]));
    assert_eq!(
        blocks,
        [
            item(0, false, "top item\ncontinues here"),
            item(1, false, "nested item"),
            item(0, false, "back to top"),
        ]
    );
}

#[test]
fn standalone_rule_lines_are_rules_but_underlines_are_text() {
    let rows = [
        ("Title line", 700.0),
        ("==========", 686.0),
        ("Body after.", 640.0),
        ("more body", 626.0),
        ("and more", 612.0),
        ("* * * * *", 570.0),
        ("Last paragraph.", 530.0),
        ("ends here", 516.0),
    ];
    let mut glyphs = Vec::new();
    for (i, (text, y)) in rows.iter().enumerate() {
        glyphs.extend(word_glyphs(text, 72.0, *y, 100 * u32::try_from(i).unwrap()));
    }
    assert_eq!(
        paragraphs(lines_of(glyphs)),
        [
            para("Title line\n=========="),
            para("Body after.\nmore body\nand more"),
            ParsedBlock::HorizontalRule,
            para("Last paragraph.\nends here"),
        ]
    );
}

#[test]
fn hyphenated_words_are_joined() {
    let blocks = paragraphs(page_of(&[
        ("we hy-", 72.0, 12.0),
        ("phenate words but keep long-", 72.0, 12.0),
        ("Term compounds", 72.0, 12.0),
    ]));
    assert_eq!(
        blocks,
        [para("we hyphenate words but keep long-\nTerm compounds")]
    );
}

/// A page with an area of 612 x 792 and lines at the given baselines.
fn area_page(rows: &[(&str, f32)], seq_from: u32) -> Page {
    let mut glyphs = Vec::new();
    for (i, (text, y)) in rows.iter().enumerate() {
        glyphs.extend(word_glyphs(
            text,
            72.0,
            *y,
            seq_from + 100 * u32::try_from(i).unwrap(),
        ));
    }
    Page {
        lines: lines_of(glyphs),
        area: Some([0.0, 0.0, 612.0, 792.0]),
    }
}

#[test]
fn running_headers_and_page_numbers_are_dropped() {
    let pages: Vec<Page> = (1..=4)
        .map(|n| {
            let number = n.to_string();
            let body = format!("Body of page {n} with 12 in it.");
            area_page(
                &[
                    ("ACME Report", 770.0),
                    (body.as_str(), 400.0),
                    (number.as_str(), 30.0),
                ],
                1000 * n,
            )
        })
        .collect();
    let blocks = structure::blocks(pages);
    assert_eq!(count(&blocks, "ACME Report"), 1, "{blocks:?}");
    assert_eq!(count(&blocks, "2"), 0, "{blocks:?}");
    assert_eq!(count(&blocks, "Body of page 2 with 12 in it."), 1);
}

/// How many paragraphs read exactly `text`.
fn count(blocks: &[ParsedBlock], text: &str) -> usize {
    block_texts(blocks).iter().filter(|t| *t == text).count()
}

#[test]
fn short_documents_keep_their_margins() {
    let pages: Vec<Page> = (1..=2)
        .map(|n| area_page(&[("Footer text", 30.0), ("Body.", 400.0)], 1000 * n))
        .collect();
    let blocks = structure::blocks(pages);
    assert_eq!(count(&blocks, "Footer text"), 2);
}

#[test]
fn two_columns_are_read_column_by_column() {
    let mut glyphs = Vec::new();
    for i in 0..6_u32 {
        let y = 700.0 - 40.0 * f32::from(u16::try_from(i).unwrap());
        glyphs.extend(word_glyphs(&format!("left {i}"), 72.0, y, 100 * i));
        glyphs.extend(word_glyphs(&format!("right {i}"), 340.0, y, 100 * i + 50));
    }
    let page = Page {
        lines: lines_of(glyphs),
        area: Some([0.0, 0.0, 612.0, 792.0]),
    };
    let blocks = structure::blocks(vec![page]);
    let order: Vec<String> = block_texts(&blocks);
    let first_right = order.iter().position(|t| t.starts_with("right")).unwrap();
    assert!(
        order[..first_right].iter().all(|t| t.starts_with("left")),
        "{order:?}"
    );
}

#[test]
fn overprinted_text_is_kept_once_and_bold() {
    let mut glyphs = word_glyphs("Bold", 72.0, 700.0, 0);
    let mut copy = word_glyphs("Bold", 72.2, 700.1, 10);
    glyphs.append(&mut copy);
    let lines = lines_of(glyphs);
    assert_eq!(texts(&lines), ["Bold"]);
    assert!(lines[0].runs.iter().all(|r| r.bold));
}

#[test]
fn structure_from_a_real_pdf() {
    let content = "BT /F2 24 Tf 72 700 Td (Main Title) Tj ET \
        BT /F1 12 Tf 72 660 Td (This is body text that is long enough to wrap and we hy-) Tj 0 -14 Td (phenate a word across lines.) Tj ET \
        BT /F1 12 Tf 72 600 Td (\\225 bullet item one) Tj 0 -14 Td (1. ordered item) Tj ET";
    let doc = read(&pdf(&[(content, "")], None));
    assert_eq!(
        doc.blocks,
        [
            heading(1, "Main Title"),
            para(
                "This is body text that is long enough to wrap and we hyphenate a word across lines."
            ),
            item(0, false, "bullet item one"),
            item(0, true, "ordered item"),
        ]
    );
}

#[test]
fn side_by_side_labels_are_not_headings() {
    let mut glyphs = glyphs_with("Label One", 72.0, 700.0, 0, 16.0, Direction::Deg0);
    glyphs.extend(glyphs_with(
        "Label Two",
        300.0,
        700.0,
        50,
        16.0,
        Direction::Deg0,
    ));
    for i in 0..3_u32 {
        let y = 670.0 - 14.0 * f32::from(u16::try_from(i).unwrap());
        glyphs.extend(word_glyphs("body text body text", 72.0, y, 100 + 50 * i));
    }
    let blocks = paragraphs(lines_of(glyphs));
    assert!(
        blocks
            .iter()
            .all(|b| !matches!(b, ParsedBlock::Heading { .. })),
        "{blocks:?}"
    );
}

#[test]
fn long_runs_of_large_lines_are_labels() {
    let mut rows: Vec<(&str, f32, f32)> = vec![
        ("Alpha", 72.0, 16.0),
        ("Beta", 72.0, 16.0),
        ("Gamma", 72.0, 16.0),
        ("Delta", 72.0, 16.0),
        ("Epsilon", 72.0, 16.0),
    ];
    rows.extend([("body text here", 72.0, 12.0); 6]);
    let blocks = paragraphs(page_of(&rows));
    assert!(
        blocks
            .iter()
            .all(|b| !matches!(b, ParsedBlock::Heading { .. })),
        "{blocks:?}"
    );
}

#[test]
fn only_three_heading_levels() {
    let body = ("body text that is the body", 72.0, 12.0);
    let blocks = paragraphs(page_of(&[
        ("Level One", 72.0, 30.0),
        ("Level Two", 72.0, 24.0),
        body,
        ("Level Three", 72.0, 20.0),
        body,
        ("Level Four", 72.0, 16.0),
        body,
        ("Level Two Again", 72.0, 24.0),
        body,
        ("Level Three Again", 72.0, 20.0),
        body,
        ("Level Four Again", 72.0, 16.0),
        body,
    ]));
    let headings: Vec<&ParsedBlock> = blocks
        .iter()
        .filter(|b| matches!(b, ParsedBlock::Heading { .. }))
        .collect();
    assert_eq!(
        headings,
        [
            &heading(1, "Level One"),
            &heading(2, "Level Two"),
            &heading(3, "Level Three"),
            &heading(2, "Level Two Again"),
            &heading(3, "Level Three Again"),
        ]
    );
}

#[test]
fn rotated_and_hyphen_ended_lines_are_not_headings() {
    let mut glyphs = glyphs_with("Side Label", 100.0, 40.0, 0, 18.0, Direction::Deg90);
    glyphs.extend(glyphs_with(
        "Broken Word-",
        72.0,
        700.0,
        50,
        18.0,
        Direction::Deg0,
    ));
    for i in 0..3_u32 {
        let y = 670.0 - 14.0 * f32::from(u16::try_from(i).unwrap());
        glyphs.extend(word_glyphs("body text body text", 72.0, y, 100 + 50 * i));
    }
    let blocks = paragraphs(lines_of(glyphs));
    assert!(
        blocks
            .iter()
            .all(|b| !matches!(b, ParsedBlock::Heading { .. })),
        "{blocks:?}"
    );
}

// --- structure: review cases -------------------------------------------------------

/// A page of `(text, x, y)` lines on a 612 x 792 area.
fn page_at(rows: &[(&str, f32, f32)], seq_from: u32) -> Page {
    let mut glyphs = Vec::new();
    for (i, (text, x, y)) in rows.iter().enumerate() {
        glyphs.extend(word_glyphs(
            text,
            *x,
            *y,
            seq_from + 100 * u32::try_from(i).unwrap(),
        ));
    }
    Page {
        lines: lines_of(glyphs),
        area: Some([0.0, 0.0, 612.0, 792.0]),
    }
}

#[test]
fn numbers_in_the_margins_are_content_unless_they_count_pages() {
    let pages: Vec<Page> = (1..=12_u32)
        .map(|n| {
            let page_no = format!("Page {n} of 12");
            let bare = n.to_string();
            let balance = format!("Balance carried forward {}.15", 10 + 11 * n);
            page_at(
                &[
                    (page_no.as_str(), 250.0, 770.0),
                    ("Body text of the page.", 72.0, 400.0),
                    (balance.as_str(), 72.0, 40.0),
                    ("1,123.10", 400.0, 40.0),
                    (bare.as_str(), 300.0, 20.0),
                ],
                1000 * n,
            )
        })
        .collect();
    let blocks = structure::blocks(pages);
    let texts = block_texts(&blocks);
    assert!(texts.iter().all(|t| !t.starts_with("Page ")), "{texts:?}");
    assert!(
        !texts.contains(&"10".to_owned()) && !texts.contains(&"12".to_owned()),
        "{texts:?}"
    );
    assert_eq!(
        count(&blocks, "1,123.10"),
        12,
        "repeated figures are content"
    );
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.starts_with("Balance carried forward"))
            .count(),
        12,
        "{texts:?}"
    );
}

#[test]
fn alternating_running_heads_are_dropped() {
    let pages: Vec<Page> = (0..6_u32)
        .map(|n| {
            let head = if n % 2 == 0 {
                "J. Smith and A. Jones"
            } else {
                "A Short Title"
            };
            page_at(
                &[(head, 72.0, 770.0), ("Body text of the page.", 72.0, 400.0)],
                1000 * n,
            )
        })
        .collect();
    let blocks = structure::blocks(pages);
    assert_eq!(count(&blocks, "J. Smith and A. Jones"), 1);
    assert_eq!(count(&blocks, "A Short Title"), 1);
}

/// Two columns of prose, `rows` lines each, from `top` down.
fn two_columns(rows: u32, top: f32, seq_from: u32) -> Vec<(String, f32, f32)> {
    let mut out = Vec::new();
    for i in 0..rows {
        let y = top - 14.0 * f32::from(u16::try_from(i).unwrap());
        out.push((format!("left column prose line {i}"), 72.0, y));
        out.push((format!("right column prose line {i}"), 330.0, y));
    }
    let _ = seq_from;
    out
}

fn page_of_rows(rows: &[(String, f32, f32)]) -> Page {
    let borrowed: Vec<(&str, f32, f32)> =
        rows.iter().map(|(t, x, y)| (t.as_str(), *x, *y)).collect();
    page_at(&borrowed, 0)
}

#[test]
fn a_full_width_title_does_not_stop_column_reading() {
    let mut rows = vec![(
        "A title that spans the whole width of the page across both columns".to_owned(),
        72.0,
        740.0,
    )];
    rows.extend(two_columns(8, 700.0, 0));
    let blocks = structure::blocks(vec![page_of_rows(&rows)]);
    let texts = block_texts(&blocks).join("\n");
    let last_left = texts.find("left column prose line 7").unwrap();
    let first_right = texts.find("right column prose line 0").unwrap();
    assert!(last_left < first_right, "{texts}");
}

#[test]
fn label_and_amount_tables_stay_row_by_row() {
    let mut rows = Vec::new();
    for i in 0..12_u16 {
        let y = 700.0 - 14.0 * f32::from(i);
        rows.push((format!("Line item {i}"), 72.0, y));
        rows.push((format!("{}.00", 1000 + i), 450.0, y));
    }
    let blocks = structure::blocks(vec![page_of_rows(&rows)]);
    let texts = block_texts(&blocks);
    let first_amount = texts.iter().position(|t| t.contains("1000.00")).unwrap();
    assert!(
        texts[..first_amount]
            .iter()
            .filter(|t| t.starts_with("Line item"))
            .count()
            <= 1,
        "rows stay paired: {texts:?}"
    );
}

#[test]
fn a_column_heading_next_to_body_in_the_other_column_stays_a_heading() {
    let mut rows = two_columns(10, 700.0, 0);
    // A larger heading in the left column, level with a right-column line.
    rows.retain(|(t, _, _)| t != "left column prose line 3");
    let mut page = page_of_rows(&rows);
    let title = glyphs_with("Related Work", 72.0, 658.0, 9000, 16.0, Direction::Deg0);
    page.lines.extend(lines_of(title));
    page.lines.sort_by(|a, b| {
        b.baseline
            .total_cmp(&a.baseline)
            .then(a.left.total_cmp(&b.left))
    });
    let blocks = structure::blocks(vec![page]);
    assert!(blocks.contains(&heading(1, "Related Work")), "{blocks:?}");
}

#[test]
fn a_list_in_the_right_column_starts_at_level_zero() {
    let blocks = paragraphs(page_of(&[
        ("Intro text here:", 330.0, 12.0),
        ("\u{2022} first", 330.0, 12.0),
        ("\u{2022} second", 330.0, 12.0),
    ]));
    assert_eq!(blocks[1], item(0, false, "first"));
}

#[test]
fn large_bullet_lines_are_list_items() {
    let blocks = paragraphs(page_of(&[
        ("Slide Title", 72.0, 32.0),
        ("\u{2022} Revenue grew in all regions", 72.0, 24.0),
        ("\u{2022} Costs stayed flat", 72.0, 24.0),
        ("details below", 90.0, 18.0),
        ("details below", 90.0, 18.0),
        ("details below", 90.0, 18.0),
    ]));
    assert_eq!(blocks[0], heading(1, "Slide Title"));
    assert!(
        matches!(blocks[1], ParsedBlock::ListItem { .. }),
        "{blocks:?}"
    );
}

#[test]
fn a_cover_page_does_not_use_up_heading_levels() {
    let cover = Page {
        lines: page_of(&[
            ("Annual Report", 72.0, 36.0),
            ("Company Name", 72.0, 24.0),
            ("Year Edition", 72.0, 20.0),
        ]),
        area: None,
    };
    let section = |seq: &str| Page {
        lines: page_of(&[
            ("Overview", 72.0, 16.0),
            (seq, 72.0, 12.0),
            ("Market Position", 72.0, 13.5),
            ("body text of the section body text", 72.0, 11.0),
            ("body text of the section body text", 72.0, 11.0),
            ("body text of the section body text", 72.0, 11.0),
        ]),
        area: None,
    };
    let blocks = structure::blocks(vec![
        cover,
        section("more body text here"),
        section("more body text there"),
    ]);
    assert!(blocks.contains(&heading(1, "Annual Report")), "{blocks:?}");
    assert!(
        blocks.iter().any(|b| matches!(b, ParsedBlock::Heading { inlines, .. } if inlines == &[Inline::plain("Overview")])),
        "{blocks:?}"
    );
    assert!(
        blocks.iter().any(|b| matches!(b, ParsedBlock::Heading { inlines, .. } if inlines == &[Inline::plain("Market Position")])),
        "{blocks:?}"
    );
}

#[test]
fn a_dash_wrapping_into_a_paragraph_is_not_a_list() {
    let blocks = paragraphs(page_of(&[
        ("The project was approved in spring", 72.0, 12.0),
        (
            "\u{2013} which had been delayed \u{2013} and started",
            72.0,
            12.0,
        ),
        ("soon after the review.", 72.0, 12.0),
    ]));
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(matches!(blocks[0], ParsedBlock::Paragraph { .. }));
}

#[test]
fn first_line_indents_separate_paragraphs() {
    let blocks = paragraphs(page_of(&[
        ("First paragraph starts here and", 72.0, 12.0),
        ("goes on for a while.", 72.0, 12.0),
        ("Second paragraph is indented", 90.0, 12.0),
        ("and continues at the margin.", 72.0, 12.0),
    ]));
    assert_eq!(
        blocks,
        [
            para("First paragraph starts here and\ngoes on for a while."),
            para("Second paragraph is indented\nand continues at the margin."),
        ]
    );
}

#[test]
fn word_private_use_bullets_are_list_markers() {
    let blocks = paragraphs(page_of(&[("\u{f0b7} item", 72.0, 12.0)]));
    assert_eq!(blocks, [item(0, false, "item")]);
}

#[test]
fn a_page_of_rotated_text_has_headings_and_a_stamp_does_not_break_rules() {
    let mut glyphs = glyphs_with("Big Heading", 100.0, 300.0, 0, 20.0, Direction::Deg90);
    for i in 0..3_u16 {
        let y = 270.0 - 14.0 * f32::from(i);
        glyphs.extend(glyphs_with(
            "body text on a turned page",
            100.0,
            y,
            100 + 50 * u32::from(i),
            12.0,
            Direction::Deg90,
        ));
    }
    let blocks = paragraphs(lines_of(glyphs));
    assert_eq!(blocks[0], heading(1, "Big Heading"));

    let mut upright = Vec::new();
    let rows = [
        ("Body paragraph text.", 700.0),
        ("more body", 686.0),
        ("and more", 672.0),
        ("* * * * *", 630.0),
        ("After the rule.", 590.0),
        ("ends here", 576.0),
    ];
    for (i, (text, y)) in rows.iter().enumerate() {
        upright.extend(word_glyphs(text, 72.0, *y, 100 * u32::try_from(i).unwrap()));
    }
    upright.extend(glyphs_with(
        "CONFIDENTIAL DRAFT",
        300.0,
        580.0,
        5000,
        12.0,
        Direction::Deg270,
    ));
    let blocks = paragraphs(lines_of(upright));
    assert!(blocks.contains(&ParsedBlock::HorizontalRule), "{blocks:?}");
}

#[test]
fn a_soft_hyphen_at_a_line_end_joins_the_word() {
    let blocks = paragraphs(page_of(&[
        ("we hy\u{ad}", 72.0, 12.0),
        ("phenate here", 72.0, 12.0),
    ]));
    assert_eq!(blocks, [para("we hyphenate here")]);
}
