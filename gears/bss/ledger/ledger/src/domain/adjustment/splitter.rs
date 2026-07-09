//! [`RecognizedDeferredSplitter`] — the **pure** recognized-vs-deferred split of a
//! credit/debit-note ex-tax amount across the targeted obligation's
//! recognition-schedule state (design §4.2). Inputs → split, with **no DB / txn /
//! async I/O**: the Group C `CreditNoteHandler` (infra) reads the owning
//! `recognition_schedule` rows under the §4.7 lock order and hands their state in
//! as [`ScheduleStreamState`] slices; the splitter decides how much of the note
//! reduces already-**recognized** revenue (`CONTRA_REVENUE` leg) versus the
//! **unreleased deferred** balance (`CONTRACT_LIABILITY` leg + a per-stream
//! schedule reduction), per revenue stream, and records a deterministic
//! [`SplitResult::split_basis_ref`]. The splitter never imports the repo (DE0301 —
//! no infra in domain), exactly as the recognition `ScheduleBuilder` takes its
//! context rather than reading the DB.
//!
//! **The split rule (no silent pro-rata, design §4.2 / PRD L273).** The note's
//! ex-tax amount is partitioned into a `recognized_part_minor` and a
//! `deferred_part_minor` (`recognized + deferred == amount`). The `deferred_part`
//! is the portion the caller's intent targets the unreleased deferred balance
//! (`requested_deferred_minor`); the remainder is the recognized part. Each
//! stream's deferred reduction is bounded by that schedule's **remaining
//! releasable** amount (`total_deferred_minor − recognized_minor`, never below 0)
//! — already-released segments are never recomputed (Slice 4 §4.6) — so the
//! deferred part can be placed onto a stream ONLY up to its releasable remainder.
//!
//! **Block-on-ambiguous.** The split is a hard block
//! ([`DomainError::CreditNoteSplitAmbiguous`]) — never a degrade to pro-rata —
//! when the basis is indeterminable:
//!
//! - no schedule/stream state at all for a note that requests a deferred portion
//!   (no item→schedule mapping to reduce);
//! - duplicate stream entries (the same `revenue_stream` twice — an ambiguous
//!   item→schedule mapping);
//! - the requested deferred part exceeds the **summed** releasable remainder
//!   across the supplied streams (over-reduction of in-flight schedules);
//! - a multi-stream note whose requested deferred part does not resolve to an
//!   unambiguous per-stream placement (more than one stream carries a releasable
//!   remainder AND the request neither drains them all nor targets exactly one) —
//!   spreading it would be the forbidden pro-rata.
//!
//! The Group C handler maps this to the RFC 9457 `CREDIT_NOTE_SPLIT_AMBIGUOUS`
//! (400 — the platform `CanonicalError` ladder has no 422) + the
//! `CreditNoteSplitBlocked` alarm + an exception stub
//! (`// exception stub (full exception_queue is Slice 7)`).

use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::status::SCHEDULE_STATUS_ACTIVE;

/// The already-read recognition-schedule state for ONE revenue stream of the
/// targeted obligation (Slice 4 one-schedule-per-stream, §4.5) — the pure input
/// the splitter derives over. Read by the Group C handler (infra) under the §4.7
/// lock order and handed in; the splitter never reads the DB itself.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleStreamState {
    /// The revenue stream this schedule books under (the split keeps each
    /// reduction on the SAME stream as the line it reduces, §4.5).
    pub revenue_stream: String,
    /// The owning `recognition_schedule` id — carried through to the per-stream
    /// split so the handler reduces the right schedule, and stamped into the
    /// deterministic [`SplitResult::split_basis_ref`].
    pub schedule_id: String,
    /// The whole ex-tax amount this schedule deferred to `CONTRACT_LIABILITY`.
    pub total_deferred_minor: i64,
    /// The cumulative amount already RELEASED to revenue (`<= total_deferred`).
    /// The releasable remainder is `total_deferred_minor − recognized_minor`.
    pub recognized_minor: i64,
    /// The schedule lifecycle status (`ACTIVE` / `COMPLETED` / `REPLACED` /
    /// `CANCELLED`). Only an `ACTIVE` schedule has a reducible deferred remainder
    /// (see [`Self::releasable_remaining_minor`]).
    pub status: String,
    /// The schedule's current lineage version at read time — stamped into
    /// [`SplitResult::split_basis_ref`] so the basis pins the exact schedule state
    /// the split was computed against (a later release/replace bumps it).
    pub version: i64,
}

