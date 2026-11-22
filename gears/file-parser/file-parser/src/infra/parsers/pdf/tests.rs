use std::fmt::Write as _;
use std::time::Duration;

use super::engine::{Deadline, Direction, Glyph, PDFIUM_LOCK, clean_title, guarded};
use super::layout::{Line, page_lines, paragraphs};
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
    assert_eq!(paragraphs(&lines), [para("left"), para("right")]);
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
    let blocks = paragraphs(&lines_of(glyphs));
    assert_eq!(blocks, [para("line0\nline1\nline2"), para("line3\nline4")]);
}

#[test]
fn a_size_change_starts_a_paragraph() {
    let mut glyphs = glyphs_with("Title", 72.0, 720.0, 0, 24.0, Direction::Deg0);
    glyphs.extend(word_glyphs("body one", 72.0, 700.0, 10));
    glyphs.extend(word_glyphs("body two", 72.0, 686.0, 20));
    assert_eq!(
        paragraphs(&lines_of(glyphs)),
        [para("Title"), para("body one\nbody two")]
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
        paragraphs(&lines_of(glyphs)),
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
            ParsedBlock::Paragraph { inlines } => inlines
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
    let content =
        "BT /F2 24 Tf 72 700 Td (Main Title) Tj ET BT /F1 12 Tf 72 660 Td (Body text here.) Tj ET";
    let doc = read(&pdf(&[(content, "")], Some(b"  Real Title  ")));
    assert_eq!(doc.title.as_deref(), Some("Real Title"));
    assert_eq!(block_texts(&doc.blocks), ["Main Title", "Body text here."]);
    let ParsedBlock::Paragraph { inlines } = &doc.blocks[0] else {
        panic!("{:?}", doc.blocks);
    };
    assert!(
        matches!(&inlines[0], Inline::Text { style, .. } if style.bold),
        "Helvetica-Bold is bold: {inlines:?}"
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
