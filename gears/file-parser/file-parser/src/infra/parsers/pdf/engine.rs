//! The only module that talks to pdfium.
//!
//! libpdfium is embedded by `build.rs`, written to a private temporary
//! directory and bound once per process. pdfium is not thread-safe across
//! documents (`pdfium-render`'s `thread_safe` feature only serialises single
//! calls, and pdfium's last-error code is process-global), so every pdfium
//! call runs on one thread, the engine's [`Worker`], which owns the binding.
//! Documents queue for it (at most [`queue_capacity`]) and their callers wait
//! asynchronously. Characters are read one by one with their geometry;
//! pdfium's own page text (`FPDFText_GetText`) is not used, as it clips to the
//! rotated page box.
//!
//! The deadline starts when the engine takes the document and is checked
//! between pdfium calls only. A single call (loading a page with deeply
//! nested forms, say) cannot be interrupted, so one hostile document can hold
//! the engine longer than [`ParseLimits::pdf_timeout`]; only a separate
//! process could bound that. Its caller still gets an answer on time, later
//! documents are turned away as busy until the engine is free, and the engine
//! thread is never joined, so it does not hold up shutdown.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use pdfium_render::prelude::{
    PdfDocument, PdfDocumentMetadataTagType, PdfFontWeight, PdfPage, PdfPageTextChar, Pdfium,
    PdfiumError, PdfiumInternalError,
};

use crate::domain::error::{DomainError, ParseLimit};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::worker::{Submit, Worker};
use crate::infra::parsers::limits::{MAX_TITLE_CHARS, ParseLimits, limit_exceeded};

/// Runtime override: a libpdfium to load instead of the embedded one (for
/// builds without one, or hosts where the temporary directory is `noexec`).
const RUNTIME_LIB_VAR: &str = "FILE_PARSER_PDFIUM_RUNTIME_LIB";

/// Glyphs smaller than this (points) are not text a reader sees.
const MIN_GLYPH_SIZE: f32 = 1.0;

/// How many characters are read between deadline checks.
const DEADLINE_STRIDE: usize = 4096;

/// Documents allowed to wait for the engine while it reads another one,
/// unless [`configure_queue`] set another number. Each queued document holds
/// its bytes in memory; a full queue answers busy.
pub const DEFAULT_MAX_QUEUED: usize = 64;

/// The queue size set at startup (`pdf.max_queued`).
static QUEUE_CAPACITY: OnceLock<usize> = OnceLock::new();

/// Sets the queue size before the engine starts. The engine is process-wide
/// and keeps the size it started with, so a second, different value is
/// refused (and logged) rather than applied halfway.
pub fn configure_queue(capacity: usize) {
    agree_capacity(&QUEUE_CAPACITY, capacity);
}

/// Sets `cell` to `capacity` unless it already holds a value; returns the
/// value kept, warning when it differs.
fn agree_capacity(cell: &OnceLock<usize>, capacity: usize) -> usize {
    let current = *cell.get_or_init(|| capacity);
    if current != capacity {
        tracing::warn!(
            requested = capacity,
            current,
            "PDF engine queue size is already set for this process; keeping it"
        );
    }
    current
}

/// How many documents may wait for the engine. The first read fixes the
/// value: an engine started without [`configure_queue`] keeps the default.
pub(super) fn queue_capacity() -> usize {
    *QUEUE_CAPACITY.get_or_init(|| DEFAULT_MAX_QUEUED)
}

/// How long past [`ParseLimits::pdf_timeout`] a caller waits for a document
/// the engine is reading before it gives up on it.
const RUN_GRACE: Duration = Duration::from_secs(2);

/// The longest a caller waits for the engine with `pdf_timeout`: up to
/// `pdf_timeout` for its turn, then `pdf_timeout` plus [`RUN_GRACE`] for the
/// document.
pub fn longest_wait(pdf_timeout: Duration) -> Duration {
    pdf_timeout.saturating_mul(2).saturating_add(RUN_GRACE)
}

/// How long the startup check waits for the engine.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the engine waits for a caller to send its job once it has offered
/// it the turn. The caller sends in the same poll that receives the offer, so
/// this runs out for a caller whose future is no longer polled, or one whose
/// runtime is too starved to poll it in time; either loses its turn (busy).
const HANDOFF_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(5)
};

/// How long a failed bind is remembered before binding is tried again.
pub(super) const BIND_RETRY: Duration = Duration::from_secs(60);

/// Work for the engine thread, given the binding state that thread owns.
type Job = Box<dyn FnOnce(&mut Option<Binding>) + Send>;

/// A place in the engine's queue. It carries no document: the engine hands
/// back a channel when it is ready, and only then does the caller send the
/// job. A caller that gives up while queued leaves nothing large behind.
struct Ticket {
    ready: oneshot::Sender<SyncSender<Job>>,
}

/// The engine thread; started on first use, and again if it has stopped. The
/// lock is held only to clone or replace the sender.
static ENGINE: Mutex<Option<Worker<Ticket>>> = Mutex::new(None);

