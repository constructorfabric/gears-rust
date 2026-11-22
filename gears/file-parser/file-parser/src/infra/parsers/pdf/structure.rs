//! Document structure from lines: headings, list items, rules, paragraphs.
//!
//! The rules are deliberately few, each with a test:
//! - Running headers and footers (the same text at the same place in the top
//!   or bottom band of most pages, or of most odd or even pages, of a
//!   document of three or more pages) are dropped after their first
//!   occurrence; page numbers (numbers there that grow with the page) are
//!   dropped everywhere.
//! - Between lines that span the text width, a clear vertical gutter with
//!   prose on both sides makes the lines read column by column.
//! - The body size is the size most of the document's characters have.
//!   Lines clearly larger than it, short, alone on their line, without
//!   sentence punctuation at the end, are heading candidates. The largest
//!   candidate size and the two used on most pages are headings, levels by
//!   size.
//! - A line starting with a bullet or a short number or letter marker is a
//!   list item; its level follows its indent within the list.
//! - A standalone line of rule characters is a horizontal rule.
//! - Other lines form paragraphs while the gap to the next line stays near the
//!   page's usual line pitch and no first-line indent starts a new one. A word
//!   hyphenated across two lines is joined.

use std::collections::{HashMap, HashSet};

use crate::domain::error::DomainError;
use crate::domain::ir::{Inline, InlineStyle, ParsedBlock};
use crate::infra::parsers::inlines::Inlines;
use crate::infra::parsers::limits::TextBudget;

use super::engine::{Direction, Rect};
use super::layout::{Line, Run, from_half_points, half_points, starts_paragraph, usual_pitch};

/// Running headers and footers live in this fraction of the page height at
/// the top and the bottom.
const MARGIN_BAND: f32 = 0.08;
/// A margin line repeated on at least this share of pages is running text.
const RUNNING_SHARE: f32 = 0.6;
/// Fewer pages than this have no running headers or footers to detect.
const RUNNING_MIN_PAGES: usize = 3;
/// Positions of running text are compared in steps of this share of the
/// page height.
const RUNNING_POSITION_STEP: f32 = 0.02;
/// A line wider than this share of the text width spans the columns.
const FULL_WIDTH: f32 = 0.6;
/// A gutter is at least this share of the page width.
const GUTTER_MIN: f32 = 0.02;
/// Lines needed on each side of a gutter.
const COLUMN_MIN_LINES: usize = 5;
/// A column of prose has lines at least this share of the text width...
const COLUMN_MIN_LINE_WIDTH: f32 = 0.25;
/// ...and at least this share of letters among its characters.
const COLUMN_MIN_LETTERS: f32 = 0.5;
/// A heading is at least this many times the body size...
const HEADING_RATIO: f32 = 1.15;
/// ...and at least this many points larger.
const HEADING_MIN_STEP: f32 = 1.0;
/// Words in a heading line at most.
const HEADING_MAX_WORDS: usize = 20;
/// Adjacent heading candidates allowed before they read as labels instead.
const HEADING_MAX_RUN: usize = 4;
/// Heading sizes kept besides the largest.
const HEADING_TIERS: usize = 2;
/// Lines one heading may span.
const HEADING_MAX_LINES: usize = 3;
/// Deepest list level.
const MAX_LIST_LEVEL: usize = 8;
/// A rule spans at least this share of the page's text width.
const RULE_MIN_WIDTH: f32 = 0.4;

/// The lines of one page and its visible area.
pub(super) struct Page {
    pub(super) lines: Vec<Line>,
    pub(super) area: Option<Rect>,
}

/// A page being structured.
struct Sheet {
    lines: Vec<Line>,
    /// Text of each line, computed once.
    texts: Vec<String>,
    /// Column of each line.
    columns: Vec<Column>,
    /// The direction most of the page's text runs in; "upright" for the
    /// rules below.
    dominant: Direction,
}

impl Sheet {
    fn upright(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(|l| l.direction == self.dominant)
    }

    /// Leftmost and rightmost extent of the page's upright text.
    fn text_span(&self) -> (f32, f32) {
        let left = self.upright().map(|l| l.left).fold(f32::INFINITY, f32::min);
        let right = self
            .upright()
            .map(|l| l.right)
            .fold(f32::NEG_INFINITY, f32::max);
        (left, right)
    }
}

