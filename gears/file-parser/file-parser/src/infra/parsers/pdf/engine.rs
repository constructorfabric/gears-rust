//! The only module that talks to pdfium.
//!
//! libpdfium is embedded by `build.rs`, written to a private temporary
//! directory and bound once per process. pdfium is not thread-safe across
//! documents (`pdfium-render`'s `thread_safe` feature only serialises single
//! calls, and pdfium's last-error code is process-global), so one lock is held
//! from binding and loading until the document and all its pages are dropped.
//! Characters are read one by one with their geometry; pdfium's own page text
//! (`FPDFText_GetText`) is not used, as it clips to the rotated page box.
//!
//! The deadline is checked between pdfium calls only. A single call (loading
//! a page with deeply nested forms, say) cannot be interrupted, so one hostile
//! document can hold the engine longer than [`ParseLimits::pdf_timeout`];
//! only a separate process could bound that. To keep such a document from
//! also tying up the blocking thread pool, at most [`MAX_WAITERS`] documents
//! wait for the engine; more are turned away as busy.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use pdfium_render::prelude::{
    PdfDocument, PdfDocumentMetadataTagType, PdfFontWeight, PdfPage, PdfPageTextChar, Pdfium,
    PdfiumError, PdfiumInternalError,
};

use crate::domain::error::DomainError;
use crate::infra::parsers::limits::{ParseLimits, limit_exceeded};

/// Runtime override: a libpdfium to load instead of the embedded one (for
/// builds without one, or hosts where the temporary directory is `noexec`).
const RUNTIME_LIB_VAR: &str = "FILE_PARSER_PDFIUM_RUNTIME_LIB";

/// Longest document title kept.
const MAX_TITLE_CHARS: usize = 512;

/// Glyphs smaller than this (points) are not text a reader sees.
const MIN_GLYPH_SIZE: f32 = 1.0;

/// How many characters are read between deadline checks.
const DEADLINE_STRIDE: usize = 4096;

/// Documents allowed to wait for the engine while another one holds it.
/// Each waits on a blocking-pool thread (512 by default in tokio); the cap
/// keeps a run of slow documents from taking all of them.
const MAX_WAITERS: usize = 64;

/// How long a failed bind is remembered before binding is tried again.
const BIND_RETRY: Duration = Duration::from_secs(60);

/// One lock for every pdfium call of this crate.
pub(super) static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

/// Documents waiting for [`PDFIUM_LOCK`].
static WAITERS: AtomicUsize = AtomicUsize::new(0);

/// The bound library, or the last failure. Only touched with
/// [`PDFIUM_LOCK`] held.
static BINDING: Mutex<Option<Binding>> = Mutex::new(None);

enum Binding {
    Bound(&'static Pdfium),
    Failed { at: Instant, error: String },
}

/// Direction of a glyph's baseline, counter-clockwise from left-to-right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Direction {
    Deg0,
    /// Text running bottom to top.
    Deg90,
    /// Upside down.
    Deg180,
    /// Text running top to bottom.
    Deg270,
    /// Any other angle.
    Other,
}

impl Direction {
    pub(super) fn from_degrees(degrees: f32) -> Self {
        let d = degrees.rem_euclid(360.0);
        let near = |target: f32| (d - target).abs() <= 10.0;
        if near(0.0) || near(360.0) {
            Self::Deg0
        } else if near(90.0) {
            Self::Deg90
        } else if near(180.0) {
            Self::Deg180
        } else if near(270.0) {
            Self::Deg270
        } else {
            Self::Other
        }
    }
}

/// One character as drawn.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent flags pdfium reports per glyph
pub(super) struct Glyph {
    pub(super) ch: char,
    /// Baseline origin, page space.
    pub(super) x: f32,
    pub(super) y: f32,
    /// Loose bounding box (the glyph's advance and the font's height), page
    /// space: left, bottom, right, top. Tight boxes leave out side bearings,
    /// so gaps between narrow glyphs would look like word breaks.
    pub(super) bounds: [f32; 4],
    /// Effective font size in points.
    pub(super) size: f32,
    pub(super) direction: Direction,
    /// Exact baseline angle, degrees counter-clockwise.
    pub(super) angle: f32,
    /// Position in the page's content. Characters that were dropped still
    /// take a position, so the glyphs around them are not content neighbours.
    pub(super) seq: u32,
    /// The content had a space (or line break) right before this glyph.
    pub(super) space_before: bool,
    /// pdfium marks this as a hyphen at a line end.
    pub(super) hyphen: bool,
    pub(super) bold: bool,
    pub(super) italic: bool,
}