/// The document the engine is reading: when it started and how long its
/// caller waits for it. Set and cleared by the engine thread; callers only
/// mark an overrun as reported.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct Running {
    since: Instant,
    /// How long the caller waits once the document started.
    patience: Duration,
    /// The overrun was reported; refusals while it lasts are not.
    reported: bool,
}

impl Running {
    /// How long the document has run past its caller's patience, if it has:
    /// pdfium is stuck in a call that cannot be interrupted.
    fn overrun(self) -> Option<Duration> {
        self.since.elapsed().checked_sub(self.patience)
    }
}

fn running() -> std::sync::MutexGuard<'static, Option<Running>> {
    // Every critical section is a read, an assignment or a flag update.
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Warns, the first time a PDF is refused while a document holds the engine
/// past its caller's patience, that this is the cause rather than load.
fn warn_if_stuck() {
    let Some(overrun) = first_report(&mut running()) else {
        return;
    };
    tracing::warn!(
        ?overrun,
        "PDF refused: the engine is still held by a document past its deadline; \
         later refusals are not logged until it returns"
    );
}

/// The overrun of the running document, the first time it is asked for;
/// `None` before it overruns and after it was reported.
fn first_report(running: &mut Option<Running>) -> Option<Duration> {
    let current = running.as_mut().filter(|r| !r.reported)?;
    let overrun = current.overrun()?;
    current.reported = true;
    Some(overrun)
}

/// How often a full queue is logged at most; the line carries the number of
/// documents refused since the last one.
const FULL_LOG_EVERY: Duration = Duration::from_mins(1);

/// Counts one refusal in `log` (last report, refusals since); returns the
/// count to report when the last report is older than [`FULL_LOG_EVERY`].
fn count_refusal(log: &mut (Option<Instant>, u64), now: Instant) -> Option<u64> {
    log.1 += 1;
    if log
        .0
        .is_some_and(|at| now.saturating_duration_since(at) < FULL_LOG_EVERY)
    {
        return None;
    }
    log.0 = Some(now);
    Some(std::mem::take(&mut log.1))
}

/// When a full queue was last logged, and refusals since.
static FULL_LOG: Mutex<(Option<Instant>, u64)> = Mutex::new((None, 0));

/// Counts a refusal for a full queue and logs it, at most once a minute.
fn note_queue_full() {
    let refused = {
        // Every critical section is a few assignments.
        let mut log = FULL_LOG.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(refused) = count_refusal(&mut log, Instant::now()) else {
            return;
        };
        refused
    };
    tracing::warn!(
        refused,
        queued = queue_capacity(),
        "PDF refused: the engine queue is full (refusals since the last report)"
    );
}

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

/// A rectangle in page space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Rect {
    pub(super) left: f32,
    pub(super) bottom: f32,
    pub(super) right: f32,
    pub(super) top: f32,
}

impl Rect {
    fn of(r: pdfium_render::prelude::PdfRect) -> Self {
        Self {
            left: r.left().value,
            bottom: r.bottom().value,
            right: r.right().value,
            top: r.top().value,
        }
    }

    fn is_finite(self) -> bool {
        [self.left, self.bottom, self.right, self.top]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// `[left, bottom, right, top]`, for building test geometry.
#[cfg(test)]
impl From<[f32; 4]> for Rect {
    fn from([left, bottom, right, top]: [f32; 4]) -> Self {
        Self {
            left,
            bottom,
            right,
            top,
        }
    }
}

/// One character as drawn.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags pdfium reports per glyph"
)]
pub(super) struct Glyph {
    pub(super) ch: char,
    /// Baseline origin, page space.
    pub(super) x: f32,
    pub(super) y: f32,
    /// Loose bounding box (the glyph's advance and the font's height). Tight
    /// boxes leave out side bearings, so gaps between narrow glyphs would look
    /// like word breaks.
    pub(super) bounds: Rect,
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

/// The glyphs of one page and its visible area.
#[derive(Debug, Default)]
pub(super) struct PageGlyphs {
    pub(super) glyphs: Vec<Glyph>,
    pub(super) area: Option<Rect>,
}

/// What a document yields besides its pages.
pub(super) struct DocumentInfo {
    pub(super) title: Option<String>,
}

/// pdfium, bound, for one document on the engine thread.
pub(super) struct Engine<'a> {
    pdfium: &'a Pdfium,
    deadline: &'a Deadline<'a>,
}