/// The blocks of a document, pages separated by page breaks. Line text was
/// charged to `budget` when the lines were built; the line breaks and spaces
/// that join lines here, and the blocks, are charged now.
pub(super) fn blocks(
    mut pages: Vec<Page>,
    budget: &mut TextBudget,
) -> Result<Vec<ParsedBlock>, DomainError> {
    remove_running_text(&mut pages);
    let sheets: Vec<Sheet> = pages.into_iter().map(prepare).collect();
    let body = body_size(&sheets);
    let mut headings: Vec<Vec<bool>> = sheets.iter().map(|s| heading_candidates(s, body)).collect();
    let tiers = Tiers::choose(&sheets, &mut headings);
    let mut out = Vec::new();
    for (i, sheet) in sheets.iter().enumerate() {
        if i > 0 {
            budget.charge_block()?;
            out.push(ParsedBlock::PageBreak);
        }
        let headings = headings.get(i).map_or(&[][..], Vec::as_slice);
        assemble(sheet, headings, &tiers, budget, &mut out)?;
    }
    Ok(out)
}

fn letters_of(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

fn dominant_direction(lines: &[Line]) -> Direction {
    let mut counts: Vec<(Direction, usize)> = Vec::new();
    for line in lines {
        let n: usize = line.runs.iter().map(|r| letters_of(&r.text)).sum();
        match counts.iter_mut().find(|(d, _)| *d == line.direction) {
            Some((_, c)) => *c += n,
            None => counts.push((line.direction, n)),
        }
    }
    counts
        .into_iter()
        .filter(|(d, _)| *d != Direction::Other)
        .max_by_key(|(_, c)| *c)
        .map_or(Direction::Deg0, |(d, _)| d)
}

fn prepare(page: Page) -> Sheet {
    let dominant = dominant_direction(&page.lines);
    let texts = page.lines.iter().map(Line::text).collect();
    let columns = vec![Column::Spanning; page.lines.len()];
    let mut sheet = Sheet {
        lines: page.lines,
        texts,
        columns,
        dominant,
    };
    read_columns(&mut sheet);
    sheet
}

// --- running headers and footers ----------------------------------------------

/// The margin band and position step of a line, if it sits in a band.
fn band(line: &Line, area: Option<Rect>) -> Option<(bool, i32)> {
    let Rect { bottom, top, .. } = area?;
    let height = top - bottom;
    if line.direction != Direction::Deg0 || height.is_nan() || height <= 0.0 {
        return None;
    }
    let relative = (line.baseline - bottom) / height;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "relative is within 0..=1 here"
    )]
    let step = (relative / RUNNING_POSITION_STEP).round() as i32;
    if relative > 1.0 - MARGIN_BAND && relative <= 1.0 {
        Some((true, step))
    } else if (0.0..MARGIN_BAND).contains(&relative) {
        Some((false, step))
    } else {
        None
    }
}

/// Band, position step and text of a margin line.
type Key = (bool, i32, String);

/// Pages a masked margin line appears on, with its numbers there.
type Occurrences<'a> = Vec<(usize, &'a [u32])>;

/// A margin line, as running-text detection sees it.
struct Margin {
    /// Band, position step and the text, lower-cased.
    exact: Key,
    /// The same with each run of digits replaced by one `#`.
    masked: Key,
    /// The numbers in the line (up to four digits each).
    numbers: Vec<u32>,
    has_letters: bool,
}

fn margin(line: &Line, area: Option<Rect>) -> Option<Margin> {
    let (side, step) = band(line, area)?;
    let text = line.text().trim().to_lowercase();
    let mut masked = String::with_capacity(text.len());
    let mut numbers = Vec::new();
    let mut digits = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        if !digits.is_empty() {
            masked.push('#');
            if digits.len() <= 4
                && let Ok(n) = digits.parse()
            {
                numbers.push(n);
            }
            digits.clear();
        }
        masked.push(c);
    }
    masked.pop();
    let has_letters = text.chars().any(char::is_alphabetic);
    Some(Margin {
        exact: (side, step, text),
        masked: (side, step, masked),
        numbers,
        has_letters,
    })
}