/// The glyphs of one page and its visible area (left, bottom, right, top).
#[derive(Debug, Default)]
pub(super) struct PageGlyphs {
    pub(super) glyphs: Vec<Glyph>,
    pub(super) area: Option<[f32; 4]>,
}

/// What a document yields besides its pages.
pub(super) struct DocumentInfo {
    pub(super) title: Option<String>,
}

/// Loads `bytes` and hands the glyphs of each page, in order, to `on_page`.
pub(super) fn read_document(
    bytes: &[u8],
    limits: &ParseLimits,
    on_page: &mut dyn FnMut(PageGlyphs) -> Result<(), DomainError>,
) -> Result<DocumentInfo, DomainError> {
    let deadline = Deadline::new(limits);
    let _guard = {
        let _waiting = Waiting::enter()?;
        PDFIUM_LOCK
            .try_lock_for(limits.pdf_timeout)
            .ok_or_else(|| engine_error("the PDF engine stayed busy with other documents"))?
    };
    let pdfium = bound()?;
    guarded(|| read_locked(pdfium, bytes, limits, &deadline, on_page))
}

/// Runs `f`, turning a panic into an engine error. pdfium-render asserts in a
/// few places; a panic must not take the worker down, and the lock does not
/// poison, so later documents still run.
pub(super) fn guarded<T>(f: impl FnOnce() -> Result<T, DomainError>) -> Result<T, DomainError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        let what = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(panic = %what, "PDF engine panicked");
        Err(engine_error("the PDF engine failed on this document"))
    })
}

/// Checks that the engine can be bound, binding it if needed.
pub fn check_engine() -> Result<(), DomainError> {
    let _guard = PDFIUM_LOCK.lock();
    bound().map(|_| ())
}

/// A slot among the documents waiting for the engine.
struct Waiting;