impl ScheduleStreamState {
    /// The deferred amount still releasable on this schedule — the cap on how much
    /// of the note's deferred part may reduce this stream (design §4.2): the
    /// not-yet-released remainder `total_deferred_minor − recognized_minor`,
    /// **floored at 0** (a drained/over-recognized snapshot never yields a negative
    /// reducible). A non-`ACTIVE` schedule (COMPLETED/REPLACED/CANCELLED) has no
    /// live releasable balance the split may reduce, so it returns 0.
    #[must_use]
    pub fn releasable_remaining_minor(&self) -> i64 {
        if self.status != SCHEDULE_STATUS_ACTIVE {
            return 0;
        }
        self.total_deferred_minor
            .saturating_sub(self.recognized_minor)
            .max(0)
    }
}

/// The split decision for ONE revenue stream: how much of the note reduces that
/// stream's recognized revenue (`CONTRA_REVENUE`) versus its unreleased deferred
/// balance (`CONTRACT_LIABILITY` + the schedule reduction). `recognized + deferred`
/// is the note amount attributed to this stream. The handler builds the per-stream
/// legs + the per-stream `recognition_schedule` reduction (negative Δ on
/// `total_deferred_minor`) from these fields, keeping the SAME `revenue_stream`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_part_minor` fields mirror the `credit_note` column names verbatim (the
// storage contract); renaming to satisfy `struct_field_names` would diverge.
#[allow(clippy::struct_field_names)]
pub struct StreamSplit {
    /// The stream this slice reduces (1:1 with the input [`ScheduleStreamState`]).
    pub revenue_stream: String,
    /// The owning schedule id this slice's deferred part reduces.
    pub schedule_id: String,
    /// Ex-tax amount of the note reducing this stream's **recognized** revenue
    /// (the `CONTRA_REVENUE` debit). `>= 0`.
    pub recognized_part_minor: i64,
    /// Ex-tax amount of the note reducing this stream's **unreleased deferred**
    /// balance (the `CONTRACT_LIABILITY` debit + the schedule reduction). `>= 0`,
    /// `<= releasable_remaining_minor` of the stream.
    pub deferred_part_minor: i64,
}

/// The result of splitting a credit/debit-note ex-tax amount across the targeted
/// obligation's recognition-schedule state: the obligation-wide recognized vs
/// deferred totals, the per-stream breakdown, and the deterministic split basis to
/// record on the note row (`credit_note.split_basis_ref`). Pure data — the Group C
/// handler reads these public fields to build the legs + per-stream schedule
/// reductions; the splitter does not import the repo.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
// The `*_part_minor` fields mirror the `credit_note` columns; `split_basis_ref`
// mirrors the column name. Renaming to satisfy `struct_field_names` would diverge.
#[allow(clippy::struct_field_names)]
pub struct SplitResult {
    /// Total ex-tax amount reducing already-recognized revenue (`Σ per-stream
    /// recognized_part`; the obligation-wide `CONTRA_REVENUE` debit).
    pub recognized_part_minor: i64,
    /// Total ex-tax amount reducing the unreleased deferred balance (`Σ per-stream
    /// deferred_part`; the obligation-wide `CONTRACT_LIABILITY` debit, and the sum
    /// of the per-stream schedule reductions).
    pub deferred_part_minor: i64,
    /// The per-stream breakdown (one entry per supplied [`ScheduleStreamState`], in
    /// input order). Each carries its own `revenue_stream` + `schedule_id` so the
    /// handler reduces the right schedule on the right stream (§4.5).
    pub per_stream: Vec<StreamSplit>,
    /// The deterministic split-basis description recorded on the note
    /// (`credit_note.split_basis_ref`, design §4.1): the PO/allocation group plus
    /// the schedule id/version/releasable state each stream was split against at
    /// the effective time. Reproducible for the same inputs (audit / replay).
    pub split_basis_ref: String,
}

/// The pure recognized-vs-deferred split derivation (design §4.2). A unit type —
/// the split is a pure function of its [`SplitInput`]; [`Self::split`] is the
/// single entry point. Mirrors the recognition `ScheduleBuilder` shape (a domain
/// type whose method takes the already-resolved context).
#[domain_model]
#[derive(Clone, Copy, Debug, Default)]
pub struct RecognizedDeferredSplitter;