/// Whether pages `on` (sorted, distinct) make up most of the document, or
/// most of its odd or even pages.
fn repeats(on: &[usize], pages: usize) -> bool {
    #[expect(clippy::cast_precision_loss, reason = "page counts are small")]
    let share = |count: usize, of: usize| count >= 2 && count as f32 >= RUNNING_SHARE * of as f32;
    let even = on.iter().filter(|p| *p % 2 == 0).count();
    let odd = on.len() - even;
    share(on.len(), pages) || share(even, pages.div_ceil(2)) || share(odd, pages >> 1)
}

fn remove_running_text(pages: &mut [Page]) {
    if pages.len() < RUNNING_MIN_PAGES {
        return;
    }
    let margins: Vec<Vec<Option<Margin>>> = pages
        .iter()
        .map(|p| p.lines.iter().map(|l| margin(l, p.area)).collect())
        .collect();
    let mut exact: HashMap<&Key, Vec<usize>> = HashMap::new();
    let mut masked: HashMap<&Key, Occurrences<'_>> = HashMap::new();
    for (page, lines) in margins.iter().enumerate() {
        for m in lines.iter().flatten() {
            let on = exact.entry(&m.exact).or_default();
            if on.last() != Some(&page) {
                on.push(page);
            }
            if !m.numbers.is_empty() {
                let occ = masked.entry(&m.masked).or_default();
                if occ.last().is_none_or(|(p, _)| *p != page) {
                    occ.push((page, &m.numbers));
                }
            }
        }
    }
    let running: HashSet<&Key> = exact
        .iter()
        .filter(|(_, on)| repeats(on, pages.len()))
        .map(|(k, _)| *k)
        .collect();
    let numbered: HashSet<&Key> = masked
        .iter()
        .filter(|(_, occ)| {
            let on: Vec<usize> = occ.iter().map(|(p, _)| *p).collect();
            repeats(&on, pages.len()) && tracks_page(occ)
        })
        .map(|(k, _)| *k)
        .collect();

    let mut kept_once: HashSet<Key> = HashSet::new();
    let mut remove: Vec<Vec<bool>> = Vec::with_capacity(pages.len());
    for lines in &margins {
        remove.push(
            lines
                .iter()
                .map(|m| {
                    let Some(m) = m else { return false };
                    if numbered.contains(&m.masked) {
                        return true;
                    }
                    // Repeated numbers alone (a figure in a table cell) are
                    // content; running text has words.
                    if m.has_letters && running.contains(&m.exact) {
                        return !kept_once.insert(m.exact.clone());
                    }
                    false
                })
                .collect(),
        );
    }
    for (page, flags) in pages.iter_mut().zip(remove) {
        let mut flags = flags.into_iter();
        page.lines.retain(|_| !flags.next().unwrap_or(false));
    }
}

/// Whether one of the numbers of a repeated margin line grows with the page
/// (a page number), on nearly all of its occurrences.
fn tracks_page(occurrences: &[(usize, &[u32])]) -> bool {
    let width = occurrences.iter().map(|(_, n)| n.len()).max().unwrap_or(0);
    (0..width).any(|j| {
        let mut offsets: HashMap<i64, usize> = HashMap::new();
        for (page, numbers) in occurrences {
            if let (Some(n), Ok(page)) = (numbers.get(j), i64::try_from(*page)) {
                *offsets.entry(i64::from(*n) - page).or_insert(0) += 1;
            }
        }
        let best = offsets.values().copied().max().unwrap_or(0);
        best >= 2 && best * 5 >= occurrences.len() * 4
    })
}

// --- columns ----------------------------------------------------------------------

/// Where a line sits in a two-column band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    /// Outside any two-column band.
    Spanning,
    Left,
    Right,
}

/// Reads two-column parts of a page column by column. The page is cut into
/// bands at lines that span the text width; each band with a gutter and prose
/// on both sides is reordered: its left lines, then its right lines.
fn read_columns(sheet: &mut Sheet) {
    let Some(start) = sheet
        .lines
        .iter()
        .position(|l| l.direction == sheet.dominant)
    else {
        return;
    };
    let end = start
        + sheet
            .lines
            .iter()
            .skip(start)
            .take_while(|l| l.direction == sheet.dominant)
            .count();
    let (text_left, text_right) = sheet.text_span();
    let width = text_right - text_left;
    if width.is_nan() || width <= 0.0 {
        return;
    }
    let mut band_start = start;
    for i in start..=end {
        let full = sheet
            .lines
            .get(i)
            .is_some_and(|l| l.right - l.left >= FULL_WIDTH * width);
        if i == end || full {
            if i > band_start {
                split_band(sheet, band_start, i, width);
            }
            band_start = i + 1;
        }
    }
}

