//! Canonical status / lifecycle wire literals, grouped by domain.
//!
//! These string tokens are part of the persisted contract (DB column values +
//! CHECK constraints + cross-gear payloads), so the **values never change** —
//! this module only gives the formerly-scattered duplicate definitions a single
//! source of truth. Each group below is ONE concept: same text under a different
//! group is a DIFFERENT constant on purpose (e.g. period `OPEN` ≠ account
//! `OPEN`, schedule `ACTIVE` ≠ AR `ACTIVE`), and must not be cross-referenced.
//!
//! State *machines* that already have a domain enum keep it (the enum is the
//! source of truth, e.g. [`crate::domain::approval::ApprovalState`],
//! [`crate::domain::payment::chargeback::DisputePhase`]); these bare consts are
//! for the columns that are a free token with a CHECK, not an enum.

// --- AR sub-class (`ar_status` on an AR line; the chargeback reclass seam) ---

/// AR sub-class stamped on the `DISPUTED` leg of a chargeback AR-reclass; the
/// projector routes a `DISPUTED`-tagged line's signed delta onto
/// `ar_invoice_balance.disputed_minor`.
pub(crate) const AR_STATUS_DISPUTED: &str = "DISPUTED";
/// AR sub-class stamped on the `ACTIVE` (counter) leg of a chargeback
/// AR-reclass — the normal, undisputed receivable.
pub(crate) const AR_STATUS_ACTIVE: &str = "ACTIVE";

// --- Recognition schedule status (`recognition_schedule.status`) ---

/// The `ACTIVE` schedule status — the one live state per business key (the
/// partial UNIQUE predicate). Only an ACTIVE schedule carries a reducible
/// deferred remainder. The terminal states (`COMPLETED`/`REPLACED`/`CANCELLED`)
/// are stamped by recognition completion / the Group H change path.
pub(crate) const SCHEDULE_STATUS_ACTIVE: &str = "ACTIVE";

/// The `COMPLETED` schedule status — a fully-recognized schedule (every segment
/// `DONE`, `recognized_minor == total_deferred_minor`) that has reached its
/// terminal state (design §4.6). Terminal: it leaves the partial
/// `UNIQUE … WHERE status='ACTIVE'` one-live slot free, drops out of the
/// runner's ACTIVE-only due-segment feed, and out of the
/// `ledger_schedule_active_total` gauge.
pub(crate) const SCHEDULE_STATUS_COMPLETED: &str = "COMPLETED";

/// The `REPLACED` schedule status — a schedule superseded by a new version (a
/// fresh `schedule_id`) via a Group H `replace` change (design §3.6). Terminal:
/// the runner does not release a `REPLACED` schedule's remaining segments.
pub(crate) const SCHEDULE_STATUS_REPLACED: &str = "REPLACED";

/// The `CANCELLED` schedule status — a schedule cancelled outright via a Group H
/// `cancel` change (design §3.6). Terminal: the runner does not release a
/// `CANCELLED` schedule's remaining segments (the unreleased deferred remainder
/// stays as `CONTRACT_LIABILITY`; no auto-reversal in v1).
pub(crate) const SCHEDULE_STATUS_CANCELLED: &str = "CANCELLED";

// --- Recognition segment status (`recognition_segment.status`) ---

/// The `PENDING` segment status — a not-yet-released slice (the seed state, and
/// the state the `RecognitionRunner` releases from).
pub(crate) const SEGMENT_STATUS_PENDING: &str = "PENDING";

/// The `QUEUED` segment status — a due slice parked out-of-order by Group E
/// (its predecessor period was not yet `DONE`). A later run drains it; for the
/// stamp guard it is a second releasable-from state alongside `PENDING`.
pub(crate) const SEGMENT_STATUS_QUEUED: &str = "QUEUED";

/// The `DONE` segment status — a released slice (its `DR CL / CR Revenue` entry
/// posted, `recognized_at`/`run_id` stamped). Terminal; the stamp guard refuses
/// to re-flip it (the at-most-once release guard).
pub(crate) const SEGMENT_STATUS_DONE: &str = "DONE";

// --- Recognition run status (`recognition_run.status`) ---

/// The `RUNNING` recognition-run status — a run in progress (bracketed by
/// `insert_run` → `finish_run`). The single-active-run guard is the `coord`
/// lease, not a `RUNNING`-row count; this status is purely the run-row
/// lifecycle marker.
pub(crate) const RUN_STATUS_RUNNING: &str = "RUNNING";

/// The `DONE` recognition-run status — a run that completed its pass.
pub(crate) const RUN_STATUS_DONE: &str = "DONE";

/// The `FAILED` recognition-run status — a run that aborted mid-pass.
pub(crate) const RUN_STATUS_FAILED: &str = "FAILED";

// --- Fiscal period status (`fiscal_period.status`) ---

/// Fiscal-period status that admits posting (set at period-open; the
/// `FiscalPeriodGuard` requires it).
pub(crate) const PERIOD_STATUS_OPEN: &str = "OPEN";
/// Fiscal-period status for a closed period (no posting; set by period close).
pub(crate) const PERIOD_STATUS_CLOSED: &str = "CLOSED";

// --- Account lifecycle (`tenant_account.lifecycle_state`) — NOT the period ---

/// Account lifecycle state that admits posting — stamped on freshly-seeded
/// accounts and asserted by the posting / note surfaces before posting to an
/// account. A SEPARATE concept from the fiscal-period `OPEN`
/// ([`PERIOD_STATUS_OPEN`]): an account lifecycle, not a period status.
pub(crate) const LIFECYCLE_OPEN: &str = "OPEN";

// --- Payer lifecycle (`ledger_payer_state.lifecycle_state`) ---

/// Payer lifecycle state stamped on a payer-closure upsert (VHP-1852 Phase 2).
/// Absence of a row means OPEN; the only persisted lifecycle literal is the
/// closed marker. A SEPARATE concept from the fiscal-period / account `OPEN`.
pub(crate) const PAYER_LIFECYCLE_CLOSED: &str = "CLOSED";