/// The pure inputs to one split: the targeted posted-invoice-item identity, its
/// PO/allocation group, the per-stream recognition-schedule state, the note's
/// ex-tax amount to split, and how much of that amount the caller's intent targets
/// the unreleased deferred balance. No DB handle — the schedule state is read by
/// the handler and passed in (DE0301).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitInput<'a> {
    /// The targeted posted invoice-item ref (the line being credited/debited) —
    /// stamped into [`SplitResult::split_basis_ref`] for audit.
    pub source_invoice_item_ref: &'a str,
    /// The PO / allocation group the targeted line books under (the split basis
    /// dimension, §4.2) — stamped into [`SplitResult::split_basis_ref`]. `None` for
    /// a line with no allocation group.
    pub po_allocation_group: Option<&'a str>,
    /// The already-read recognition-schedule state, one entry per revenue stream of
    /// the targeted obligation (Slice 4 one-schedule-per-stream). Empty when the
    /// line has no recognition schedule (a fully point-in-time line) — then the
    /// whole amount is recognized and a deferred request blocks.
    pub streams: &'a [ScheduleStreamState],
    /// The note's ex-tax amount to split, in minor units (`>= 0`). The
    /// `credit_note.amount_minor` is incl-tax; the caller passes the ex-tax revenue
    /// portion here (tax is reversed on its own `TAX_PAYABLE` leg, §4.2).
    pub amount_minor_ex_tax: i64,
    /// How much of `amount_minor_ex_tax` the note's intent reduces the **unreleased
    /// deferred** balance (`0 <= requested_deferred_minor <=
    /// amount_minor_ex_tax`). The remainder reduces recognized revenue. A request
    /// over the summed releasable remainder, or one that cannot be placed onto the
    /// streams unambiguously, is a block (no pro-rata).
    pub requested_deferred_minor: i64,
}