fn split_band(sheet: &mut Sheet, start: usize, end: usize, width: f32) {
    let Some(band) = sheet.lines.get(start..end) else {
        return;
    };
    let Some(texts) = sheet.texts.get(start..end) else {
        return;
    };
    let Some((_, gap_right)) = gutter(band, texts, width) else {
        return;
    };
    let mut order: Vec<usize> = (start..end).collect();
    let side = |i: &usize| {
        sheet
            .lines
            .get(*i)
            .map_or(0, |l| u8::from(l.left >= gap_right))
    };
    order.sort_by_key(side);
    let mut lines = Vec::with_capacity(order.len());
    let mut texts = Vec::with_capacity(order.len());
    let mut columns = Vec::with_capacity(order.len());
    for &i in &order {
        let (Some(line), Some(text)) = (sheet.lines.get(i), sheet.texts.get(i)) else {
            return;
        };
        columns.push(if line.left >= gap_right {
            Column::Right
        } else {
            Column::Left
        });
        lines.push(line.clone());
        texts.push(text.clone());
    }
    sheet.lines.splice(start..end, lines);
    sheet.texts.splice(start..end, texts);
    sheet.columns.splice(start..end, columns);
}

/// The widest vertical band no line of `band` crosses, away from its edges,
/// with enough prose on both sides.
fn gutter(band: &[Line], texts: &[String], width: f32) -> Option<(f32, f32)> {
    if band.len() < 2 * COLUMN_MIN_LINES {
        return None;
    }
    let left = band.iter().map(|l| l.left).fold(f32::INFINITY, f32::min);
    let right = band
        .iter()
        .map(|l| l.right)
        .fold(f32::NEG_INFINITY, f32::max);
    let span = right - left;
    let mut spans: Vec<(f32, f32)> = band.iter().map(|l| (l.left, l.right)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (lo, hi) = (left + 0.2 * span, right - 0.2 * span);
    let mut best: Option<(f32, f32)> = None;
    let mut reach = f32::NEG_INFINITY;
    for (start, end) in spans {
        if reach > f32::NEG_INFINITY && start > reach {
            let (a, b) = (reach.max(lo), start.min(hi));
            if b - a >= GUTTER_MIN * width && best.is_none_or(|(x, y)| b - a > y - x) {
                best = Some((a, b));
            }
        }
        reach = reach.max(end);
    }
    let (gap_left, gap_right) = best?;
    let prose = |pick: &dyn Fn(&Line) -> bool| {
        let side: Vec<(&Line, &String)> = band.iter().zip(texts).filter(|(l, _)| pick(l)).collect();
        if side.len() < COLUMN_MIN_LINES {
            return false;
        }
        let mut widths: Vec<f32> = side.iter().map(|(l, _)| l.right - l.left).collect();
        widths.sort_by(f32::total_cmp);
        let median = widths.get(widths.len() >> 1).copied().unwrap_or(0.0);
        let (letters, chars) = side.iter().fold((0_usize, 0_usize), |(a, c), (_, t)| {
            (
                a + t.chars().filter(|c| c.is_alphabetic()).count(),
                c + letters_of(t),
            )
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "character counts stay far below 2^24"
        )]
        let letter_share = letters as f32 / chars.max(1) as f32;
        median >= COLUMN_MIN_LINE_WIDTH * width && letter_share >= COLUMN_MIN_LETTERS
    };
    (prose(&|l| l.right <= gap_left) && prose(&|l| l.left >= gap_right))
        .then_some((gap_left, gap_right))
}

// --- sizes and headings -----------------------------------------------------------

/// The size most of the document's characters have; ties go to the smaller.
fn body_size(sheets: &[Sheet]) -> f32 {
    let mut bins: HashMap<i32, usize> = HashMap::new();
    for sheet in sheets {
        for (line, text) in sheet.lines.iter().zip(&sheet.texts) {
            *bins.entry(half_points(line.size)).or_insert(0) += letters_of(text);
        }
    }
    bins.iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map_or(0.0, |(bin, _)| from_half_points(*bin))
}

fn looks_like_heading(line: &Line, text: &str, body: f32, dominant: Direction) -> bool {
    // Rotated text (side labels, stamps, table headers) is never a heading.
    if line.direction != dominant
        || line.size < body * HEADING_RATIO
        || line.size < body + HEADING_MIN_STEP
    {
        return false;
    }
    let text = text.trim();
    // A large bullet line is a list item ("1. Introduction" may be a heading).
    if list_marker(text).is_some_and(|(ordered, _)| !ordered) {
        return false;
    }
    let alphabetic = text.chars().filter(|c| c.is_alphabetic()).count();
    let digits = text.chars().filter(|c| c.is_numeric()).count();
    alphabetic >= 2
        && digits <= alphabetic
        && text.split_whitespace().count() <= HEADING_MAX_WORDS
        && !text.ends_with(['.', ',', ';', '-'])
        && !text
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_lowercase)
}

