//! From glyphs to lines to blocks. Pure geometry; no pdfium.
//!
//! Glyphs are grouped by baseline direction and turned upright, so rotated
//! pages and rotated text read the same way. Within a direction, glyphs whose
//! baselines are close form a row; a row splits into separate lines at wide
//! gaps (columns, table cells). Words are separated where the gap between
//! glyphs is wide or the content had a space. Right-to-left runs, laid out
//! visually in the PDF, are put back into logical order. Lines become
//! paragraphs while the gap to the next line stays near the page's usual line
//! pitch.

use std::collections::HashMap;

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::{BLOCK_COST, TextBudget};

use super::engine::{Direction, Glyph};

/// A gap wider than this many font sizes ends a line on the same baseline.
const SEGMENT_GAP: f32 = 2.5;
/// A gap wider than this fraction of the font size separates words.
const WORD_GAP: f32 = 0.2;
/// Baselines closer than this fraction of the font size share a row.
const BASELINE_TOLERANCE: f32 = 0.35;
/// A line pitch above this multiple of the page's usual pitch starts a paragraph.
const PARAGRAPH_PITCH: f32 = 1.4;
/// Without a usual pitch, a gap above this many font sizes starts a paragraph.
const PARAGRAPH_GAP_FALLBACK: f32 = 2.0;
/// A size change above this (points) starts a paragraph.
const SIZE_CHANGE: f32 = 1.0;

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Run {
    pub(super) text: String,
    pub(super) bold: bool,
    pub(super) italic: bool,
}

/// One line of text, in its upright frame.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub(super) runs: Vec<Run>,
    pub(super) direction: Direction,
    pub(super) left: f32,
    pub(super) right: f32,
    pub(super) baseline: f32,
    /// The size most of the line's glyphs have.
    pub(super) size: f32,
}

impl Line {
    pub(super) fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// What building lines needs besides the glyphs.
struct Ctx<'a> {
    budget: &'a mut TextBudget,
    /// The page reads right to left.
    rtl: bool,
}

/// A glyph in its direction's upright frame.
struct Placed {
    glyph: Glyph,
    /// Baseline origin.
    x: f32,
    y: f32,
    /// Extent along the baseline.
    left: f32,
    right: f32,
}

/// The lines of one page, in reading order: left-to-right text top to
/// bottom, then text in other directions. The text is charged to `budget`,
/// with a block cost for each line and run.
pub(super) fn page_lines(
    glyphs: Vec<Glyph>,
    budget: &mut TextBudget,
) -> Result<Vec<Line>, DomainError> {
    // The page's base direction: right-to-left when most of its letters are.
    let (rtl, ltr) = glyphs.iter().fold((0_usize, 0_usize), |(r, l), g| {
        (
            r + usize::from(is_rtl(g.ch)),
            l + usize::from(is_strong_ltr(g.ch)),
        )
    });
    let mut ctx = Ctx {
        budget,
        rtl: rtl > ltr,
    };
    let mut by_direction: [Vec<Glyph>; 5] = Default::default();
    for glyph in glyphs {
        let slot = match glyph.direction {
            Direction::Deg0 => 0,
            Direction::Deg90 => 1,
            Direction::Deg270 => 2,
            Direction::Deg180 => 3,
            Direction::Other => 4,
        };
        by_direction[slot].push(glyph);
    }
    let mut lines = Vec::new();
    let [level, rising, falling, flipped, odd] = by_direction;
    for (direction, glyphs) in [
        (Direction::Deg0, level),
        (Direction::Deg90, rising),
        (Direction::Deg270, falling),
        (Direction::Deg180, flipped),
    ] {
        rows(direction, glyphs, &mut ctx, &mut lines)?;
    }
    content_order_lines(odd, &mut ctx, &mut lines)?;
    Ok(lines)
}

/// Rotates a point by `-angle` degrees, into the frame where text at
/// `angle` runs left to right.
fn upright(angle: f32, x: f32, y: f32) -> (f32, f32) {
    let (sin, cos) = angle.to_radians().sin_cos();
    (x * cos + y * sin, y * cos - x * sin)
}