/// Runs `work` on the engine thread and waits for its result.
///
/// - The queue is full: busy.
/// - The engine does not take the document within `pdf_timeout`: busy.
/// - The document takes longer than `pdf_timeout` (plus [`RUN_GRACE`]) once
///   taken: `pdf_timeout` exceeded, even if pdfium is stuck in one call.
///
/// So a caller waits for the engine at most twice `pdf_timeout` plus
/// [`RUN_GRACE`].
// cancel-safe: dropping the future abandons the document. A queued ticket is
// skipped, and a running document stops at its next deadline check.
pub(super) async fn run<T, F>(limits: &ParseLimits, work: F) -> Result<T, DomainError>
where
    T: Send + 'static,
    F: FnOnce(&Engine<'_>) -> Result<T, DomainError> + Send + 'static,
{
    run_waiting(limits, limits.pdf_timeout, work).await
}

/// [`run`], waiting up to `turn_wait` for the engine to take the document.
// cancel-safe: as `run`.
async fn run_waiting<T, F>(
    limits: &ParseLimits,
    turn_wait: Duration,
    work: F,
) -> Result<T, DomainError>
where
    T: Send + 'static,
    F: FnOnce(&Engine<'_>) -> Result<T, DomainError> + Send + 'static,
{
    let pdf_timeout = limits.pdf_timeout;
    let (ready_tx, ready_rx) = oneshot::channel();
    submit(Ticket { ready: ready_tx })?;
    let job_tx = match timeout(turn_wait, ready_rx).await {
        Ok(Ok(job_tx)) => job_tx,
        // The ticket was dropped unanswered: the engine thread stopped.
        Ok(Err(_)) => return Err(engine_error("the PDF engine dropped the document")),
        Err(_) => {
            warn_if_stuck();
            return Err(DomainError::busy(
                "the PDF engine stayed busy with other documents",
            ));
        }
    };
    let (reply_tx, reply_rx) = oneshot::channel::<Result<T, DomainError>>();
    let patience = pdf_timeout.saturating_add(RUN_GRACE);
    let span = tracing::Span::current();
    let job: Job = Box::new(move |state| {
        let _span = span.enter();
        let started = Running {
            since: Instant::now(),
            patience,
            reported: false,
        };
        *running() = Some(started);
        let abandoned = || reply_tx.is_closed();
        let deadline = Deadline::new(pdf_timeout, &abandoned);
        let result = guarded(|| {
            let pdfium = bound(state)?;
            work(&Engine {
                pdfium,
                deadline: &deadline,
            })
        });
        *running() = None;
        if let Some(overrun) = started.overrun() {
            tracing::warn!(?overrun, "PDF engine is free again after a stuck document");
        }
        // The caller may be gone by now; nobody else wants the result.
        drop(reply_tx.send(result));
    });
    // Never blocks: the channel holds one job and this is the only send. It
    // fails when the engine stopped waiting for this caller.
    if job_tx.try_send(job).is_err() {
        return Err(lost_turn());
    }
    match timeout(patience, reply_rx).await {
        Ok(Ok(result)) => result,
        // The job always answers once it runs (panics included), so it was
        // dropped unrun: the engine gave up on the hand-off.
        Ok(Err(_)) => Err(lost_turn()),
        Err(_) => {
            tracing::error!(
                ?pdf_timeout,
                "PDF engine is stuck in one pdfium call past the deadline; \
                 later PDFs are refused as busy until it returns"
            );
            Err(limit_exceeded(
                ParseLimit::PdfTimeout,
                format!("the document took longer than {pdf_timeout:?}"),
            ))
        }
    }
}

/// The engine stopped waiting for this caller's job before it arrived.
fn lost_turn() -> DomainError {
    DomainError::busy("the PDF engine moved on before the document was handed over")
}

/// Queues `ticket`, starting the engine thread if there is none.
fn submit(ticket: Ticket) -> Result<(), DomainError> {
    let submitted = {
        // Every critical section is one assignment or a channel send, so a
        // poisoned lock holds consistent data.
        let mut slot = ENGINE.lock().unwrap_or_else(PoisonError::into_inner);
        let worker =
            match slot.as_ref() {
                Some(worker) => worker,
                None => slot.insert(start_engine().map_err(|e| {
                    engine_error(&format!("cannot start the PDF engine thread: {e}"))
                })?),
            };
        let submitted = worker.submit(ticket);
        if submitted == Err(Submit::Gone) {
            // Started again for the next document.
            *slot = None;
        }
        submitted
    };
    // Logged outside the engine lock.
    match submitted {
        Ok(()) => Ok(()),
        Err(Submit::Full) => {
            warn_if_stuck();
            note_queue_full();
            Err(DomainError::busy("the PDF engine is busy; try again later"))
        }
        Err(Submit::Gone) => Err(engine_error("the PDF engine had stopped")),
    }
}

fn start_engine() -> std::io::Result<Worker<Ticket>> {
    let mut state: Option<Binding> = None;
    Worker::spawn("pdfium", queue_capacity(), move |ticket: Ticket| {
        let (job_tx, job_rx) = sync_channel(1);
        // A caller that gave up while queued has dropped its receiver; one
        // that goes away before sending the job drops `job_tx`.
        if ticket.ready.send(job_tx).is_err() {
            return;
        }
        match job_rx.recv_timeout(HANDOFF_TIMEOUT) {
            Ok(job) => job(&mut state),
            Err(RecvTimeoutError::Timeout) => tracing::warn!(
                timeout = ?HANDOFF_TIMEOUT,
                "PDF caller took its turn but sent no document; skipped"
            ),
            Err(RecvTimeoutError::Disconnected) => {}
        }
    })
}

/// Runs `f`, turning a panic into an engine error. pdfium-render asserts in a
/// few places; a panic must fail only the document it happened on.
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
// cancel-safe: dropping the future drops the check; nothing is left queued
// but a ticket.
pub async fn check_engine() -> Result<(), DomainError> {
    let (ready_tx, ready_rx) = oneshot::channel();
    submit(Ticket { ready: ready_tx })?;
    let answer = async {
        let job_tx = ready_rx.await.ok()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let job: Job = Box::new(move |state| {
            drop(reply_tx.send(guarded(|| bound(state).map(|_| ()))));
        });
        job_tx.try_send(job).ok()?;
        reply_rx.await.ok()
    };
    match timeout(CHECK_TIMEOUT, answer).await {
        Ok(Some(result)) => result,
        Ok(None) => Err(engine_error("the PDF engine check failed")),
        Err(_) => Err(engine_error(&format!(
            "the PDF engine did not answer within {CHECK_TIMEOUT:?}"
        ))),
    }
}

impl Engine<'_> {
    /// Loads `bytes` and hands the glyphs of each page, in order, to `on_page`.
    pub(super) fn read_document(
        &self,
        bytes: &[u8],
        limits: &ParseLimits,
        on_page: &mut dyn FnMut(PageGlyphs) -> Result<(), DomainError>,
    ) -> Result<DocumentInfo, DomainError> {
        read_document(self.pdfium, bytes, limits, self.deadline, on_page)
    }
}

fn read_document(
    pdfium: &Pdfium,
    bytes: &[u8],
    limits: &ParseLimits,
    deadline: &Deadline<'_>,
    on_page: &mut dyn FnMut(PageGlyphs) -> Result<(), DomainError>,
) -> Result<DocumentInfo, DomainError> {
    // The caller may have given up while the document waited for the engine.
    deadline.check()?;
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| load_error(&e))?;
    let count = usize::try_from(document.pages().len()).unwrap_or(0);
    if count > limits.max_pdf_pages {
        return Err(limit_exceeded(
            ParseLimit::MaxPdfPages,
            format!("{count} pages, limit {}", limits.max_pdf_pages),
        ));
    }
    let title = title(&document);
    // An unreadable page reads as empty; the rest still count.
    let mut unreadable = Unreadable::default();
    for index in 0..count {
        deadline.check()?;
        let glyphs = {
            let page = i32::try_from(index)
                .map_err(|e| e.to_string())
                .and_then(|i| document.pages().get(i).map_err(|e| e.to_string()));
            match page {
                Ok(page) => page_glyphs(&page, limits, deadline, &mut unreadable, index)?,
                Err(error) => {
                    unreadable.note(index, &error);
                    PageGlyphs::default()
                }
            }
        };
        on_page(glyphs)?;
    }
    unreadable.report(count);
    Ok(DocumentInfo { title })
}