impl RecognizedDeferredSplitter {
    /// Split the note's ex-tax amount into recognized vs deferred parts across the
    /// targeted obligation's per-stream recognition-schedule state. **Pure** — no
    /// DB / txn / async. Order of operations (each a documented gate):
    ///
    /// 1. **Shape validation** — a negative amount, a negative requested deferred,
    ///    or a requested deferred over the note amount is a malformed request
    ///    ([`DomainError::AmountOutOfRange`]).
    /// 2. **Duplicate-stream gate** — the same `revenue_stream` supplied twice is
    ///    an ambiguous item→schedule mapping ⇒
    ///    [`DomainError::CreditNoteSplitAmbiguous`].
    /// 3. **No-schedule gate** — a note requesting a deferred part with NO schedule
    ///    state has no obligation to reduce ⇒ ambiguous. A zero-deferred request
    ///    with no streams is fine (wholly recognized — a fully point-in-time line).
    /// 4. **Releasable cap** — the requested deferred part must not exceed the
    ///    summed releasable remainder across the streams (over-reducing in-flight
    ///    schedules) ⇒ ambiguous.
    /// 5. **Per-stream placement** — place the deferred part onto the streams
    ///    deterministically (single-stream, drain-all, or single-releasable-target);
    ///    anything else would be pro-rata ⇒ ambiguous. The recognized part is the
    ///    per-stream remainder.
    ///
    /// # Errors
    /// [`DomainError::AmountOutOfRange`] (malformed amounts) or
    /// [`DomainError::CreditNoteSplitAmbiguous`] (an indeterminable split basis —
    /// the block-on-ambiguous safety net; the handler maps it to the RFC 9457
    /// `CREDIT_NOTE_SPLIT_AMBIGUOUS` 400 + the `CreditNoteSplitBlocked` alarm).
    pub fn split(input: &SplitInput<'_>) -> Result<SplitResult, DomainError> {
        // 1. Shape validation.
        if input.amount_minor_ex_tax < 0 {
            return Err(DomainError::AmountOutOfRange(format!(
                "credit/debit-note ex-tax split amount must be >= 0, got {}",
                input.amount_minor_ex_tax
            )));
        }
        if input.requested_deferred_minor < 0 {
            return Err(DomainError::AmountOutOfRange(format!(
                "requested deferred part must be >= 0, got {}",
                input.requested_deferred_minor
            )));
        }
        if input.requested_deferred_minor > input.amount_minor_ex_tax {
            return Err(DomainError::AmountOutOfRange(format!(
                "requested deferred part {} exceeds the ex-tax split amount {}",
                input.requested_deferred_minor, input.amount_minor_ex_tax
            )));
        }

        // 2. Duplicate-stream gate — the same revenue_stream twice is an ambiguous
        //    item→schedule mapping (which schedule does a reduction land on?).
        for (i, s) in input.streams.iter().enumerate() {
            if input.streams[..i]
                .iter()
                .any(|prev| prev.revenue_stream == s.revenue_stream)
            {
                return Err(DomainError::CreditNoteSplitAmbiguous(format!(
                    "duplicate recognition-schedule state for revenue stream `{}` (item `{}`): \
                     ambiguous item→schedule mapping",
                    s.revenue_stream, input.source_invoice_item_ref
                )));
            }
        }

        let requested_deferred = input.requested_deferred_minor;
        let recognized_total = input.amount_minor_ex_tax - requested_deferred;

        // 3. No-schedule gate — a deferred request needs a schedule to reduce.
        if input.streams.is_empty() {
            if requested_deferred > 0 {
                return Err(DomainError::CreditNoteSplitAmbiguous(format!(
                    "note for item `{}` requests a deferred part of {} but the line has no \
                     recognition-schedule state to reduce",
                    input.source_invoice_item_ref, requested_deferred
                )));
            }
            // Wholly recognized, no streams (a fully point-in-time line). The whole
            // amount is the recognized part; no per-stream deferred reduction.
            return Ok(SplitResult {
                recognized_part_minor: recognized_total,
                deferred_part_minor: 0,
                per_stream: Vec::new(),
                split_basis_ref: build_split_basis_ref(input),
            });
        }

        // 4. Releasable cap — the requested deferred must fit the summed releasable
        //    remainder across the streams (no over-reduction of in-flight schedules;
        //    Slice 4's recognized_minor <= total_deferred CHECK is the authoritative
        //    durable guard, this is the up-front domain block).
        let total_releasable: i64 = input
            .streams
            .iter()
            .map(ScheduleStreamState::releasable_remaining_minor)
            .sum();
        if requested_deferred > total_releasable {
            return Err(DomainError::CreditNoteSplitAmbiguous(format!(
                "requested deferred part {requested_deferred} exceeds the summed releasable \
                 remainder {total_releasable} across {} stream(s) for item `{}`",
                input.streams.len(),
                input.source_invoice_item_ref
            )));
        }

        // 5. Per-stream placement of the deferred part (deterministic, no pro-rata).
        let deferred_by_stream = place_deferred(input, requested_deferred, total_releasable)?;

        // The recognized part is placed onto the SAME streams (each stream's note
        // attribution is recognized + deferred). With the deferred part pinned per
        // stream, place the recognized remainder so each stream's total is
        // unambiguous too: a single-stream note carries it on that stream; a
        // multi-stream note (which only reaches here with the deferred placement
        // determinate) carries the recognized remainder on the stream that took the
        // deferred part, else the sole stream. See place_recognized.
        let recognized_by_stream = place_recognized(input, recognized_total, &deferred_by_stream)?;

        let per_stream: Vec<StreamSplit> = input
            .streams
            .iter()
            .enumerate()
            .map(|(i, s)| StreamSplit {
                revenue_stream: s.revenue_stream.clone(),
                schedule_id: s.schedule_id.clone(),
                recognized_part_minor: recognized_by_stream[i],
                deferred_part_minor: deferred_by_stream[i],
            })
            .collect();

        Ok(SplitResult {
            recognized_part_minor: recognized_total,
            deferred_part_minor: requested_deferred,
            per_stream,
            split_basis_ref: build_split_basis_ref(input),
        })
    }
}