fn place(angle: f32, glyph: Glyph) -> Placed {
    let (along, baseline) = upright(angle, glyph.x, glyph.y);
    let b = glyph.bounds;
    let corners = [
        (b.left, b.bottom),
        (b.left, b.top),
        (b.right, b.bottom),
        (b.right, b.top),
    ]
    .map(|(cx, cy)| upright(angle, cx, cy).0);
    let left = corners.iter().copied().fold(f32::INFINITY, f32::min);
    let right = corners.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    Placed {
        glyph,
        x: along,
        y: baseline,
        left,
        right,
    }
}

/// The angle that turns a direction's glyphs upright: its nominal angle plus
/// the median deviation of the glyphs from it, so slightly skewed text (a
/// tilted scan's text layer) still lines up.
fn frame_angle(direction: Direction, glyphs: &[Glyph]) -> f32 {
    let nominal = match direction {
        Direction::Deg0 | Direction::Other => 0.0,
        Direction::Deg90 => 90.0,
        Direction::Deg180 => 180.0,
        Direction::Deg270 => 270.0,
    };
    let mut deviations: Vec<f32> = glyphs
        .iter()
        .map(|g| (g.angle - nominal + 180.0).rem_euclid(360.0) - 180.0)
        .filter(|d| d.is_finite())
        .collect();
    deviations.sort_by(f32::total_cmp);
    nominal
        + deviations
            .get(deviations.len() >> 1)
            .copied()
            .unwrap_or(0.0)
}

/// Groups glyphs of one direction into rows by baseline and splits rows into
/// lines at wide gaps.
fn rows(
    direction: Direction,
    glyphs: Vec<Glyph>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<Line>,
) -> Result<(), DomainError> {
    let angle = frame_angle(direction, &glyphs);
    let mut placed: Vec<Placed> = glyphs.into_iter().map(|g| place(angle, g)).collect();
    // Top to bottom, then left to right.
    placed.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    let mut row: Vec<Placed> = Vec::new();
    let mut row_y = 0.0_f32;
    let mut row_size = 0.0_f32;
    for p in placed {
        // The smaller size: one large glyph (a drop cap) must not pull in
        // the line above it.
        let tolerance = BASELINE_TOLERANCE * row_size.min(p.glyph.size);
        if !row.is_empty() && (row_y - p.y).abs() > tolerance {
            segments(direction, std::mem::take(&mut row), ctx, out)?;
        }
        if row.is_empty() {
            row_y = p.y;
            row_size = p.glyph.size;
        }
        row.push(p);
    }
    if !row.is_empty() {
        segments(direction, row, ctx, out)?;
    }
    Ok(())
}

fn segments(
    direction: Direction,
    mut row: Vec<Placed>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<Line>,
) -> Result<(), DomainError> {
    row.sort_by(|a, b| {
        a.left
            .total_cmp(&b.left)
            .then(a.glyph.seq.cmp(&b.glyph.seq))
    });
    let mut start = 0;
    for i in 1..=row.len() {
        let split = match (row.get(i - 1), row.get(i)) {
            (Some(prev), Some(next)) => {
                let gap = next.left - prev.right;
                gap > SEGMENT_GAP * prev.glyph.size.max(next.glyph.size)
            }
            _ => true,
        };
        if split {
            if let Some(glyphs) = row.get(start..i)
                && let Some(line) = build_line(direction, glyphs, ctx)?
            {
                out.push(line);
            }
            start = i;
        }
    }
    Ok(())
}

/// A styled character of a line under construction.
#[derive(Clone, Copy)]
struct Token {
    ch: char,
    bold: bool,
    italic: bool,
}