/// Pages that could not be read, reported once per document.
#[derive(Default)]
struct Unreadable {
    pages: usize,
    first: Option<(usize, String)>,
}

impl Unreadable {
    fn note(&mut self, index: usize, error: &str) {
        self.pages += 1;
        if self.first.is_none() {
            self.first = Some((index, error.to_owned()));
        }
    }

    fn report(&self, total: usize) {
        if let Some((page, error)) = &self.first {
            tracing::warn!(
                unreadable = self.pages,
                total,
                first_page = page + 1,
                %error,
                "PDF pages could not be read and are treated as empty"
            );
        }
    }
}

fn page_glyphs(
    page: &PdfPage<'_>,
    limits: &ParseLimits,
    deadline: &Deadline<'_>,
    unreadable: &mut Unreadable,
    index: usize,
) -> Result<PageGlyphs, DomainError> {
    let text = match page.text() {
        Ok(text) => text,
        Err(e) => {
            unreadable.note(index, &e.to_string());
            return Ok(PageGlyphs::default());
        }
    };
    // pdfium answers -1 when it cannot count the characters.
    let Ok(count) = usize::try_from(text.len()) else {
        unreadable.note(index, "pdfium could not count the page's characters");
        return Ok(PageGlyphs::default());
    };
    if count > limits.max_pdf_chars_per_page {
        return Err(limit_exceeded(
            ParseLimit::MaxPdfCharsPerPage,
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
        .map(|b| Rect::of(b.bounds));
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
    area: Option<Rect>,
    fonts: &mut FontCache,
) -> Option<Glyph> {
    let tight = Rect::of(c.tight_bounds().ok()?);
    let bounds = c.loose_bounds().map_or(tight, Rect::of);
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
            bounds.top - bounds.bottom,
            c.angle_degrees().map_or(0.0, |a| 360.0 - a),
        ),
    };
    let finite = [x, y, size, degrees].iter().all(|v| v.is_finite())
        && bounds.is_finite()
        && tight.is_finite();
    if !finite || size < MIN_GLYPH_SIZE {
        return None;
    }
    if let Some(area) = area
        && (tight.right < area.left
            || tight.left > area.right
            || tight.top < area.bottom
            || tight.bottom > area.top)
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
            let base = name.split_once('+').map_or(name.as_str(), |(_, b)| b);
            let lower = base.to_ascii_lowercase();
            // pdfium reports weight 0 for a font without a stem width (the
            // standard fonts); only then does the name decide.
            let weight = c.font_weight().filter(|w| *w != PdfFontWeight::Custom(0));
            let bold =
                c.font_is_bold_reenforced() || weight.map_or_else(|| bold_by_name(base), is_heavy);
            let italic =
                lower.contains("italic") || lower.contains("oblique") || c.font_is_italic();
            self.style = (bold, italic);
            self.name = name;
        }
        self.style
    }
}

