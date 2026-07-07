//! Adjustments infra (Slice 3) — the in-transaction handlers that drive the pure
//! adjustment domain (`crate::domain::adjustment`) through the foundation engine.
//!
//! Phase 1 / Group C ships the [`credit_note_service::CreditNoteHandler`]: the
//! orchestrator that posts a credit note's balanced compensating entry (design
//! §4.2) and, in the SAME ACID txn (via a [`PostSidecar`](crate::infra::posting::service::PostSidecar)),
//! reduces the owning `recognition_schedule`'s deferred total, seeds + bumps the
//! `invoice_exposure` headroom counter (the authoritative cap CHECK), seeds the
//! reusable-credit wallet remainder, and persists the `credit_note` row.
//!
//! Phase 1 / Group D ships the [`debit_note_service::DebitNoteHandler`]: the
//! orchestrator for an *additional charge* against a posted invoice. It posts a
//! **direct-split** entry that mirrors the Slice-1 invoice-post (DR `AR` / CR
//! `REVENUE` / CR `CONTRACT_LIABILITY` / CR `TAX_PAYABLE`) — NOT a compensating
//! reduction — and in the SAME ACID txn: builds the releasing `recognition_schedule`
//! when the note defers (D4, reusing the invoice-post's
//! [`ScheduleBuilderSidecar`](crate::infra::recognition::sidecar)), **raises** the
//! `invoice_exposure` headroom (`debit_note_total_minor += amount`), and persists
//! the `debit_note` row.
//!
//! Phase 2 / Group B ships the [`refund_service::RefundHandler`]: the orchestrator
//! for a **money-OUT** refund against a settled receipt (design §4.4). It resolves
//! the origin `payment_settlement` (by `payment_id` + `currency`), routes by
//! `phase` to the two-leg shape (stage-1 `… · CR REFUND_CLEARING`; stage-2 `DR
//! REFUND_CLEARING · CR CASH_CLEARING`; or the single-step `… · CR CASH_CLEARING`),
//! and posts it atomically with the `refund` record row via the
//! [`refund_service::RefundPostSidecar`]. Pattern A draws down `UNALLOCATED`,
//! Pattern B re-opens `AR`; a refund NEVER restates revenue and NEVER debits
//! `CONTRACT_LIABILITY`. The cap increments (Group C), dual-control (Group D),
//! refund-of-refund (Group E), `unknown_final` disposition (Group F), and REST
//! (Group G) land in later groups.
//!
//! Phase 3 / Group 4 ships the
//! [`manual_adjustment_service::ManualAdjustmentHandler`]: the orchestrator for a
//! **governed** manual adjustment (design §4.6) — the ledger's escape hatch for
//! corrections the typed flows do not cover (rounding residue, suspense /
//! cash-clearing clean-up). It clears the pure
//! [`govern`](crate::domain::adjustment::manual::govern) gate, posts the balanced
//! entry through the engine (idempotent on `(tenant, MANUAL_ADJUSTMENT,
//! adjustment_id)`), and publishes `billing.ledger.manual_adjustment.posted` in the
//! post txn. A disguised bad-debt write-off is rejected (`MANUAL_ADJUSTMENT_NOT_ALLOWED`)
//! and additionally captured (`SecuredAuditSink`) + paged (the `AttemptedWriteOff`
//! alarm) out-of-band. Dual-control (`SoD` + threshold) is Group 5.

pub mod credit_note_service;
pub mod debit_note_service;
pub mod manual_adjustment_service;
pub mod refund_service;