/// Place `requested_deferred` onto the streams (index-aligned with
/// `input.streams`), deterministically and WITHOUT pro-rata. Returns the per-stream
/// deferred amounts, or [`DomainError::CreditNoteSplitAmbiguous`] when no
/// unambiguous placement exists. Resolvable cases:
///
/// - `requested_deferred == 0` ⇒ all zero (wholly recognized).
/// - exactly ONE stream has a releasable remainder ⇒ the whole deferred part lands
///   on it (the others take 0).
/// - `requested_deferred == total_releasable` ⇒ DRAIN every stream to its releasable
///   remainder (an unambiguous full reduction, no proportioning choice).
///
/// Any other multi-releasable-stream partial request is the forbidden pro-rata ⇒
/// block.
fn place_deferred(
    input: &SplitInput<'_>,
    requested_deferred: i64,
    total_releasable: i64,
) -> Result<Vec<i64>, DomainError> {
    let n = input.streams.len();
    let mut deferred = vec![0_i64; n];

    if requested_deferred == 0 {
        return Ok(deferred);
    }

    // Indices of streams that can absorb a deferred reduction.
    let releasable_idx: Vec<usize> = input
        .streams
        .iter()
        .enumerate()
        .filter(|(_, s)| s.releasable_remaining_minor() > 0)
        .map(|(i, _)| i)
        .collect();

    // Exactly one stream carries a releasable remainder ⇒ unambiguous single target
    // (the cap gate already guaranteed requested_deferred <= total_releasable, i.e.
    // <= this stream's remainder).
    if releasable_idx.len() == 1 {
        deferred[releasable_idx[0]] = requested_deferred;
        return Ok(deferred);
    }

    // Multiple releasable streams: the ONLY unambiguous multi-stream placement is a
    // full drain (request == total releasable). A partial request would have to
    // proportion across streams — the forbidden pro-rata.
    if requested_deferred == total_releasable {
        for &i in &releasable_idx {
            deferred[i] = input.streams[i].releasable_remaining_minor();
        }
        return Ok(deferred);
    }

    Err(DomainError::CreditNoteSplitAmbiguous(format!(
        "multi-stream deferred reduction of {requested_deferred} across {} releasable stream(s) \
         (summed releasable {total_releasable}) for item `{}` has no unambiguous per-stream \
         placement (a partial split would be pro-rata)",
        releasable_idx.len(),
        input.source_invoice_item_ref
    )))
}

/// Place the `recognized_total` remainder onto the streams (index-aligned),
/// deterministically. The deferred part is already pinned per stream; the
/// recognized remainder must land unambiguously too:
///
/// - single stream ⇒ the whole recognized part lands on it.
/// - `recognized_total == 0` ⇒ all zero.
/// - multi-stream ⇒ the recognized remainder lands on the SINGLE stream that took
///   the deferred part (the unambiguous "this stream is the one being reduced"
///   case). If the deferred placement spanned multiple streams (a full drain) AND a
///   non-zero recognized remainder must also be placed, there is no unambiguous
///   per-stream attribution for the recognized part ⇒ block.
fn place_recognized(
    input: &SplitInput<'_>,
    recognized_total: i64,
    deferred_by_stream: &[i64],
) -> Result<Vec<i64>, DomainError> {
    let n = input.streams.len();
    let mut recognized = vec![0_i64; n];

    if recognized_total == 0 {
        return Ok(recognized);
    }
    if n == 1 {
        recognized[0] = recognized_total;
        return Ok(recognized);
    }

    // Multi-stream: attribute the recognized remainder to the single stream that
    // took the deferred reduction (the line being reduced). More than one stream
    // with a deferred part (a full drain) plus a recognized remainder cannot be
    // attributed without proportioning ⇒ block.
    let deferred_streams: Vec<usize> = (0..n).filter(|&i| deferred_by_stream[i] > 0).collect();
    if deferred_streams.len() == 1 {
        recognized[deferred_streams[0]] = recognized_total;
        return Ok(recognized);
    }

    Err(DomainError::CreditNoteSplitAmbiguous(format!(
        "multi-stream recognized reduction of {recognized_total} for item `{}` has no unambiguous \
         per-stream placement (the deferred part spans {} stream(s); attributing the recognized \
         remainder would be pro-rata)",
        input.source_invoice_item_ref,
        deferred_streams.len()
    )))
}

/// Build the deterministic `split_basis_ref` (design §4.1): a stable, reproducible
/// description of the basis the split was computed against — the PO/allocation
/// group plus each stream's `schedule_id@version` and releasable state at the
/// effective time. The same inputs always render the same string (audit / replay);
/// streams are rendered in input order (the caller supplies them in a stable
/// order). Format is intentionally compact + greppable, not a parsed contract.
fn build_split_basis_ref(input: &SplitInput<'_>) -> String {
    let po = input.po_allocation_group.unwrap_or("-");
    if input.streams.is_empty() {
        return format!(
            "item={};po={};streams=none",
            input.source_invoice_item_ref, po
        );
    }
    let streams = input
        .streams
        .iter()
        .map(|s| {
            format!(
                "{}:{}@v{}:def={}:rec={}:rel={}:{}",
                s.revenue_stream,
                s.schedule_id,
                s.version,
                s.total_deferred_minor,
                s.recognized_minor,
                s.releasable_remaining_minor(),
                s.status,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "item={};po={};streams=[{}]",
        input.source_invoice_item_ref, po, streams
    )
}

#[cfg(test)]
#[path = "splitter_tests.rs"]
mod splitter_tests;