/// Weights that make a font bold.
const BOLD_WEIGHTS: [&str; 4] = ["bold", "black", "heavy", "demi"];

/// What may come before a weight in the same word (`SemiBold`, `Extrabold`).
const WEIGHT_MODIFIERS: [&str; 5] = ["semi", "demi", "extra", "ultra", "super"];

/// What may follow a weight in the same word (`BoldItalicMT`, `BoldIt`).
const STYLE_SUFFIXES: [&str; 7] = ["italic", "oblique", "ital", "obl", "it", "mt", "ps"];

/// Whether font name `base` names a bold weight. The name is split into words
/// at punctuation, spaces and case changes; a word counts when it is a weight,
/// optionally after a modifier and before style suffixes (`Bold`,
/// `Semibolditalic`). A word all in one case may also end that way, as in
/// `ARIALBOLDITALIC`. A weight as the first word counts only when every word
/// after it is a weight or style too, so `BlackChancery` is a family name.
fn bold_by_name(base: &str) -> bool {
    let words = name_words(base);
    words.iter().enumerate().any(|(i, word)| {
        let lower = word.to_ascii_lowercase();
        let one_case =
            !word.chars().any(char::is_lowercase) || !word.chars().any(char::is_uppercase);
        if !weight_word(&lower, one_case) {
            return false;
        }
        i > 0
            || words[1..].iter().all(|w| {
                let w = w.to_ascii_lowercase();
                styles_only(&w) || weight_word(&w, false)
            })
    })
}

/// Splits a font name into words at non-alphanumeric characters, before an
/// upper-case letter that follows a lower-case one (`ArialBlack`), and before
/// the last capital of a run followed by a lower-case letter (`PSBold`).
fn name_words(name: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = name.char_indices().collect();
    let mut words = Vec::new();
    let mut start = None;
    for (i, &(at, c)) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if let Some(from) = start.take() {
                words.push(&name[from..at]);
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|p| chars[p].1);
        let next = chars.get(i + 1).map(|n| n.1);
        let boundary = c.is_uppercase()
            && (prev.is_some_and(char::is_lowercase)
                || (prev.is_some_and(char::is_uppercase) && next.is_some_and(char::is_lowercase)));
        match start {
            Some(from) if boundary => {
                words.push(&name[from..at]);
                start = Some(at);
            }
            Some(_) => {}
            None => start = Some(at),
        }
    }
    if let Some(from) = start {
        words.push(&name[from..]);
    }
    words
}

/// Whether lower-case `word` is a weight, optionally after a modifier and
/// before style suffixes; with `anywhere`, whatever may come before it.
fn weight_word(word: &str, anywhere: bool) -> bool {
    BOLD_WEIGHTS.iter().any(|weight| {
        word.match_indices(weight).any(|(at, _)| {
            let before = &word[..at];
            styles_only(&word[at + weight.len()..])
                && (anywhere || before.is_empty() || WEIGHT_MODIFIERS.contains(&before))
        })
    })
}

/// Whether lower-case `rest` is made of style suffixes only (or is empty).
fn styles_only(rest: &str) -> bool {
    rest.is_empty()
        || STYLE_SUFFIXES
            .iter()
            .any(|s| rest.strip_prefix(s).is_some_and(styles_only))
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
    DomainError::internal(format!("PDF engine: {detail}"))
}

fn load_error(e: &PdfiumError) -> DomainError {
    match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
            pdf_error("the document is password-protected")
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
            pdf_error("the document uses unsupported encryption")
        }
        other => {
            tracing::debug!(error = ?other, "pdfium could not load the document");
            pdf_error("not a readable PDF document")
        }
    }
}

/// When the document must be done, and whether its caller is still waiting.
pub(super) struct Deadline<'a> {
    /// `None` when `limit` reaches past what `Instant` can hold: no
    /// wall-clock deadline then, only the caller's.
    at: Option<Instant>,
    limit: Duration,
    abandoned: &'a dyn Fn() -> bool,
}

impl<'a> Deadline<'a> {
    pub(super) fn new(limit: Duration, abandoned: &'a dyn Fn() -> bool) -> Self {
        Self {
            at: Instant::now().checked_add(limit),
            limit,
            abandoned,
        }
    }

    pub(super) fn check(&self) -> Result<(), DomainError> {
        if self.at.is_some_and(|at| Instant::now() > at) {
            return Err(limit_exceeded(
                ParseLimit::PdfTimeout,
                format!("the document took longer than {:?}", self.limit),
            ));
        }
        if (self.abandoned)() {
            // Nobody reads this; it only frees the engine sooner.
            return Err(engine_error("the caller stopped waiting for the document"));
        }
        Ok(())
    }
}