/// For each line, whether no other line of its column shares its baseline.
/// Labels in diagrams and table cells sit side by side; headings stand alone.
fn alone_on_row(sheet: &Sheet) -> Vec<bool> {
    let lines = &sheet.lines;
    let mut order: Vec<usize> = (0..lines.len()).collect();
    let key = |i: &usize| lines.get(*i).map_or(0.0, |l| l.baseline);
    order.sort_by(|a, b| key(a).total_cmp(&key(b)));
    let mut alone = vec![true; lines.len()];
    for pair in order.windows(2) {
        let [a, b] = pair else { continue };
        let (Some(la), Some(lb)) = (lines.get(*a), lines.get(*b)) else {
            continue;
        };
        let tolerance = 0.3 * la.size.min(lb.size);
        let same_column = sheet.columns.get(*a) == sheet.columns.get(*b);
        if same_column
            && la.direction == lb.direction
            && (la.baseline - lb.baseline).abs() <= tolerance
        {
            for i in [*a, *b] {
                if let Some(flag) = alone.get_mut(i) {
                    *flag = false;
                }
            }
        }
    }
    alone
}

/// Heading candidates of a page: large enough (see [`looks_like_heading`]),
/// alone on their line, and not part of a run of more than
/// [`HEADING_MAX_RUN`] adjacent candidates (labels of a diagram, a table of
/// contents in large type).
fn heading_candidates(sheet: &Sheet, body: f32) -> Vec<bool> {
    let alone = alone_on_row(sheet);
    let mut flags: Vec<bool> = sheet
        .lines
        .iter()
        .zip(&sheet.texts)
        .zip(&alone)
        .map(|((l, t), alone)| *alone && looks_like_heading(l, t, body, sheet.dominant))
        .collect();
    let mut run: Vec<usize> = Vec::new();
    let flush = |run: &mut Vec<usize>, flags: &mut Vec<bool>| {
        if run.len() > HEADING_MAX_RUN {
            for &i in run.iter() {
                if let Some(f) = flags.get_mut(i) {
                    *f = false;
                }
            }
        }
        run.clear();
    };
    for i in 0..sheet.lines.len() {
        let adjacent = run
            .last()
            .and_then(|&p| sheet.lines.get(p))
            .zip(sheet.lines.get(i))
            .is_some_and(|(p, l)| {
                let gap = p.baseline - l.baseline;
                gap > 0.0 && gap <= 2.0 * p.size.max(l.size)
            });
        if flags.get(i).copied().unwrap_or(false) {
            if !adjacent {
                flush(&mut run, &mut flags);
            }
            run.push(i);
        } else {
            flush(&mut run, &mut flags);
        }
    }
    flush(&mut run, &mut flags);
    flags
}

/// Index of the tier `bin` belongs to: the first (largest) tier at most
/// half a point above it. `tiers` is strictly descending, so this is a binary
/// search; a linear scan per candidate line was quadratic in the number of
/// distinct sizes, which a document chooses.
pub(super) fn tier_of(tiers: &[i32], bin: i32) -> Option<usize> {
    let at = tiers.partition_point(|&t| t - bin > 1);
    (at < tiers.len()).then_some(at)
}

/// The heading sizes kept and their levels.
struct Tiers {
    /// Kept tier sizes (half points), largest first; level = index + 1.
    kept: Vec<i32>,
}