impl Waiting {
    fn enter() -> Result<Self, DomainError> {
        if WAITERS.fetch_add(1, Ordering::AcqRel) >= MAX_WAITERS {
            WAITERS.fetch_sub(1, Ordering::AcqRel);
            return Err(engine_error("the PDF engine is busy; try again later"));
        }
        Ok(Self)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn read_locked(
    pdfium: &Pdfium,
    bytes: &[u8],
    limits: &ParseLimits,
    deadline: &Deadline,
    on_page: &mut dyn FnMut(PageGlyphs) -> Result<(), DomainError>,
) -> Result<DocumentInfo, DomainError> {
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| load_error(&e))?;
    let count = usize::try_from(document.pages().len()).unwrap_or(0);
    if count > limits.max_pdf_pages {
        return Err(limit_exceeded(
            "max_pdf_pages",
            format!("{count} pages, limit {}", limits.max_pdf_pages),
        ));
    }
    let title = title(&document);
    for index in 0..count {
        deadline.check()?;
        let glyphs = {
            let page = i32::try_from(index)
                .ok()
                .and_then(|i| document.pages().get(i).ok());
            match page {
                Some(page) => page_glyphs(&page, limits, deadline)?,
                // An unreadable page reads as empty; the rest still count.
                None => PageGlyphs::default(),
            }
        };
        on_page(glyphs)?;
    }
    Ok(DocumentInfo { title })
}

fn page_glyphs(
    page: &PdfPage<'_>,
    limits: &ParseLimits,
    deadline: &Deadline,
) -> Result<PageGlyphs, DomainError> {
    let Ok(text) = page.text() else {
        return Ok(PageGlyphs::default());
    };
    let count = usize::try_from(text.len()).unwrap_or(0);
    if count > limits.max_pdf_chars_per_page {
        return Err(limit_exceeded(
            "max_pdf_chars_per_page",
            format!(
                "a page has {count} characters, limit {}",
                limits.max_pdf_chars_per_page
            ),
        ));
    }
    let area = page
        .boundaries()
        .crop()
        .or_else(|_| page.boundaries().media())
        .ok()
        .map(|b| {
            let r = b.bounds;
            [
                r.left().value,
                r.bottom().value,
                r.right().value,
                r.top().value,
            ]
        });
    let chars = text.chars();
    let mut glyphs = Vec::new();
    let mut fonts = FontCache::default();
    let mut space_before = false;
    let mut seq: u32 = 0;
    for index in 0..count {
        if index % DEADLINE_STRIDE == 0 {
            deadline.check()?;
        }
        let Ok(c) = chars.get(index) else {
            seq = seq.saturating_add(1);
            continue;
        };
        let ch = c.unicode_char();
        if ch.is_some_and(char::is_whitespace) {
            space_before = true;
            continue;
        }
        seq = seq.saturating_add(1);
        let hyphen = c.is_hyphen().unwrap_or(false);
        // pdfium writes a soft line-end hyphen as U+0002; an unmapped glyph
        // comes back as U+FFFD or no character at all.
        let ch = match ch {
            Some('\u{2}') if hyphen => '-',
            Some(ch) if !ch.is_control() && ch != char::REPLACEMENT_CHARACTER => ch,
            _ => continue,
        };
        if c.is_generated().unwrap_or(false) {
            continue;
        }
        if let Some(glyph) = glyph(&c, ch, hyphen, area, &mut fonts) {
            glyphs.push(Glyph {
                seq,
                space_before,
                ..glyph
            });
            space_before = false;
        }
    }
    Ok(PageGlyphs { glyphs, area })
}

/// A glyph for `c`, or `None` when it is not visible text: degenerate
/// geometry, smaller than [`MIN_GLYPH_SIZE`], or outside the page.
fn glyph(
    c: &PdfPageTextChar<'_>,
    ch: char,
    hyphen: bool,
    area: Option<[f32; 4]>,
    fonts: &mut FontCache,
) -> Option<Glyph> {
    let rect = |r: pdfium_render::prelude::PdfRect| {
        [
            r.left().value,
            r.bottom().value,
            r.right().value,
            r.top().value,
        ]
    };
    let tight = rect(c.tight_bounds().ok()?);
    let bounds = c.loose_bounds().map_or(tight, rect);
    let (x, y) = c.origin().ok()?;
    let (x, y) = (x.value, y.value);
    // The text matrix gives both direction and scale; `scaled_font_size`
    // uses only its vertical component and so reads 0 for rotated text.
    let unscaled = c.unscaled_font_size().value;
    let (size, degrees) = match c.matrix() {
        Ok(m) => (
            unscaled * m.c().hypot(m.d()),
            m.b().atan2(m.a()).to_degrees(),
        ),
        Err(_) => (
            bounds[3] - bounds[1],
            c.angle_degrees().map_or(0.0, |a| 360.0 - a),
        ),
    };
    let finite = [x, y, size, degrees]
        .iter()
        .chain(&bounds)
        .chain(&tight)
        .all(|v| v.is_finite());
    if !finite || size < MIN_GLYPH_SIZE {
        return None;
    }
    if let Some([left, bottom, right, top]) = area
        && (tight[2] < left || tight[0] > right || tight[3] < bottom || tight[1] > top)
    {
        return None;
    }
    let (bold, italic) = fonts.style(c);
    Some(Glyph {
        ch,
        x,
        y,
        bounds,
        size,
        direction: Direction::from_degrees(degrees),
        angle: degrees.rem_euclid(360.0),
        seq: 0,
        space_before: false,
        hyphen,
        bold,
        italic,
    })
}

/// Bold and italic by font. The name is read for every glyph (pdfium has no
/// cheaper font identity per character); the flags only when it changes.
#[derive(Default)]
struct FontCache {
    name: String,
    style: (bool, bool),
}

impl FontCache {
    fn style(&mut self, c: &PdfPageTextChar<'_>) -> (bool, bool) {
        let name = c.font_name();
        if name != self.name {
            // Standard fonts report no weight, so the name decides first.
            let base = name.split_once('+').map_or(name.as_str(), |(_, b)| b);
            let lower = base.to_ascii_lowercase();
            let bold = ["bold", "black", "heavy", "semibold", "demi"]
                .iter()
                .any(|w| lower.contains(w))
                || c.font_is_bold_reenforced()
                || c.font_weight().is_some_and(is_heavy);
            let italic =
                lower.contains("italic") || lower.contains("oblique") || c.font_is_italic();
            self.style = (bold, italic);
            self.name = name;
        }
        self.style
    }
}

fn is_heavy(weight: PdfFontWeight) -> bool {
    match weight {
        PdfFontWeight::Weight600
        | PdfFontWeight::Weight700Bold
        | PdfFontWeight::Weight800
        | PdfFontWeight::Weight900 => true,
        PdfFontWeight::Custom(w) => w >= 600,
        _ => false,
    }
}

/// The Info dictionary title, unless it is blank or a producer's placeholder
/// ("Untitled", "Microsoft Word - report.docx", a file name).
fn title(document: &PdfDocument<'_>) -> Option<String> {
    let tag = document.metadata().get(PdfDocumentMetadataTagType::Title)?;
    clean_title(tag.value())
}

pub(super) fn clean_title(raw: &str) -> Option<String> {
    let title: String = raw
        .split(|c: char| c.is_whitespace() || c == '\0')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    let lower = title.to_ascii_lowercase();
    let junk = title.is_empty()
        || lower == "untitled"
        || lower.starts_with("microsoft word - ")
        || lower.starts_with("microsoft powerpoint - ")
        || [
            ".doc", ".docx", ".pdf", ".rtf", ".odt", ".indd", ".txt", ".xls", ".xlsx",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext));
    (!junk).then_some(title)
}