/// Whether a bind that failed at `failed_at` is due again at `now`. A failure
/// is remembered for [`BIND_RETRY`]: binding writes the library out again,
/// which is not worth repeating per document.
pub(super) fn retry_due(failed_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(failed_at) >= BIND_RETRY
}

/// The bound library, borrowed from the engine guard; binds on first use.
fn bound(state: &mut Option<Binding>) -> Result<&Pdfium, DomainError> {
    bound_with(state, Instant::now(), bind)
}

/// [`bound`] at `now`, binding with `bind`.
fn bound_with(
    state: &mut Option<Binding>,
    now: Instant,
    bind: impl FnOnce() -> Result<&'static Pdfium, String>,
) -> Result<&'static Pdfium, DomainError> {
    match state {
        Some(Binding::Bound(pdfium)) => return Ok(*pdfium),
        Some(Binding::Failed { at, error }) if !retry_due(*at, now) => {
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
            *state = Some(Binding::Failed { at: now, error });
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
        "this build has no bundled libpdfium; set {RUNTIME_LIB_VAR} to a libpdfium (chromium/{})",
        env!("FILE_PARSER_PDFIUM_BUILD")
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
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        let name = entry.file_name();
        if is_stale(name.to_str(), &path, current, modified, now) {
            let _removed = std::fs::remove_dir_all(&path);
        }
    }
}