impl Tiers {
    /// Groups candidate sizes into tiers (sizes within half a point), keeps
    /// the largest tier and the [`HEADING_TIERS`] largest tiers used more than
    /// once (the one-off sizes of a cover page cannot use up the levels;
    /// labels a little larger than the body fall below the kept tiers), and
    /// clears the other candidates.
    fn choose(sheets: &[Sheet], headings: &mut [Vec<bool>]) -> Self {
        let mut bins: Vec<i32> = Vec::new();
        for (sheet, flags) in sheets.iter().zip(headings.iter()) {
            for (line, flag) in sheet.lines.iter().zip(flags) {
                if *flag {
                    bins.push(half_points(line.size));
                }
            }
        }
        bins.sort_unstable_by(|a, b| b.cmp(a));
        bins.dedup();
        let mut tiers: Vec<i32> = Vec::new();
        for bin in bins {
            if tiers.last().is_none_or(|&last| last - bin > 1) {
                tiers.push(bin);
            }
        }
        let tier_of = |size: f32| tier_of(&tiers, half_points(size));
        let mut uses = vec![0_usize; tiers.len()];
        for (sheet, flags) in sheets.iter().zip(headings.iter()) {
            for (line, flag) in sheet.lines.iter().zip(flags) {
                if *flag && let Some(n) = tier_of(line.size).and_then(|t| uses.get_mut(t)) {
                    *n += 1;
                }
            }
        }
        // Tiers are largest first, so the first repeated ones are the largest.
        let mut keep: Vec<usize> = std::iter::once(0)
            .filter(|_| !tiers.is_empty())
            .chain(
                (1..tiers.len())
                    .filter(|t| uses.get(*t).is_some_and(|&n| n >= 2))
                    .take(HEADING_TIERS),
            )
            .collect();
        keep.sort_unstable();
        let kept: Vec<i32> = keep.iter().filter_map(|&t| tiers.get(t).copied()).collect();
        for (sheet, flags) in sheets.iter().zip(headings.iter_mut()) {
            for (line, flag) in sheet.lines.iter().zip(flags.iter_mut()) {
                let bin = half_points(line.size);
                if *flag && !kept.iter().any(|&k| (k - bin).abs() <= 1) {
                    *flag = false;
                }
            }
        }
        Self { kept }
    }

    fn level(&self, size: f32) -> u8 {
        let bin = half_points(size);
        let rank = self.kept.iter().take_while(|&&k| k - bin > 1).count();
        u8::try_from(rank + 1).unwrap_or(u8::MAX)
    }
}

// --- list markers -----------------------------------------------------------------

/// The marker a list line starts with: whether it is ordered, and how many
/// characters (marker and following spaces) to drop.
fn list_marker(text: &str) -> Option<(bool, usize)> {
    let chars: Vec<char> = text.chars().take(8).collect();
    let followed = |at: usize| {
        chars.get(at) == Some(&' ') && chars.get(at + 1).is_some_and(|c| !c.is_whitespace())
    };
    let first = *chars.first()?;
    // Bullets, dashes, and the private-use bullets of Symbol and Wingdings
    // fonts (Word's default list styles).
    if matches!(
        first,
        '\u{2022}'
            | '\u{b7}'
            | '\u{25e6}'
            | '\u{25aa}'
            | '\u{2023}'
            | '\u{27a2}'
            | '\u{27a4}'
            | '\u{25ba}'
            | '\u{25b6}'
            | '\u{25cb}'
            | '\u{25cf}'
            | '\u{25a0}'
            | '\u{25a1}'
            | '\u{f0b7}'
            | '\u{f0a7}'
            | '\u{f076}'
            | '\u{f0d8}'
            | '\u{f0fc}'
            | '\u{f0a8}'
            | '-'
            | '\u{2013}'
            | '\u{2014}'
            | '*'
    ) && followed(1)
    {
        return Some((false, 2));
    }
    let close = |c: char| matches!(c, '.' | ')');
    // "(1) ", "(a) "
    if first == '(' {
        let end = chars.iter().position(|&c| c == ')')?;
        let inner: String = chars.get(1..end)?.iter().collect();
        if valid_counter(&inner) && followed(end + 1) {
            return Some((true, end + 2));
        }
        return None;
    }
    // "1. ", "12) ", "a) ", "iv. "
    let end = chars.iter().position(|&c| close(c))?;
    let inner: String = chars.get(..end)?.iter().collect();
    (valid_counter(&inner) && followed(end + 1)).then_some((true, end + 2))
}