fn pdf_error(detail: &str) -> DomainError {
    DomainError::parse_error(format!("Failed to parse PDF: {detail}"))
}

/// A failure of the engine rather than of the document: reported as an
/// internal error.
fn engine_error(detail: &str) -> DomainError {
    DomainError::io_error(format!("PDF engine: {detail}"))
}

fn load_error(e: &PdfiumError) -> DomainError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            pdf_error("the document is password-protected")
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            pdf_error("the document uses unsupported encryption")
        }
        _ => pdf_error("not a readable PDF document"),
    }
}

pub(super) struct Deadline {
    at: Instant,
    limit: std::time::Duration,
}

impl Deadline {
    pub(super) fn new(limits: &ParseLimits) -> Self {
        Self {
            at: Instant::now() + limits.pdf_timeout,
            limit: limits.pdf_timeout,
        }
    }

    pub(super) fn check(&self) -> Result<(), DomainError> {
        if Instant::now() > self.at {
            return Err(limit_exceeded(
                "pdf_timeout",
                format!("the document took longer than {:?}", self.limit),
            ));
        }
        Ok(())
    }
}

/// The bound library; binds on first use. Called with [`PDFIUM_LOCK`] held.
/// A failure is remembered for [`BIND_RETRY`]: binding writes the library
/// out again, which is not worth repeating per document.
fn bound() -> Result<&'static Pdfium, DomainError> {
    let mut state = BINDING.lock();
    match &*state {
        Some(Binding::Bound(pdfium)) => return Ok(pdfium),
        Some(Binding::Failed { at, error }) if at.elapsed() < BIND_RETRY => {
            return Err(engine_error(&format!("not available: {error}")));
        }
        _ => {}
    }
    match bind() {
        Ok(pdfium) => {
            *state = Some(Binding::Bound(pdfium));
            Ok(pdfium)
        }
        Err(error) => {
            tracing::error!(%error, "cannot load libpdfium; PDF parsing is unavailable");
            let err = engine_error(&format!("not available: {error}"));
            *state = Some(Binding::Failed {
                at: Instant::now(),
                error,
            });
            Err(err)
        }
    }
}

fn bind() -> Result<&'static Pdfium, String> {
    if let Some(path) = std::env::var_os(RUNTIME_LIB_VAR) {
        let bindings = Pdfium::bind_to_library(std::path::PathBuf::from(path))
            .map_err(|e| format!("{RUNTIME_LIB_VAR}: {e}"))?;
        return Ok(Box::leak(Box::new(Pdfium::new(bindings))));
    }
    bind_embedded()
}

#[cfg(not(pdfium_unbundled))]
fn bind_embedded() -> Result<&'static Pdfium, String> {
    static LIBRARY: &[u8] = include_bytes!(env!("FILE_PARSER_PDFIUM_EMBED"));
    // A private directory (0700 on Unix): a shared, predictable path would let
    // another local user plant a library for this process to load.
    let dir = tempfile::Builder::new()
        .prefix("file-parser-pdfium-")
        .tempdir()
        .map_err(|e| format!("cannot create a directory for libpdfium: {e}"))?;
    let path = Pdfium::pdfium_platform_library_name_at_path(dir.path());
    std::fs::write(&path, LIBRARY)
        .map_err(|e| format!("cannot write libpdfium to {}: {e}", path.display()))?;
    let bindings = Pdfium::bind_to_library(&path).map_err(|e| e.to_string())?;
    let pdfium: &'static Pdfium = Box::leak(Box::new(Pdfium::new(bindings)));
    // A loaded library stays mapped after its file is removed on Unix;
    // Windows keeps the file locked, so the directory stays for the process
    // and directories older processes left behind are removed instead.
    if cfg!(unix) {
        drop(dir);
    } else {
        remove_stale_dirs(dir.path());
        let _kept = dir.keep();
    }
    Ok(pdfium)
}

#[cfg(pdfium_unbundled)]
fn bind_embedded() -> Result<&'static Pdfium, String> {
    Err(format!(
        "this build has no bundled libpdfium; set {RUNTIME_LIB_VAR} to a libpdfium (chromium/7961)"
    ))
}

/// Removes `file-parser-pdfium-*` directories older than a day from the
/// temporary directory, best effort; `current` is kept.
#[cfg(not(pdfium_unbundled))]
fn remove_stale_dirs(current: &std::path::Path) {
    let Some(parent) = current.parent() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let day = Duration::from_hours(24);
    for entry in entries.flatten() {
        let path = entry.path();
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("file-parser-pdfium-"))
            && path != current
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > day));
        if stale {
            let _removed = std::fs::remove_dir_all(&path);
        }
    }
}