/// Whether a temporary directory entry is a libpdfium directory, not
/// `current`, last changed more than a day before `now`. An entry without a
/// readable name or time, or from the future, is kept.
#[cfg(not(pdfium_unbundled))]
fn is_stale(
    name: Option<&str>,
    path: &std::path::Path,
    current: &std::path::Path,
    modified: Option<std::time::SystemTime>,
    now: std::time::SystemTime,
) -> bool {
    name.is_some_and(|n| n.starts_with("file-parser-pdfium-"))
        && path != current
        && modified
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > Duration::from_hours(24))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::cell::Cell;
    use std::path::Path;
    use std::time::SystemTime;

    use super::super::tests::{block_on, engine_alone, engine_shared, pdf};
    use super::*;

    #[test]
    fn bold_is_read_from_weight_words_of_the_font_name() {
        for (name, bold) in [
            ("Helvetica-Bold", true),
            ("BoldItalic", true),
            ("Bold-Oblique", true),
            ("Arial-BoldMT", true),
            ("TimesNewRomanPS-BoldItalicMT", true),
            ("TimesNewRomanPSBold", true),
            ("Arial,Bold", true),
            ("Arial (Bold)", true),
            ("ARIALBOLDITALIC", true),
            ("arialbold", true),
            ("OpenSans-Semibolditalic", true),
            ("MyriadPro-Semibold", true),
            ("NotoSans-SemiBold", true),
            ("Futura-DemiBold", true),
            ("AvenirNext-Demi", true),
            ("Demibold", true),
            ("ExtraBold", true),
            ("HelveticaNeue-ExtraBold", true),
            ("HelveticaNeue-Heavy", true),
            ("Roboto_Heavy", true),
            ("Montserrat-Black", true),
            ("Arial Black", true),
            ("ArialBlack", true),
            ("Helvetica-BOLD", true),
            ("Academic", false),
            ("Academy", false),
            ("AcademicSans-Regular", false),
            ("Blackletter", false),
            ("BlackadderITC-Regular", false),
            ("BlackChancery", false),
            ("Blackoak", false),
            ("Heavyweight", false),
            ("Shoeblack", false),
            ("Boldini", false),
            ("TimesNewRomanPSMT", false),
            ("Calibri-Light", false),
            ("Helvetica", false),
            ("Garamond-Italic", false),
            ("", false),
        ] {
            assert_eq!(bold_by_name(name), bold, "{name}");
        }
    }

    #[test]
    fn the_queue_size_set_first_is_kept() {
        let cell = OnceLock::new();
        assert_eq!(agree_capacity(&cell, 8), 8);
        assert_eq!(agree_capacity(&cell, 8), 8);
        assert_eq!(agree_capacity(&cell, 16), 8, "a later value is not applied");
    }

    #[test]
    fn a_stuck_document_is_reported_once() {
        let mut running = Some(Running {
            since: Instant::now(),
            patience: Duration::from_hours(1),
            reported: false,
        });
        assert_eq!(first_report(&mut running), None, "not overrun yet");
        if let Some(r) = running.as_mut() {
            r.patience = Duration::ZERO;
        }
        assert!(first_report(&mut running).is_some());
        assert_eq!(first_report(&mut running), None, "reported already");
        assert_eq!(first_report(&mut None), None);
    }

    #[test]
    fn a_full_queue_is_reported_once_a_minute_with_the_count() {
        let start = Instant::now();
        let mut log = (None, 0);
        assert_eq!(
            count_refusal(&mut log, start),
            Some(1),
            "the first is reported"
        );
        assert_eq!(
            count_refusal(&mut log, start + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            count_refusal(&mut log, start + Duration::from_secs(30)),
            None
        );
        assert_eq!(
            count_refusal(&mut log, start + FULL_LOG_EVERY),
            Some(3),
            "the refusals since the last report"
        );
    }

    #[test]
    fn callers_wait_twice_the_timeout_plus_the_grace() {
        assert_eq!(
            longest_wait(Duration::from_secs(60)),
            Duration::from_secs(122)
        );
        assert_eq!(longest_wait(Duration::MAX), Duration::MAX);
    }

    #[test]
    fn a_failed_bind_is_remembered_until_the_retry_window_ends() {
        let binds = Cell::new(0);
        let failing = || {
            binds.set(binds.get() + 1);
            Err::<&'static Pdfium, _>("no library".to_owned())
        };
        let mut state = None;
        let start = Instant::now();
        assert!(bound_with(&mut state, start, failing).is_err());
        assert_eq!(binds.get(), 1);
        assert!(matches!(state, Some(Binding::Failed { .. })));
        let inside = start + BIND_RETRY.saturating_sub(Duration::from_secs(1));
        let err = bound_with(&mut state, inside, failing).expect_err("still failed");
        assert!(err.to_string().contains("no library"), "{err}");
        assert_eq!(binds.get(), 1, "no new bind inside the window");
        assert!(bound_with(&mut state, start + BIND_RETRY, failing).is_err());
        assert_eq!(binds.get(), 2, "bound again once the window ended");
    }

    #[test]
    fn a_deadline_too_far_to_represent_only_follows_the_caller() {
        let waiting = || false;
        assert!(Deadline::new(Duration::MAX, &waiting).check().is_ok());
        let gone = || true;
        assert!(Deadline::new(Duration::MAX, &gone).check().is_err());
    }

    #[cfg(not(pdfium_unbundled))]
    #[test]
    fn only_old_libpdfium_directories_of_other_processes_are_stale() {
        let current = Path::new("/tmp/file-parser-pdfium-current");
        let other = Path::new("/tmp/file-parser-pdfium-other");
        let now = SystemTime::now();
        let old = now - Duration::from_hours(25);
        let name = Some("file-parser-pdfium-other");
        assert!(is_stale(name, other, current, Some(old), now));
        assert!(!is_stale(
            Some("file-parser-pdfium-current"),
            current,
            current,
            Some(old),
            now
        ));
        assert!(!is_stale(
            Some("something-else"),
            Path::new("/tmp/something-else"),
            current,
            Some(old),
            now
        ));
        let recent = now - Duration::from_hours(23);
        assert!(!is_stale(name, other, current, Some(recent), now));
        let future = now + Duration::from_hours(1);
        assert!(!is_stale(name, other, current, Some(future), now));
        assert!(!is_stale(name, other, current, None, now));
        assert!(!is_stale(None, other, current, Some(old), now));
    }

    /// Runs `hold` on the engine from a thread of its own; returns once it
    /// has started. The job waits for its turn longer than `limits` says, so
    /// a short `pdf_timeout` sets only how long its caller waits once it runs.
    fn hold_engine(
        limits: ParseLimits,
        hold: impl FnOnce() + Send + 'static,
    ) -> std::thread::JoinHandle<Result<(), DomainError>> {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let turn_wait = Duration::from_secs(30);
            block_on(run_waiting(&limits, turn_wait, move |_engine| {
                started_tx.send(()).expect("the test waits");
                hold();
                Ok(())
            }))
        });
        if started_rx.recv().is_err() {
            let result = holder.join().expect("holder ends");
            panic!("the hold job did not run: {result:?}");
        }
        holder
    }

    fn full_log_now() -> (Option<Instant>, u64) {
        *FULL_LOG.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running_now() -> Option<Running> {
        *running()
    }

    #[test]
    fn a_full_queue_answers_busy_and_is_logged() {
        let _engine = engine_alone();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = hold_engine(ParseLimits::default(), move || {
            release_rx.recv().unwrap_or_default();
        });
        let log_before = full_log_now();
        let mut queued = Vec::new();
        let refused = loop {
            let (ready, ready_rx) = oneshot::channel();
            match submit(Ticket { ready }) {
                Ok(()) => queued.push(ready_rx),
                Err(e) => break e,
            }
            assert!(queued.len() <= queue_capacity(), "the queue is bounded");
        };
        assert_eq!(queued.len(), queue_capacity());
        assert!(matches!(refused, DomainError::Busy { .. }), "{refused}");
        // Either logged now or counted toward the next report.
        assert_ne!(full_log_now(), log_before, "the refusal was counted");
        assert!(full_log_now().0.is_some(), "a full queue was logged");
        // Callers that gave up leave tickets the engine skips.
        drop(queued);
        drop(release_tx);
        holder
            .join()
            .expect("holder ends")
            .expect("held document ends");
        block_on(run(&ParseLimits::default(), |_engine| Ok(()))).expect("the engine is free");
    }

    #[test]
    fn a_refusal_during_an_overrun_is_reported_and_the_engine_clears_it() {
        let _engine = engine_alone();
        let stuck = ParseLimits {
            pdf_timeout: Duration::from_millis(50),
            ..ParseLimits::default()
        };
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // Stands in for a pdfium call that cannot be interrupted.
        let holder = hold_engine(stuck.clone(), move || {
            release_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_default();
        });
        assert!(running_now().is_some(), "set while a document runs");
        let err = holder
            .join()
            .expect("holder ends")
            .expect_err("its caller gives up");
        assert!(
            matches!(
                err,
                DomainError::LimitExceeded {
                    limit: ParseLimit::PdfTimeout,
                    ..
                }
            ),
            "{err}"
        );
        let waited = Instant::now();
        while running_now().is_none_or(|r| r.overrun().is_none()) {
            assert!(waited.elapsed() < Duration::from_secs(10), "no overrun");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!running_now().is_some_and(|r| r.reported));
        let refused = block_on(run(&stuck, |_engine| Ok(()))).expect_err("still held");
        assert!(matches!(refused, DomainError::Busy { .. }), "{refused}");
        assert!(
            running_now().is_some_and(|r| r.reported),
            "the refusal reported the overrun"
        );
        drop(release_tx);
        let waited = Instant::now();
        while running_now().is_some() {
            assert!(
                waited.elapsed() < Duration::from_secs(10),
                "the engine clears the running document when it returns"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reads `bytes` on the engine; its caller gives up once `gone` answers
    /// true when asked with the number of deadline checks so far and the
    /// pages handed on. Returns the result and the pages handed on.
    fn read_until(
        bytes: Vec<u8>,
        gone: impl Fn(usize, usize) -> bool + Send + 'static,
    ) -> (Result<(), DomainError>, usize) {
        block_on(run(&ParseLimits::default(), move |engine| {
            let pages = Cell::new(0);
            let checks = Cell::new(0);
            let abandoned = || {
                checks.set(checks.get() + 1);
                gone(checks.get(), pages.get())
            };
            let deadline = Deadline::new(Duration::from_hours(1), &abandoned);
            let result = read_document(
                engine.pdfium,
                &bytes,
                &ParseLimits::default(),
                &deadline,
                &mut |_page| {
                    pages.set(pages.get() + 1);
                    Ok(())
                },
            );
            Ok((result.map(|_| ()), pages.get()))
        }))
        .expect("the engine runs the read")
    }

    fn assert_given_up(result: &Result<(), DomainError>) {
        let err = result.as_ref().expect_err("the read stops");
        assert!(err.to_string().contains("caller stopped waiting"), "{err}");
    }

    #[test]
    fn a_caller_giving_up_stops_the_read_between_pages() {
        let _engine = engine_shared();
        // The second page has no characters, so only the check between
        // pages can stop it.
        let bytes = pdf(&[("BT /F1 12 Tf 72 700 Td (x) Tj ET", ""), ("", "")], None);
        let (result, pages) = read_until(bytes, |_, pages| pages == 1);
        assert_given_up(&result);
        assert_eq!(pages, 1, "the first page was read, the second was not");
    }

    #[test]
    fn a_caller_giving_up_stops_the_read_inside_a_long_page() {
        let _engine = engine_shared();
        let text = "x".repeat(DEADLINE_STRIDE + 100);
        let content = format!("BT /F1 1 Tf 0 0 Td ({text}) Tj ET");
        // Checks: before loading, before the page, at characters 0 and 4096.
        let (result, pages) = read_until(pdf(&[(content.as_str(), "")], None), |checks, _| {
            checks == 4
        });
        assert_given_up(&result);
        assert_eq!(pages, 0, "stopped inside the page");
    }

    #[test]
    fn a_page_pdfium_cannot_load_reads_as_empty() {
        let _engine = engine_shared();
        let mut bytes = pdf(&[("BT /F1 12 Tf 72 700 Td (x) Tj ET", ""), ("", "")], None);
        // Point the second page at an object the file does not have: same
        // length, so the cross-reference table still holds.
        let kids = b"/Kids [5 0 R 7 0 R ]";
        let at = bytes
            .windows(kids.len())
            .position(|w| w == kids)
            .expect("page tree");
        bytes[at..at + kids.len()].copy_from_slice(b"/Kids [5 0 R 9 0 R ]");
        let (result, pages) = read_until(bytes, |_, _| false);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(pages, 2, "the unreadable page still counts");
    }

    #[test]
    fn a_caller_that_never_sends_its_job_does_not_hold_the_engine() {
        let _engine = engine_shared();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let (ready_tx, ready_rx) = oneshot::channel();
        submit(Ticket { ready: ready_tx }).expect("queued");
        // Take the turn, as a caller whose future then stops being polled.
        let job_tx = runtime
            .block_on(ready_rx)
            .expect("the engine offers the turn");
        let result = runtime.block_on(run(&ParseLimits::default(), |_engine| Ok(())));
        assert!(result.is_ok(), "the next document still runs: {result:?}");
        assert!(
            job_tx.try_send(Box::new(|_| {})).is_err(),
            "the engine stopped waiting for the stalled caller"
        );
    }
}