/// 1-3 digits, one lowercase letter, or a short lowercase roman numeral.
fn valid_counter(inner: &str) -> bool {
    let digits = !inner.is_empty() && inner.len() <= 3 && inner.chars().all(|c| c.is_ascii_digit());
    let letter = inner.len() == 1 && inner.chars().all(|c| c.is_ascii_lowercase());
    let roman = matches!(
        inner,
        "i" | "ii" | "iii" | "iv" | "v" | "vi" | "vii" | "viii" | "ix" | "x"
    );
    digits || letter || roman
}

/// `runs` without their first `n` characters (and the spaces after them).
fn drop_chars(runs: &[Run], mut n: usize) -> Vec<Run> {
    let mut out = Vec::new();
    for run in runs {
        let count = run.text.chars().count();
        if n >= count {
            n -= count;
            continue;
        }
        let text: String = run.text.chars().skip(n).collect();
        n = 0;
        let text = if out.is_empty() {
            text.trim_start().to_owned()
        } else {
            text
        };
        if !text.is_empty() {
            out.push(Run {
                text,
                bold: run.bold,
                italic: run.italic,
            });
        }
    }
    out
}

// --- rules ------------------------------------------------------------------------

fn rule_text(text: &str) -> bool {
    let marks = text.chars().filter(|c| !c.is_whitespace()).count();
    marks >= 3
        && text.chars().all(|c| {
            c.is_whitespace() || matches!(c, '-' | '_' | '=' | '*' | '\u{2014}' | '\u{2500}')
        })
}

// --- assembly ---------------------------------------------------------------------

/// What is being built from consecutive lines.
enum Open {
    None,
    Paragraph(Inlines),
    Heading {
        level: u8,
        text: String,
        lines: usize,
        size: f32,
    },
    Item {
        ordered: bool,
        level: u8,
        left: f32,
        size: f32,
        inlines: Inlines,
    },
}

/// Whether a line, by its text, ends a sentence or introduces what follows.
fn ends_clause(text: &str) -> bool {
    text.trim_end().ends_with(['.', '!', '?', ':', ';'])
}

