//! Adjustments domain (Slice 3, design §4.1 / §4.2) — the shared pure domain
//! for credit/debit notes & refunds. Backend-agnostic: no DB / txn / async I/O
//! (the in-txn handlers — `CreditNoteHandler` / `DebitNoteHandler` / `RefundHandler`,
//! Groups C–G — own the durable posting + counter/schedule deltas; this layer
//! only derives the pure decisions they apply).
//!
//! v1 (Phase 1 / Groups B–D) ships:
//!
//! - [`debit_note`] — the pure debit-note request shape
//!   ([`debit_note::DebitNoteRequest`]) + the deterministic **direct-split** leg
//!   plan ([`debit_note::build_debit_note_legs`], design §4.3): a mirror of the
//!   Slice-1 invoice-post split (DR `AR` incl-tax / CR `REVENUE` recognized-now /
//!   CR `CONTRACT_LIABILITY` deferred-per-PO / CR `TAX_PAYABLE`). Unlike the credit
//!   note it does NOT use the [`splitter`] (a debit note is a fresh charge, not a
//!   reduction of an existing obligation); when it defers, the Group D infra
//!   [`DebitNoteHandler`](crate::infra::adjustment::debit_note_service) runs the
//!   SAME recognition [`ScheduleBuilder`](crate::domain::recognition::builder)
//!   path Slice 1 uses to build the releasing schedule in the same atomic txn (D4),
//!   and raises the invoice's headroom (`invoice_exposure.debit_note_total_minor
//!   += amount`).
//! - [`credit_note`] — the pure credit-note request shape
//!   ([`credit_note::CreditNoteRequest`]) + the deterministic compensating-leg
//!   plan ([`credit_note::build_credit_note_legs`], design §4.2): the
//!   `CONTRA_REVENUE`/`GOODWILL` + per-stream `CONTRACT_LIABILITY` + `TAX_PAYABLE`
//!   debits against the open-AR-capped `AR` credit + the `REUSABLE_CREDIT`
//!   remainder (K-2). Backed by the Group C infra
//!   [`CreditNoteHandler`](crate::infra::adjustment::credit_note_service) which
//!   reads the schedule state + open AR, drives the [`splitter`], builds the legs,
//!   and posts them atomically with the schedule/headroom/wallet writes.
//! - [`splitter`] — [`splitter::RecognizedDeferredSplitter`], the pure
//!   recognized-vs-deferred split of a credit/debit-note ex-tax amount across the
//!   targeted obligation's recognition-schedule state (one schedule per revenue
//!   stream, Slice 4 §4.5). The deferred part of each stream is bounded by that
//!   schedule's remaining releasable amount (`total_deferred_minor −
//!   recognized_minor`); the recognized part is the remainder. An indeterminable
//!   basis (no item→schedule/stream mapping, an unresolved per-stream split, or a
//!   deferred request over the summed releasable) is a **block**
//!   ([`crate::domain::error::DomainError::CreditNoteSplitAmbiguous`]) — never a
//!   silent pro-rata (design §4.2, PRD L273). The Group C `CreditNoteHandler`
//!   reads the [`splitter::SplitResult`] public fields to build the `CONTRA_REVENUE`
//!   / `CONTRACT_LIABILITY` leg amounts + the per-stream schedule reductions, and
//!   records the deterministic [`splitter::SplitResult::split_basis_ref`] on the
//!   `credit_note` row.
//!
//! The splitter is **sync + pure** (mirroring [`crate::domain::recognition`]): it
//! is a function of the already-read schedule state handed in as
//! [`splitter::ScheduleStreamState`] inputs, so there is no async I/O to model and
//! a sync API keeps it callable from the in-txn Group C handler without an
//! executor. The schedule state is READ by the handler (infra) and passed in — the
//! splitter never imports the repo (DE0301 — no infra in domain), exactly as the
//! recognition `ScheduleBuilder` takes its context rather than reading the DB.
//!
//! Phase 2 / Group B adds:
//!
//! - [`refund`] — the pure refund request shape ([`refund::RefundRequest`]) + the
//!   deterministic two-leg plan ([`refund::build_refund_legs`], design §4.4): the
//!   money-OUT unwind of a settled receipt through the two-stage `REFUND_CLEARING`
//!   liability. Pattern A (`A_UNALLOCATED`) draws down the on-account `UNALLOCATED`
//!   pool; Pattern B (`B_RESTORE_AR`) re-opens the receivable (`AR`). Stage-1
//!   (`initiated`) CREDITS `REFUND_CLEARING`; stage-2 (`confirmed`) DEBITS it back
//!   to `CASH_CLEARING` as the cash leaves (a single-step D1 switch collapses both
//!   into one `… · CR CASH_CLEARING` move). A refund NEVER restates revenue and
//!   NEVER debits `CONTRACT_LIABILITY` (the unreleased-deferred restatement rides a
//!   paired credit note, not the refund). Backed by the Group B infra
//!   [`RefundHandler`](crate::infra::adjustment::refund_service) which resolves the
//!   origin `payment_settlement` (by `payment_id` + `currency`), routes by `phase`,
//!   builds the legs, and posts them atomically with the `refund` row.

pub mod credit_note;
pub mod debit_note;
pub mod manual;
pub mod refund;
pub mod splitter;
