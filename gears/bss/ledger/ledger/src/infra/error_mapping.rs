//! The SINGLE authoritative `DomainError` → AIP-193 `CanonicalError` ladder
//! (ADR-0005). Both surfaces consume it: REST handlers via `?`/explicit map,
//! and the in-process `LedgerClientV1` via `.map_err(CanonicalError::from)`.
//! There is no parallel `DomainError → LedgerError` path — the SDK's typed
//! `LedgerError` is projected from `CanonicalError`, so a domain variant is
//! assigned a canonical category in exactly one place: here.
//!
//! Wire codes (the `field_violation` reason / `precondition` type / `aborted`
//! reason / `quota` subject strings) are the machine-readable discriminators a
//! consumer matches on after the coarse category; they mirror the old
//! `LedgerErrorCode` `SCREAMING_SNAKE` names so the contract is unchanged.

use toolkit::api::canonical_prelude::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error("gts.cf.bss.ledger.entry.v1~")]
struct EntryResource;
#[resource_error("gts.cf.bss.ledger.ledger.v1~")]
struct LedgerResource;
#[resource_error("gts.cf.bss.ledger.fiscal_period.v1~")]
struct FiscalPeriodResource;

impl From<DomainError> for CanonicalError {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per DomainError variant — the error taxonomy is flat by design"
    )]
    fn from(err: DomainError) -> Self {
        use DomainError as D;
        match err {
            // ── InvalidArgument ──
            D::Unbalanced(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "LEDGER_ENTRY_UNBALANCED")
                .create(),
            D::Empty(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "LEDGER_ENTRY_EMPTY")
                .create(),
            D::MixedPayer(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "MIXED_PAYER_TENANT")
                .create(),
            D::MissingPayer(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "MISSING_PAYER")
                .create(),
            D::MixedLegalEntity(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "MIXED_LEGAL_ENTITY")
                .create(),
            D::InconsistentScale(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "AMOUNT_OUT_OF_RANGE")
                .create(),
            D::AmountOutOfRange(d) => EntryResource::invalid_argument()
                .with_field_violation("amount_minor", d, "AMOUNT_OUT_OF_RANGE")
                .create(),
            D::EntryTooLarge(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "LEDGER_ENTRY_TOO_LARGE")
                .create(),
            D::InvalidRequest(d) => LedgerResource::invalid_argument()
                .with_constraint(d)
                .create(),
            D::ScaleOutOfRange(d) => LedgerResource::invalid_argument()
                .with_field_violation("currency_scales", d, "CURRENCY_SCALE_OUT_OF_RANGE")
                .create(),
            D::CreditResidualUndisposed(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "CREDIT_RESIDUAL_UNDISPOSED")
                .create(),
            D::AllocationTooLarge(d) => EntryResource::invalid_argument()
                .with_field_violation("splits", d, "ALLOCATION_TOO_LARGE")
                .create(),
            D::AllocationCurrencyMismatch(d) => EntryResource::invalid_argument()
                .with_field_violation("currency", d, "ALLOCATION_CURRENCY_MISMATCH")
                .create(),
            D::CurrencyMismatch(d) => EntryResource::invalid_argument()
                .with_field_violation("currency", d, "CURRENCY_MISMATCH")
                .create(),
            // FX rate stale + tenant forbids fallback. Design asks for 422; the
            // platform CanonicalError ladder has no 422, so it lands on
            // InvalidArgument (400) like the other config-gap codes.
            D::FxRateStaleNotAllowed(d) => EntryResource::invalid_argument()
                .with_field_violation("rate_snapshot_ref", d, "FX_RATE_STALE_NOT_ALLOWED")
                .create(),
            D::AllocationSplitInvalid(d) => EntryResource::invalid_argument()
                .with_field_violation("splits", d, "ALLOCATION_SPLIT_INVALID")
                .create(),
            D::ScheduleTooLong(d) => EntryResource::invalid_argument()
                .with_field_violation("segments", d, "SCHEDULE_TOO_LONG")
                .create(),
            D::SspSnapshotRequired(d) => EntryResource::invalid_argument()
                .with_field_violation("ssp_snapshot_ref", d, "SSP_SNAPSHOT_REQUIRED")
                .create(),
            D::MissingPoAllocationGroup(d) => EntryResource::invalid_argument()
                .with_field_violation("po_allocation_group", d, "MISSING_PO_ALLOCATION_GROUP")
                .create(),
            D::RecognitionPolicyConflict(d) => EntryResource::invalid_argument()
                .with_field_violation("policy_ref", d, "RECOGNITION_POLICY_CONFLICT")
                .create(),
            // The recognized-vs-deferred split basis is indeterminable
            // (block-on-ambiguous, design §4.2 — never a silent pro-rata). A
            // design-422; like the other adjustment/recognition config-gap codes
            // it lands on the InvalidArgument category (400) — the platform has no
            // 422. The Group C handler additionally raises the
            // `CreditNoteSplitBlocked` alarm + an exception stub (Slice 7).
            D::CreditNoteSplitAmbiguous(d) => EntryResource::invalid_argument()
                .with_field_violation("split", d, "CREDIT_NOTE_SPLIT_AMBIGUOUS")
                .create(),
            // A credit note (incl. tax) would push `credit_note_total_minor` past
            // the invoice's headroom `original_total + Σ debit notes − Σ prior
            // credit notes` — the `invoice_exposure` CHECK rejected the in-txn
            // bump (design §4.2 / §4.7, AC #24). The design asks for a 422; like
            // the other adjustment/recognition config-gap codes it lands on the
            // InvalidArgument category (400) — the platform has no 422. (It is the
            // headroom CAP, NOT a retriable balance race, so it is an
            // InvalidArgument 400 rather than the ABORTED 409 the money-out caps
            // use: an over-cap credit note must route via goodwill/non-revenue,
            // never silently retried through S3 — design §4.2.)
            D::CreditNoteExceedsHeadroom(d) => EntryResource::invalid_argument()
                .with_field_violation("amount_minor", d, "CREDIT_NOTE_EXCEEDS_HEADROOM")
                .create(),
            // A refund stage-1 initiation whose cap would be exceeded: the total
            // money-out cap (`refunded + clawed_back <= settled`, or the
            // Pattern-A spendable-headroom `allocated + refunded_unallocated <=
            // settled`) for `RefundExceedsSettled`, the Pattern-B per-`(payment,
            // invoice)` cap (`refunded <= allocated`) for `RefundExceedsAllocated`.
            // The `payment_settlement` / `payment_allocation_refund` CHECK rejected
            // the in-txn increment under the rank-1 lock, BEFORE the cash left
            // (design §4.4 / §4.7, §5). The design asks for a 422; like the other
            // adjustment cap codes it lands on the InvalidArgument category (400) —
            // the platform `CanonicalError` ladder has no 422. (An over-refund is a
            // hard cap, not a retriable balance race, so it is InvalidArgument 400
            // rather than the ABORTED 409 the allocate/chargeback money-out caps
            // use — a refund over the settled/allocated amount must be corrected,
            // never silently retried.)
            D::RefundExceedsSettled(d) => EntryResource::invalid_argument()
                .with_field_violation("amount_minor", d, "REFUND_EXCEEDS_SETTLED")
                .create(),
            D::RefundExceedsAllocated(d) => EntryResource::invalid_argument()
                .with_field_violation("amount_minor", d, "REFUND_EXCEEDS_ALLOCATED")
                .create(),
            // A schedule modification arrived with a `catch_up` / unknown
            // treatment: the ledger does not own the catch-up decision and never
            // silently applies a modification prospectively (design §3.6), so it
            // surfaces for upstream review rather than mutating schedule state.
            // The platform has no 422, so this design-422 lands on the
            // InvalidArgument category (400) like the other recognition config-gap
            // codes (`SCHEDULE_TOO_LONG` / `SSP_SNAPSHOT_REQUIRED` / …).
            D::ModificationTreatmentReview(d) => EntryResource::invalid_argument()
                .with_field_violation("treatment", d, "MODIFICATION_TREATMENT_REVIEW")
                .create(),
            // A deferred recognition line was assembled without a resolvable
            // `source_invoice_item_ref` (the §4.7 invoice-link invariant — a
            // deferred Contract-liability balance must anchor to the invoice line
            // it draws down). Blocked BEFORE the post (no orphan schedule). Like
            // the other recognition config-gap codes this design-422 lands on the
            // InvalidArgument category (400) — the platform has no 422.
            D::RecognitionWithoutInvoiceLink(d) => EntryResource::invalid_argument()
                .with_field_violation(
                    "source_invoice_item_ref",
                    d,
                    "RECOGNITION_WITHOUT_INVOICE_LINK",
                )
                .create(),
            // A governed manual adjustment failed the code-owned allow-list, the
            // global REVENUE/CONTRACT_LIABILITY ban, or the write-off structural
            // guard (design §4.6 / Rev3 S3-minor). Both the generic
            // `ManualAdjustmentReject::NotAllowed` and the `AttemptedWriteOff`
            // signal collapse onto this one variant; the design asks for a 422 but
            // the platform `CanonicalError` ladder has no 422, so — like the other
            // adjustment config-gap codes — it lands on the InvalidArgument
            // category (400). The Group 3 handler additionally treats the
            // write-off SHAPE as an alarm: a `SecuredAuditSink` capture + page
            // (`AttemptedWriteOff`) fired out-of-band, exactly as the credit-note
            // split-block code raises its alarm.
            D::ManualAdjustmentNotAllowed(d) => EntryResource::invalid_argument()
                .with_field_violation("lines", d, "MANUAL_ADJUSTMENT_NOT_ALLOWED")
                .create(),
            // Group 2B controlled-metadata guard: the PATCH value carried raw
            // customer PII (an email / phone / payment number, or a prohibited
            // key). The audit chain is append-only, so the value is screened
            // before any write. A 400 `InvalidArgument` carrying the
            // `PII_IN_METADATA_VALUE` wire code on the `value` field.
            D::PiiInMetadataValue(d) => EntryResource::invalid_argument()
                .with_field_violation("value", d, "PII_IN_METADATA_VALUE")
                .create(),
            // Group 2C cross-tenant elevation guard: a cross-tenant audit read
            // arrived without an investigation reason (`reason` + `reason_code`).
            // Architecturally a 422 Unprocessable Entity; the toolkit
            // CanonicalError model has no 422 category, so this is a
            // `FailedPrecondition` (HTTP 400) carrying the
            // `MISSING_INVESTIGATION_REASON` wire code. The code is the
            // discriminator consumers match on.
            D::MissingInvestigationReason(d) => EntryResource::failed_precondition()
                .with_precondition_violation(
                    "investigation_reason",
                    d,
                    "MISSING_INVESTIGATION_REASON",
                )
                .create(),

            // ── PermissionDenied ──
            // Group 2C cross-tenant elevation guard: the caller's role is not
            // authorized to open another tenant's audit data. A 403
            // `PermissionDenied` carrying the `CROSS_TENANT_ACCESS_DENIED` wire
            // code (mirrors `authz_error_to_canonical`, which also carries the
            // deny reason on a `permission_denied`).
            D::CrossTenantAccessDenied(_d) => EntryResource::permission_denied()
                .with_reason("CROSS_TENANT_ACCESS_DENIED")
                .create(),

            // ── FailedPrecondition ──
            D::PeriodClosed(d) => FiscalPeriodResource::failed_precondition()
                .with_precondition_violation("fiscal_period", d, "PERIOD_CLOSED")
                .create(),
            D::AccountClosed(d) => EntryResource::failed_precondition()
                .with_precondition_violation("account", d, "ACCOUNT_CLOSED")
                .create(),
            D::AccountMappingMissing(d) => EntryResource::failed_precondition()
                .with_precondition_violation("account_mapping", d, "ACCOUNT_MAPPING_MISSING")
                .create(),
            D::PayerClosed(d) => EntryResource::failed_precondition()
                .with_precondition_violation("payer", d, "PAYER_CLOSED")
                .create(),
            D::NegativeBalance(d) => EntryResource::failed_precondition()
                .with_precondition_violation("account_balance", d, "NEGATIVE_BALANCE_VIOLATION")
                .create(),
            D::SettlementReturnOverAllocated(d) => EntryResource::failed_precondition()
                .with_precondition_violation("settled_minor", d, "SETTLEMENT_RETURN_OVER_ALLOCATED")
                .create(),
            D::InvalidDisputeTransition(d) => EntryResource::failed_precondition()
                .with_precondition_violation("dispute_phase", d, "INVALID_DISPUTE_PHASE")
                .create(),
            D::ChargebackExceedsSettled(d) => EntryResource::failed_precondition()
                .with_precondition_violation("clawed_back_minor", d, "CHARGEBACK_EXCEEDS_SETTLED")
                .create(),
            D::ChargebackOnRefunded(d) => EntryResource::failed_precondition()
                .with_precondition_violation("clawed_back_minor", d, "CHARGEBACK_ON_REFUNDED")
                .create(),
            D::ClockSkewQuarantine(d) => EntryResource::failed_precondition()
                .with_precondition_violation("clock", d, "CLOCK_SKEW_QUARANTINE")
                .create(),
            D::PeriodNotOpen(d) => FiscalPeriodResource::failed_precondition()
                .with_precondition_violation("fiscal_period", d, "PERIOD_NOT_OPEN")
                .create(),

            // ── Aborted (409) ──
            // Balance / headroom caps are retriable conflicts on mutable state
            // ("re-read the balance and retry"): the request is well-formed but
            // raced or exceeded the *current* cap, so they map to ABORTED → 409,
            // not InvalidArgument → 400. The platform has no 422, so the design's
            // 422/409 cap codes all land on 409 here (design doc updated to match).
            D::IdempotencyConflict(d) => EntryResource::aborted(d)
                .with_reason("IDEMPOTENCY_PAYLOAD_CONFLICT")
                .create(),
            D::CurrencyScaleLocked(d) => LedgerResource::aborted(d)
                .with_reason("CURRENCY_SCALE_LOCKED")
                .create(),
            D::MoneyOutCapExceeded(d) => EntryResource::aborted(d)
                .with_reason("ALLOCATION_EXCEEDS_SETTLED")
                .create(),
            D::GrantExceedsUnallocated(d) => EntryResource::aborted(d)
                .with_reason("GRANT_EXCEEDS_UNALLOCATED")
                .create(),
            D::CreditExceedsOpenAr(d) => EntryResource::aborted(d)
                .with_reason("CREDIT_EXCEEDS_OPEN_AR")
                .create(),
            D::CreditExceedsWallet(d) => EntryResource::aborted(d)
                .with_reason("CREDIT_EXCEEDS_WALLET")
                .create(),
            // No acceptable local FX rate for the pair (all providers stale/absent,
            // no allowed stale fallback). A transient conflict — a later RateSyncJob
            // tick may resolve it — so ABORTED → 409 (design §5).
            D::FxRateUnavailable(d) => EntryResource::aborted(d)
                .with_reason("FX_RATE_UNAVAILABLE")
                .create(),
            // A concurrent close holds the single-active `coord` lease — a
            // transient conflict the caller may retry once the holder finishes.
            D::PeriodCloseInProgress(d) => FiscalPeriodResource::aborted(d)
                .with_reason("PERIOD_CLOSE_IN_PROGRESS")
                .create(),
            // The close gate found a blocking condition (tie-out variance, an open
            // close-blocking exception, a pending mapping, a due-but-unrecognised
            // segment, or a control-feed gate). The body carries the accumulated
            // `blocked_reasons`; resolve them and retry. A conflict on the period's
            // current state ⇒ ABORTED → 409 (spec §3.5, the sibling of
            // PeriodCloseInProgress), NOT FailedPrecondition → 400.
            D::PeriodCloseBlocked(d) => FiscalPeriodResource::aborted(d)
                .with_reason("PERIOD_CLOSE_BLOCKED")
                .create(),
            // The per-schedule `recognized_minor <= total_deferred_minor` CHECK
            // rejected a release (a concurrent run already drew the schedule down,
            // or a stale-"satisfied" gate over-released). A conflict the caller can
            // retry against the then-current `recognized_minor` ⇒ 409, mirroring
            // `IdempotencyConflict` (design §4.3 / §5).
            D::OverRecognition(d) => EntryResource::aborted(d)
                .with_reason("OVER_RECOGNITION")
                .create(),
            // Refund-of-refund claw-back DEFERRED (Group E, design §4.4 / Rev3): the
            // PSP claw-back arrived BEFORE / without the matching outbound refund
            // stage-1 (or claws back more than was refunded), so applying its
            // money-out decrement now would underflow the counter. It was durably
            // QUEUED (never hard-failed) and the drain retries it once the matching
            // outbound lands — a transient conflict the caller observes / retries
            // (the future REST surface maps it to a 202-like accepted-but-queued).
            D::RefundClawbackDeferred(d) => EntryResource::aborted(d)
                .with_reason("REFUND_CLAWBACK_DEFERRED")
                .create(),
            // Cross-currency operation not yet supported (Slice 5 remediation): a
            // refund-of-refund claw-back or a mapping-correction whose functional
            // carry-forward needs a prior locked rate (Slice 7) is rejected up front
            // — NOT queued (unlike `RefundClawbackDeferred`): a permanent precondition
            // failure the caller resolves by routing the operation through a manual
            // adjustment.
            D::FxOperationUnsupported(d) => EntryResource::failed_precondition()
                .with_precondition_violation("entry", d, "FX_OPERATION_UNSUPPORTED")
                .create(),
            // Refund dispute-hold (Z5-2, design §5): the origin payment has an OPEN
            // dispute, so moving the refund's cash leg now would pay out funds that
            // are sub judice (held in `DISPUTE_HOLD` / reclassed `DISPUTED`). The
            // refund was durably HELD on the `REFUND_DISPUTE_HOLD` queue (never
            // hard-failed, never posted) and the hold drain re-drives it once the
            // dispute resolves WON (or cancels it on LOST — the chargeback already
            // returned the money). A transient conflict the caller observes / the
            // REST surface maps to a 202-like accepted-but-held (mirrors
            // `RefundClawbackDeferred`).
            D::RefundDisputeHeld(d) => EntryResource::aborted(d)
                .with_reason("REFUND_DISPUTE_HELD")
                .create(),
            // Dual-control (VHP-1852): an over-threshold mutation needs a second
            // actor; self-approval, a non-actionable target (wrong state / lost
            // race / expired), and out-of-range policy config are all well-formed
            // requests that conflict with current governance state → 409.
            D::DualControlRequired(d) => EntryResource::aborted(d)
                .with_reason("DUAL_CONTROL_REQUIRED")
                .create(),
            D::SelfApprovalForbidden(d) => EntryResource::aborted(d)
                .with_reason("SELF_APPROVAL_FORBIDDEN")
                .create(),
            D::ApprovalNotActionable(d) => EntryResource::aborted(d)
                .with_reason("APPROVAL_NOT_ACTIONABLE")
                .create(),
            D::DualControlPolicyOutOfRange(d) => EntryResource::aborted(d)
                .with_reason("DUAL_CONTROL_POLICY_OUT_OF_RANGE")
                .create(),

            D::TamperVerificationFailed(d) => EntryResource::aborted(d)
                .with_reason("TAMPER_VERIFICATION_FAILED")
                .create(),
            // Slice 6 §4.6 (AC #15): a correction invented a pinned evidence ref
            // the original never had — financial corrections must reuse the
            // original's pinned evidence. A 409 Conflict carrying the
            // `POLICY_VERSION_VIOLATION` reason (same family as the other aborted
            // conflicts).
            D::PolicyVersionViolation(d) => EntryResource::aborted(d)
                .with_reason("POLICY_VERSION_VIOLATION")
                .create(),
            // ── ResourceExhausted ──
            D::TenantPostingLocked(d) => EntryResource::resource_exhausted(d.clone())
                .with_quota_violation("TENANT_POSTING_LOCKED", d)
                .create(),

            // ── NotFound ──
            D::PeriodNotFound(d) => FiscalPeriodResource::not_found(d.clone())
                .with_resource(d)
                .create(),
            // `ApprovalNotFound` (dual-control), `PayerPiiNotFound` (Group 3A PII
            // erasure / re-identification), `NoteInvoiceNotFound`, and
            // `RefundOriginNotFound` share the entry-resource 404 (resource-based,
            // no distinct wire code — the gear's not-found shape).
            // `NoteInvoiceNotFound`: a credit/debit note named an `origin_invoice_id`
            // with no posted `INVOICE_POST` entry (design §4.2 / §5 — a note MUST
            // link an originating posted invoice). `RefundOriginNotFound`: a refund
            // named a `payment_id` with no `payment_settlement` (design §4.4 / §9 D7
            // — a refund MUST unwind a settled receipt). All are scoped existence, so
            // a foreign-tenant row is indistinguishable from absent (no leak).
            D::ApprovalNotFound(d)
            | D::PayerPiiNotFound(d)
            | D::NoteInvoiceNotFound(d)
            | D::RefundOriginNotFound(d) => EntryResource::not_found(d.clone())
                .with_resource(d)
                .create(),

            // ── Internal (diagnostic stays server-side) ──
            D::Internal(d) => CanonicalError::internal(format!("ledger: {d}")).create(),
        }
    }
}

#[cfg(test)]
#[path = "error_mapping_tests.rs"]
mod tests;