fn assemble(
    sheet: &Sheet,
    headings: &[bool],
    tiers: &Tiers,
    budget: &mut TextBudget,
    out: &mut Vec<ParsedBlock>,
) -> Result<(), DomainError> {
    let lines = &sheet.lines;
    let pitch = usual_pitch(lines);
    let (text_left, text_right) = sheet.text_span();
    // Left edges of the open list's levels.
    let mut list_lefts: Vec<f32> = Vec::new();
    let mut open = Open::None;
    for (i, line) in lines.iter().enumerate() {
        let text = sheet.texts.get(i).map_or("", String::as_str);
        let prev = i.checked_sub(1).and_then(|p| lines.get(p));
        let prev_text = i.checked_sub(1).and_then(|p| sheet.texts.get(p));
        let next = lines.get(i + 1);
        let em = line.size.max(1.0);
        let breaks_before = prev.is_none_or(|p| starts_paragraph(p, line, pitch));

        let standalone = breaks_before && next.is_none_or(|n| starts_paragraph(line, n, pitch));
        if standalone
            && rule_text(text)
            && line.right - line.left >= RULE_MIN_WIDTH * (text_right - text_left)
        {
            close(&mut open, budget, out)?;
            list_lefts.clear();
            budget.charge_block()?;
            out.push(ParsedBlock::HorizontalRule);
            continue;
        }

        if headings.get(i).copied().unwrap_or(false) {
            let level = tiers.level(line.size);
            let joins = matches!(&open, Open::Heading { level: l, lines: n, size, .. }
                if *l == level && *n < HEADING_MAX_LINES && (size - line.size).abs() <= 0.5)
                && !prev.is_some_and(|p| {
                    let gap = p.baseline - line.baseline;
                    gap <= 0.0 || gap > 1.8 * line.size
                });
            if joins {
                if let Open::Heading {
                    text: t, lines: n, ..
                } = &mut open
                {
                    budget.charge(1)?;
                    t.push(' ');
                    t.push_str(text.trim());
                    *n += 1;
                }
            } else {
                close(&mut open, budget, out)?;
                list_lefts.clear();
                open = Open::Heading {
                    level,
                    text: text.trim().to_owned(),
                    lines: 1,
                    size: line.size,
                };
            }
            continue;
        }

        // A marker at the margin of a running paragraph is the paragraph
        // wrapping before a dash or an abbreviation, not a list.
        let continues_paragraph = matches!(open, Open::Paragraph(_))
            && !breaks_before
            && prev.is_some_and(|p| (line.left - p.left).abs() < 0.5 * em)
            && !prev_text.is_some_and(|t| ends_clause(t));
        let marker = list_marker(text).filter(|_| !rule_text(text) && !continues_paragraph);
        if let Some((ordered, drop)) = marker {
            close(&mut open, budget, out)?;
            while list_lefts.last().is_some_and(|&l| line.left < l - 0.5 * em) {
                list_lefts.pop();
            }
            if list_lefts.last().is_none_or(|&l| line.left > l + 0.5 * em) {
                list_lefts.push(line.left);
            }
            let level = list_lefts.len().saturating_sub(1).min(MAX_LIST_LEVEL);
            let mut inlines = Inlines::default();
            push_runs(&mut inlines, &drop_chars(&line.runs, drop));
            open = Open::Item {
                ordered,
                level: u8::try_from(level).unwrap_or(0),
                left: line.left,
                size: line.size,
                inlines,
            };
            continue;
        }

        // A first-line indent starts a paragraph: this line is indented and
        // the next one returns to the previous margin.
        let indented = prev.is_some_and(|p| {
            line.left > p.left + 0.8 * em
                && next.is_some_and(|n| (n.left - p.left).abs() < 0.5 * em)
        });
        match &mut open {
            Open::Item {
                left,
                size,
                inlines,
                ..
            } if !breaks_before
                && (line.size - *size).abs() <= 1.0
                && line.left > *left + 0.3 * *size =>
            {
                join_line(inlines, &line.runs, budget)?;
            }
            Open::Paragraph(inlines) if !breaks_before && !indented => {
                join_line(inlines, &line.runs, budget)?;
            }
            _ => {
                close(&mut open, budget, out)?;
                list_lefts.clear();
                let mut inlines = Inlines::default();
                push_runs(&mut inlines, &line.runs);
                open = Open::Paragraph(inlines);
            }
        }
    }
    close(&mut open, budget, out)?;
    Ok(())
}

fn style(run: &Run) -> InlineStyle {
    InlineStyle {
        bold: run.bold,
        italic: run.italic,
        ..InlineStyle::default()
    }
}

fn push_runs(inlines: &mut Inlines, runs: &[Run]) {
    for run in runs {
        inlines.push_charged_text(&run.text, &style(run));
    }
}

/// Appends a line to a paragraph or item: after a line break, or directly
/// when the previous line ends in a hyphen inside a word that this line
/// continues in lowercase.
fn join_line(
    inlines: &mut Inlines,
    runs: &[Run],
    budget: &mut TextBudget,
) -> Result<(), DomainError> {
    let continues = runs
        .first()
        .and_then(|r| r.text.chars().next())
        .is_some_and(char::is_lowercase);
    if continues && inlines.drop_trailing_hyphen() {
        push_runs(inlines, runs);
        return Ok(());
    }
    // After a styled run the break starts a run of its own: charge that too.
    inlines.push("\n", &InlineStyle::default(), None, budget)?;
    push_runs(inlines, runs);
    Ok(())
}

fn close(
    open: &mut Open,
    budget: &mut TextBudget,
    out: &mut Vec<ParsedBlock>,
) -> Result<(), DomainError> {
    let block = match std::mem::replace(open, Open::None) {
        Open::None => None,
        Open::Paragraph(mut inlines) => {
            let inlines = inlines.take();
            (!inlines.is_empty()).then_some(ParsedBlock::Paragraph { inlines })
        }
        Open::Heading { level, text, .. } => Some(ParsedBlock::Heading {
            level,
            inlines: vec![Inline::plain(text)],
        }),
        Open::Item {
            ordered,
            level,
            mut inlines,
            ..
        } => {
            let inlines = inlines.take();
            (!inlines.is_empty()).then(|| ParsedBlock::ListItem {
                level,
                ordered,
                blocks: vec![ParsedBlock::Paragraph { inlines }],
            })
        }
    };
    if let Some(block) = block {
        budget.charge_block()?;
        out.push(block);
    }
    Ok(())
}
