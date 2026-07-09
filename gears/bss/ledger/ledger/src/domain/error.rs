//! `DomainError` — the gear's internal ledger-rejection vocabulary. Per
//! ADR-0005 (SDK canonical projection) this enum is mapped to the AIP-193
//! [`toolkit_canonical_errors::CanonicalError`] envelope by the SINGLE
//! authoritative ladder in [`crate::infra::error_mapping`]; both surfaces (REST
//! and the in-process `LedgerClientV1`) consume that ladder, so a domain
//! variant is assigned a canonical category in exactly one place. SDK
//! consumers see only the typed [`bss_ledger_sdk::LedgerError`] projection of
//! the resulting `CanonicalError` — never this enum.

use toolkit_macros::domain_model;

/// A ledger operation rejection. Each variant carries a human-readable detail
/// and maps to exactly one canonical category in `infra::error_mapping`.
#[domain_model]
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    // ── InvalidArgument (bad request shape / value) ──
    #[error("entry does not net to zero per currency: {0}")]
    Unbalanced(String),
    #[error("entry has no lines: {0}")]
    Empty(String),
    #[error("entry spans more than one payer tenant: {0}")]
    MixedPayer(String),
    #[error("entry is missing its payer: {0}")]
    MissingPayer(String),
    #[error("entry spans more than one legal entity: {0}")]
    MixedLegalEntity(String),
    #[error("lines in the same currency carry different scales: {0}")]
    InconsistentScale(String),
    #[error("amount out of range: {0}")]
    AmountOutOfRange(String),
    #[error("entry exceeds the maximum line count: {0}")]
    EntryTooLarge(String),
    #[error("invalid provisioning request: {0}")]
    InvalidRequest(String),
    #[error("currency scale out of range: {0}")]
    ScaleOutOfRange(String),
    #[error("reusable-credit residual is undisposed: {0}")]
    CreditResidualUndisposed(String),
    #[error("allocation exceeds settled amount: {0}")]
    MoneyOutCapExceeded(String),
    #[error("allocation spans too many invoices: {0}")]
    AllocationTooLarge(String),
    #[error("allocation currency mismatch: {0}")]
    AllocationCurrencyMismatch(String),
    #[error("currency does not match the settled payment: {0}")]
    CurrencyMismatch(String),
    // ── FX & multi-currency (Slice 5) ──
    // Raised at rate-lock time (RateLocker), BEFORE the post txn — like the
    // governed-adjustment guard these are decided pre-post and never ride the
    // posting sentinel; they are listed in domain_parts/from_parts for the
    // exhaustive-match contract only.
    #[error("no acceptable FX rate is available for the currency pair: {0}")]
    FxRateUnavailable(String),
    #[error("FX rate is stale and tenant policy forbids the fallback: {0}")]
    FxRateStaleNotAllowed(String),
    #[error("caller-computed allocation split is invalid: {0}")]
    AllocationSplitInvalid(String),
    #[error("credit grant exceeds available unallocated: {0}")]
    GrantExceedsUnallocated(String),
    #[error("credit application exceeds open AR: {0}")]
    CreditExceedsOpenAr(String),
    #[error("credit application exceeds available wallet: {0}")]
    CreditExceedsWallet(String),
    #[error("recognition schedule exceeds the configured segment ceiling: {0}")]
    ScheduleTooLong(String),
    #[error("multi-PO recognition line is missing a resolvable SSP snapshot ref: {0}")]
    SspSnapshotRequired(String),
    #[error("recognition line is missing a resolvable PO allocation group: {0}")]
    MissingPoAllocationGroup(String),
    #[error("recognition deferral/timing policy is ambiguous or unresolvable: {0}")]
    RecognitionPolicyConflict(String),
    #[error(
        "credit/debit-note recognized-vs-deferred split is ambiguous (no silent pro-rata): {0}"
    )]
    CreditNoteSplitAmbiguous(String),
    #[error("credit note exceeds the invoice's remaining headroom: {0}")]
    CreditNoteExceedsHeadroom(String),
    #[error("refund exceeds the settled amount available to refund: {0}")]
    RefundExceedsSettled(String),
    #[error("refund exceeds the amount allocated to the invoice: {0}")]
    RefundExceedsAllocated(String),
    #[error("schedule modification needs treatment review (not auto-prospective): {0}")]
    ModificationTreatmentReview(String),
    #[error("deferred recognition line has no resolvable invoice-item link: {0}")]
    RecognitionWithoutInvoiceLink(String),
    #[error("manual adjustment is not allowed (governed allow-list / write-off guard): {0}")]
    ManualAdjustmentNotAllowed(String),
    // Group 2B controlled-metadata guard: the PATCH value carried raw customer
    // PII (an email / phone / payment number, or a prohibited key). The
    // secured-audit record is append-only, so the value is screened BEFORE any
    // write and rejected here — a 400 InvalidArgument carrying the
    // `PII_IN_METADATA_VALUE` wire code on the `value` field.
    #[error("metadata value carries prohibited customer PII: {0}")]
    PiiInMetadataValue(String),
    // Group 2C cross-tenant elevation guard: an investigation reason
    // (`reason` + `reason_code`) is required before a cross-tenant audit read
    // may open the target tenant. Architecturally a 422 Unprocessable Entity;
    // the toolkit CanonicalError has no 422 category, so it projects as a 400
    // FailedPrecondition carrying the `MISSING_INVESTIGATION_REASON` wire code
    // (same convention as `PiiInMetadataValue`'s sibling failed-precondition codes).
    #[error("a cross-tenant audit read requires an investigation reason: {0}")]
    MissingInvestigationReason(String),

    // ── PermissionDenied (cross-tenant elevation not authorized) ──
    #[error("cross-tenant access denied: {0}")]
    CrossTenantAccessDenied(String),

    // ── FailedPrecondition (resource state forbids the op) ──
    #[error("fiscal period is closed: {0}")]
    PeriodClosed(String),
    #[error("account is closed: {0}")]
    AccountClosed(String),
    #[error("payer is closed: {0}")]
    PayerClosed(String),
    #[error("account mapping is missing and the tenant policy is HARD_BLOCK: {0}")]
    AccountMappingMissing(String),
    #[error("posting would drive a guarded balance negative: {0}")]
    NegativeBalance(String),
    #[error("settlement return exceeds the returnable settled amount: {0}")]
    SettlementReturnOverAllocated(String),
    #[error("invalid dispute phase transition: {0}")]
    InvalidDisputeTransition(String),
    #[error("chargeback clawback exceeds the settled amount: {0}")]
    ChargebackExceedsSettled(String),
    #[error("chargeback lost on an already-refunded payment: {0}")]
    ChargebackOnRefunded(String),
    #[error("clock skew quarantine: {0}")]
    ClockSkewQuarantine(String),
    #[error("fiscal period is not OPEN: {0}")]
    PeriodNotOpen(String),
    #[error("fiscal period close is blocked: {0}")]
    PeriodCloseBlocked(String),

    // ── Aborted (conflict; the caller may retry) ──
    #[error("a period close is already in progress: {0}")]
    PeriodCloseInProgress(String),
    #[error("idempotency key reused with a different payload: {0}")]
    IdempotencyConflict(String),
    #[error("currency scale is locked: {0}")]
    CurrencyScaleLocked(String),
    #[error("recognition release would exceed the schedule's deferred total: {0}")]
    OverRecognition(String),
    #[error("refund claw-back deferred (out-of-order / would underflow money-out): {0}")]
    RefundClawbackDeferred(String),
    #[error(
        "cross-currency operation not yet supported (functional carry-forward needs a \
         prior locked rate — Slice 7): {0}"
    )]
    FxOperationUnsupported(String),
    #[error("refund held: the origin payment has an open dispute: {0}")]
    RefundDisputeHeld(String),
    #[error("operation requires dual-control approval: {0}")]
    DualControlRequired(String),
    #[error("approver must differ from preparer: {0}")]
    SelfApprovalForbidden(String),
    #[error("approval is not in an actionable state: {0}")]
    ApprovalNotActionable(String),
    #[error("dual-control policy config is out of range: {0}")]
    DualControlPolicyOutOfRange(String),
    #[error("tamper verification failed: {0}")]
    TamperVerificationFailed(String),
    // Slice 6 §4.6 (AC #15): a correction must REUSE the original posting's
    // pinned evidence refs — it may not invent a pinned ref the original never
    // had. A 409 Conflict (aborted) carrying the `POLICY_VERSION_VIOLATION`
    // wire code (same family as the other aborted conflicts).
    #[error("policy version violation: {0}")]
    PolicyVersionViolation(String),

    // ── ResourceExhausted (backpressure) ──
    #[error("tenant posting is locked: {0}")]
    TenantPostingLocked(String),

    // ── NotFound ──
    #[error("fiscal period not found: {0}")]
    PeriodNotFound(String),
    #[error("approval not found: {0}")]
    ApprovalNotFound(String),
    // Group 3A PII erasure / re-identification: no `payer_pii_map` row exists
    // for the `(tenant, payer_tenant_id)` the request targets.
    #[error("payer PII map not found: {0}")]
    PayerPiiNotFound(String),
    #[error("originating posted invoice not found: {0}")]
    NoteInvoiceNotFound(String),
    #[error("origin settled payment not found for refund: {0}")]
    RefundOriginNotFound(String),

    // ── Internal (infrastructure fault; diagnostic stays server-side) ──
    #[error("internal: {0}")]
    Internal(String),
}