fn build_line(
    direction: Direction,
    glyphs: &[Placed],
    ctx: &mut Ctx<'_>,
) -> Result<Option<Line>, DomainError> {
    let (Some(first), Some(last)) = (glyphs.first(), glyphs.last()) else {
        return Ok(None);
    };
    let kept = without_overprint(glyphs);
    let mut tokens: Vec<Token> = Vec::with_capacity(kept.len());
    let mut prev: Option<&Placed> = None;
    for &(p, overprinted) in &kept {
        if let Some(prev) = prev
            && needs_space(prev, p)
        {
            tokens.push(Token {
                ch: ' ',
                bold: prev.glyph.bold && p.glyph.bold,
                italic: prev.glyph.italic && p.glyph.italic,
            });
        }
        push_normalized(&mut tokens, &p.glyph, overprinted);
        prev = Some(p);
    }
    // A soft hyphen ending the line marks a word broken there: keep it as a
    // hyphen so the paragraph can join the word.
    if kept.last().is_some_and(|(p, _)| p.glyph.ch == '\u{ad}') {
        tokens.push(Token {
            ch: '-',
            bold: false,
            italic: false,
        });
    }
    reorder_rtl(&mut tokens, ctx.rtl);
    let runs = runs(&tokens);
    let bytes: usize = runs.iter().map(|r| r.text.len()).sum();
    if bytes == 0 {
        return Ok(None);
    }
    // The line and each run cost memory beyond their text: a page of
    // single-glyph lines or runs must not be charged a byte each.
    let pieces = runs.len().saturating_add(1);
    ctx.budget
        .charge(bytes.saturating_add(pieces.saturating_mul(BLOCK_COST)))?;
    Ok(Some(Line {
        runs,
        direction,
        left: first.left,
        right: last.right.max(first.right),
        baseline: first.y,
        size: common_size(glyphs),
    }))
}

fn needs_space(prev: &Placed, next: &Placed) -> bool {
    // Neighbours in content order: pdfium's own spacing (spaces in the
    // content, or ones it inserted for gaps) decides, whichever way the line
    // runs.
    let (lo, hi) = if prev.glyph.seq < next.glyph.seq {
        (&prev.glyph, &next.glyph)
    } else {
        (&next.glyph, &prev.glyph)
    };
    if hi.seq == lo.seq + 1 {
        return hi.space_before;
    }
    // Otherwise the gap decides; ideographic scripts use no spaces.
    if is_cjk(prev.glyph.ch) && is_cjk(next.glyph.ch) {
        return false;
    }
    let gap = next.left - prev.right;
    gap > WORD_GAP * f32::midpoint(prev.glyph.size, next.glyph.size)
}

/// Pushes the glyph's character, expanding ligatures and turning special
/// spaces into plain ones.
fn push_normalized(tokens: &mut Vec<Token>, glyph: &Glyph, overprinted: bool) {
    let bold = glyph.bold || overprinted;
    let expanded: &str = match glyph.ch {
        '\u{fb00}' => "ff",
        '\u{fb01}' => "fi",
        '\u{fb02}' => "fl",
        '\u{fb03}' => "ffi",
        '\u{fb04}' => "ffl",
        '\u{fb05}' | '\u{fb06}' => "st",
        '\u{ad}' => "",
        _ => {
            tokens.push(Token {
                ch: glyph.ch,
                bold,
                italic: glyph.italic,
            });
            return;
        }
    };
    tokens.extend(expanded.chars().map(|ch| Token {
        ch,
        bold,
        italic: glyph.italic,
    }));
}

/// The glyphs of a line without overprinted copies: a character drawn again
/// within a third of a point (fake bold) is kept once and marked bold.
fn without_overprint(glyphs: &[Placed]) -> Vec<(&Placed, bool)> {
    let mut kept: Vec<(&Placed, bool)> = Vec::with_capacity(glyphs.len());
    for p in glyphs {
        let start = kept.len().saturating_sub(8);
        let copy = kept.get_mut(start..).and_then(|recent| {
            recent.iter_mut().find(|(k, _)| {
                k.glyph.ch == p.glyph.ch && (k.x - p.x).abs() < 0.3 && (k.y - p.y).abs() < 0.3
            })
        });
        match copy {
            Some((_, bold)) => *bold = true,
            None => kept.push((p, false)),
        }
    }
    kept
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFF00..=0xFFEF // Half- and full-width forms
        | 0x3000..=0x303F // CJK punctuation
    )
}

fn is_rtl(ch: char) -> bool {
    matches!(ch as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF) && !is_digit(ch)
}

/// A letter that is not right-to-left.
fn is_strong_ltr(ch: char) -> bool {
    ch.is_alphabetic() && !is_rtl(ch)
}

/// European, Arabic-Indic and extended Arabic-Indic digits: numbers read left
/// to right in any script.
fn is_digit(ch: char) -> bool {
    ch.is_ascii_digit() || matches!(ch as u32, 0x0660..=0x0669 | 0x06F0..=0x06F9)
}

/// Puts right-to-left text, which the line holds in visual order, into
/// logical order.
///
/// A line reads right to left when it has no left-to-right letters, or when
/// the page mostly reads right to left (`page_rtl`); pdfium's character order
/// cannot tell, as it applies its own bidi ordering (which changed between
/// pdfium builds). In a left-to-right line each run from an RTL letter to the
/// last RTL letter before a left-to-right letter is reversed, and the neutrals
/// around it stay put. In a right-to-left line the whole line is reversed and
/// the left-to-right runs are turned back. Numbers (digits with single `.`,
/// `,`, `:` or `/` between them) always keep their order; brackets in
/// reversed text are mirrored.
fn reorder_rtl(tokens: &mut [Token], page_rtl: bool) {
    if !tokens.iter().any(|t| is_rtl(t.ch)) {
        return;
    }
    let rtl_base = page_rtl || !tokens.iter().any(|t| is_strong_ltr(t.ch));
    if rtl_base {
        reverse_mirrored(tokens);
        let mut kept = vec![false; tokens.len()];
        for (start, end) in spans(tokens, is_strong_ltr, is_rtl) {
            if let Some(run) = tokens.get_mut(start..=end) {
                reverse_mirrored(run);
            }
            if let Some(k) = kept.get_mut(start..=end) {
                k.fill(true);
            }
        }
        for (start, end) in number_spans(tokens) {
            let untouched = kept.get(start..=end).is_some_and(|k| !k.contains(&true));
            if untouched && let Some(number) = tokens.get_mut(start..=end) {
                number.reverse();
            }
        }
    } else {
        for (start, end) in spans(tokens, is_rtl, is_strong_ltr) {
            if let Some(run) = tokens.get_mut(start..=end) {
                reverse_mirrored(run);
                for (s, e) in number_spans(run) {
                    if let Some(number) = run.get_mut(s..=e) {
                        number.reverse();
                    }
                }
            }
        }
    }
}

fn reverse_mirrored(run: &mut [Token]) {
    run.reverse();
    for t in run.iter_mut() {
        t.ch = mirror(t.ch);
    }
}

/// Spans from a token where `starts` holds to the last such token before one
/// where `stops` holds (inclusive ends).
fn spans(
    tokens: &[Token],
    starts: fn(char) -> bool,
    stops: fn(char) -> bool,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !tokens.get(i).is_some_and(|t| starts(t.ch)) {
            i += 1;
            continue;
        }
        let mut end = i;
        let mut j = i + 1;
        while let Some(t) = tokens.get(j) {
            if stops(t.ch) {
                break;
            }
            if starts(t.ch) {
                end = j;
            }
            j += 1;
        }
        out.push((i, end));
        i = end + 1;
    }
    out
}

/// Numbers: runs of digits with single separators between digits.
fn number_spans(tokens: &[Token]) -> Vec<(usize, usize)> {
    let digit = |i: usize| tokens.get(i).is_some_and(|t| is_digit(t.ch));
    let separator = |i: usize| {
        tokens
            .get(i)
            .is_some_and(|t| matches!(t.ch, '.' | ',' | ':' | '/'))
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if !digit(i) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut j = i + 1;
        while j < tokens.len() {
            if digit(j) {
                end = j;
                j += 1;
            } else if separator(j) && digit(j + 1) {
                j += 1;
            } else {
                break;
            }
        }
        out.push((start, end));
        i = end + 1;
    }
    out
}

fn mirror(ch: char) -> char {
    match ch {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        other => other,
    }
}

fn runs(tokens: &[Token]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for t in tokens {
        match runs.last_mut() {
            Some(run) if run.bold == t.bold && run.italic == t.italic => run.text.push(t.ch),
            _ => runs.push(Run {
                text: t.ch.to_string(),
                bold: t.bold,
                italic: t.italic,
            }),
        }
    }
    runs
}

/// The size most glyphs have, in half-point bins; ties go to the smaller.
fn common_size(glyphs: &[Placed]) -> f32 {
    let mut bins: HashMap<i32, usize> = HashMap::new();
    for p in glyphs {
        *bins.entry(half_points(p.glyph.size)).or_insert(0) += 1;
    }
    bins.iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map_or(0.0, |(bin, _)| from_half_points(*bin))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to 0..=1e6 half points first"
)]
pub(super) fn half_points(size: f32) -> i32 {
    (size * 2.0).round().clamp(0.0, 1.0e6) as i32
}

#[expect(clippy::cast_precision_loss, reason = "bins stay far below 2^24")]
pub(super) fn from_half_points(bin: i32) -> f32 {
    bin as f32 / 2.0
}

/// Lines of glyphs at odd angles, in content order: each unbroken run of
/// content positions is one line.
fn content_order_lines(
    mut glyphs: Vec<Glyph>,
    ctx: &mut Ctx<'_>,
    out: &mut Vec<Line>,
) -> Result<(), DomainError> {
    glyphs.sort_by_key(|g| g.seq);
    let mut start = 0;
    for i in 1..=glyphs.len() {
        let split = match (glyphs.get(i - 1), glyphs.get(i)) {
            (Some(a), Some(b)) => b.seq != a.seq + 1,
            _ => true,
        };
        if split {
            if let Some(run) = glyphs.get(start..i) {
                let placed: Vec<Placed> = run
                    .iter()
                    .cloned()
                    .map(|g| {
                        let (x, y) = (g.x, g.y);
                        Placed {
                            glyph: g,
                            x,
                            y,
                            left: f32::NEG_INFINITY,
                            right: f32::NEG_INFINITY,
                        }
                    })
                    .collect();
                if let Some(mut line) = build_line(Direction::Other, &placed, ctx)? {
                    line.left = 0.0;
                    line.right = 0.0;
                    out.push(line);
                }
            }
            start = i;
        }
    }
    Ok(())
}

pub(super) fn starts_paragraph(prev: &Line, line: &Line, pitch: Option<f32>) -> bool {
    if prev.direction != line.direction
        || line.direction == Direction::Other
        || (prev.size - line.size).abs() > SIZE_CHANGE
    {
        return true;
    }
    let gap = prev.baseline - line.baseline;
    if gap <= 0.0 {
        // Another line on the same baseline: a column or a table cell.
        return true;
    }
    match pitch {
        Some(pitch) => gap > PARAGRAPH_PITCH * pitch,
        None => gap > PARAGRAPH_GAP_FALLBACK * line.size.max(prev.size),
    }
}

/// The median distance between consecutive baselines of same-sized lines,
/// ignoring gaps too wide to be line spacing.
pub(super) fn usual_pitch(lines: &[Line]) -> Option<f32> {
    let mut pitches: Vec<f32> = lines
        .windows(2)
        .filter_map(|w| match w {
            [a, b] if a.direction == b.direction && (a.size - b.size).abs() <= SIZE_CHANGE => {
                let gap = a.baseline - b.baseline;
                (gap > 0.0 && gap < 4.0 * a.size.max(b.size)).then_some(gap)
            }
            _ => None,
        })
        .collect();
    if pitches.is_empty() {
        return None;
    }
    pitches.sort_by(f32::total_cmp);
    pitches.get(pitches.len() >> 1).copied()
}
