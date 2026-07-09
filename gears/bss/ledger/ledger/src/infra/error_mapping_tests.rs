//! Exhaustive `DomainError` → AIP-193 `CanonicalError` ladder check: every
//! variant maps to its expected canonical category, and a representative set
//! carry the agreed wire code. Locks the machine-readable error contract.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use toolkit::api::canonical_prelude::{CanonicalError, Problem};

use crate::domain::error::DomainError;

/// The canonical category (HTTP-ish status family) of a projected error.
fn status(err: DomainError) -> u16 {
    CanonicalError::from(err).status_code()
}

#[test]
fn each_variant_maps_to_expected_category() {
    let d = || "x".to_owned();
    // (variant, expected canonical HTTP status)
    // FailedPrecondition maps to 400 (same as InvalidArgument per AIP-193 / toolkit).
    let cases: Vec<(DomainError, u16)> = vec![
        (DomainError::Unbalanced(d()), 400),
        (DomainError::Empty(d()), 400),
        (DomainError::MixedPayer(d()), 400),
        (DomainError::MissingPayer(d()), 400),
        (DomainError::MixedLegalEntity(d()), 400),
        (DomainError::InconsistentScale(d()), 400),
        (DomainError::AmountOutOfRange(d()), 400),
        (DomainError::EntryTooLarge(d()), 400),
        (DomainError::InvalidRequest(d()), 400),
        (DomainError::AllocationTooLarge(d()), 400),
        (DomainError::AllocationCurrencyMismatch(d()), 400),
        (DomainError::CurrencyMismatch(d()), 400),
        (DomainError::AllocationSplitInvalid(d()), 400),
        // FX (Slice 5): no acceptable rate is a transient conflict → ABORTED 409;
        // stale-not-allowed is a design-422 → InvalidArgument 400 (no platform 422).
        (DomainError::FxRateUnavailable(d()), 409),
        (DomainError::FxRateStaleNotAllowed(d()), 400),
        // Balance / headroom caps are retriable conflicts → ABORTED → 409 (1a).
        (DomainError::GrantExceedsUnallocated(d()), 409),
        (DomainError::CreditExceedsOpenAr(d()), 409),
        (DomainError::CreditExceedsWallet(d()), 409),
        (DomainError::MoneyOutCapExceeded(d()), 409),
        (DomainError::ScheduleTooLong(d()), 400),
        (DomainError::SspSnapshotRequired(d()), 400),
        (DomainError::MissingPoAllocationGroup(d()), 400),
        (DomainError::RecognitionPolicyConflict(d()), 400),
        // Design-422 → InvalidArgument 400 (the platform has no 422): the headroom
        // cap is NOT a retriable balance race (an over-cap credit note routes via
        // goodwill/non-revenue), so it is a 400, not the ABORTED 409 the money-out
        // caps use.
        (DomainError::CreditNoteExceedsHeadroom(d()), 400),
        // The refund stage-1 cap rejects are design-422 → InvalidArgument 400 (the
        // platform has no 422), like the headroom cap — an over-refund must be
        // corrected, not retried, so they are 400, not the ABORTED 409 the
        // allocate/chargeback money-out caps use.
        (DomainError::RefundExceedsSettled(d()), 400),
        (DomainError::RefundExceedsAllocated(d()), 400),
        (DomainError::ModificationTreatmentReview(d()), 400),
        (DomainError::RecognitionWithoutInvoiceLink(d()), 400),
        (DomainError::ScaleOutOfRange(d()), 400),
        (DomainError::CreditResidualUndisposed(d()), 400),
        // 2C: MissingInvestigationReason is architecturally a 422, but the
        // toolkit CanonicalError has no 422 category — it projects as
        // FailedPrecondition (400). The `MISSING_INVESTIGATION_REASON` wire code
        // is the discriminator.
        (DomainError::MissingInvestigationReason(d()), 400),
        // 2C: an unauthorized cross-tenant elevation is a 403 PermissionDenied.
        (DomainError::CrossTenantAccessDenied(d()), 403),
        (DomainError::PeriodClosed(d()), 400),
        (DomainError::AccountClosed(d()), 400),
        (DomainError::PayerClosed(d()), 400),
        (DomainError::NegativeBalance(d()), 400),
        (DomainError::SettlementReturnOverAllocated(d()), 400),
        (DomainError::InvalidDisputeTransition(d()), 400),
        (DomainError::ChargebackExceedsSettled(d()), 400),
        (DomainError::ChargebackOnRefunded(d()), 400),
        (DomainError::ClockSkewQuarantine(d()), 400),
        (DomainError::PeriodNotOpen(d()), 400),
        (DomainError::IdempotencyConflict(d()), 409),
        (DomainError::CurrencyScaleLocked(d()), 409),
        (DomainError::OverRecognition(d()), 409),
        // Dual-control (VHP-1852): over-threshold / self-approval / non-actionable
        // target / out-of-range policy are all well-formed requests that conflict
        // with current governance state → ABORTED → 409 (no platform 422).
        (DomainError::DualControlRequired(d()), 409),
        (DomainError::SelfApprovalForbidden(d()), 409),
        (DomainError::ApprovalNotActionable(d()), 409),
        (DomainError::DualControlPolicyOutOfRange(d()), 409),
        (DomainError::TamperVerificationFailed(d()), 409),
        // §4.6 (AC #15): a correction that fails to reuse the original's pinned
        // evidence is a 409 Conflict (aborted).
        (DomainError::PolicyVersionViolation(d()), 409),
        // Group E: a deferred refund-of-refund claw-back is ABORTED (409) — accepted
        // and queued, a transient conflict the caller observes / retries (the future
        // REST surface maps it to a 202-like accepted-but-queued).
        (DomainError::RefundClawbackDeferred(d()), 409),
        (DomainError::TenantPostingLocked(d()), 429),
        (DomainError::PeriodNotFound(d()), 404),
        (DomainError::NoteInvoiceNotFound(d()), 404),
        (DomainError::RefundOriginNotFound(d()), 404),
        // Variants previously absent from this list:
        // RefundDisputeHeld is an Aborted 409 (held, re-driven on dispute WON),
        // the sibling of RefundClawbackDeferred.
        (DomainError::RefundDisputeHeld(d()), 409),
        (DomainError::CreditNoteSplitAmbiguous(d()), 400),
        (DomainError::ManualAdjustmentNotAllowed(d()), 400),
        (DomainError::PiiInMetadataValue(d()), 400),
        (DomainError::ApprovalNotFound(d()), 404),
        (DomainError::PayerPiiNotFound(d()), 404),
        (DomainError::Internal(d()), 500),
    ];
    for (variant, want) in cases {
        let label = format!("{variant:?}");
        assert_eq!(status(variant), want, "wrong category for {label}");
    }
}

#[test]
fn unbalanced_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::Unbalanced(
        "dr!=cr".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("LEDGER_ENTRY_UNBALANCED"),
        "missing wire code: {body}"
    );
}

#[test]
fn note_invoice_not_found_carries_resource_wire_code_and_404() {
    // F4: a credit/debit note against an unposted invoice → NOT_FOUND (404),
    // the same not-found shape as `PeriodNotFound` / `ApprovalNotFound`.
    let err = DomainError::NoteInvoiceNotFound("INV-NONE".into());
    assert_eq!(status(err.clone()), 404, "NoteInvoiceNotFound must be 404");
    let problem = Problem::from(CanonicalError::from(err));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("INV-NONE"),
        "missing the offending invoice id in the problem body: {body}"
    );
}

#[test]
fn refund_origin_not_found_carries_resource_wire_code_and_404() {
    // §4.4 / D7: a refund against a payment with no settlement → NOT_FOUND (404),
    // the same not-found shape as `NoteInvoiceNotFound` / `ApprovalNotFound`.
    let err = DomainError::RefundOriginNotFound("PAY-NONE".into());
    assert_eq!(status(err.clone()), 404, "RefundOriginNotFound must be 404");
    let problem = Problem::from(CanonicalError::from(err));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("PAY-NONE"),
        "missing the offending payment id in the problem body: {body}"
    );
}

#[test]
fn allocation_split_invalid_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::AllocationSplitInvalid(
        "over open".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("ALLOCATION_SPLIT_INVALID"),
        "missing wire code: {body}"
    );
}

#[test]
fn credit_note_exceeds_headroom_carries_field_violation_wire_code() {
    // The headroom cap is a design-422 that lands on InvalidArgument (400) — a
    // field violation, NOT the ABORTED reason the retriable money-out caps use.
    let problem = Problem::from(CanonicalError::from(
        DomainError::CreditNoteExceedsHeadroom("over headroom".into()),
    ));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CREDIT_NOTE_EXCEEDS_HEADROOM"),
        "missing wire code: {body}"
    );
}

#[test]
fn refund_exceeds_settled_carries_field_violation_wire_code() {
    // The refund total-money-out / spendable-headroom cap is a design-422 that
    // lands on InvalidArgument (400) — a field violation, NOT the ABORTED reason
    // the retriable money-out caps use (an over-refund must be corrected).
    let problem = Problem::from(CanonicalError::from(DomainError::RefundExceedsSettled(
        "refund > settled".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("REFUND_EXCEEDS_SETTLED"),
        "missing wire code: {body}"
    );
}

#[test]
fn refund_exceeds_allocated_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::RefundExceedsAllocated(
        "refund > allocated".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("REFUND_EXCEEDS_ALLOCATED"),
        "missing wire code: {body}"
    );
}

// Cap rejections now map to ABORTED (409) and carry their wire code as the
// `aborted` reason rather than a field violation (1a); the code string is still
// present in the serialized problem body.
#[test]
fn grant_exceeds_unallocated_carries_aborted_reason_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::GrantExceedsUnallocated(
        "grant > pool".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("GRANT_EXCEEDS_UNALLOCATED"),
        "missing wire code: {body}"
    );
}

#[test]
fn credit_exceeds_open_ar_carries_aborted_reason_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::CreditExceedsOpenAr(
        "target > open".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CREDIT_EXCEEDS_OPEN_AR"),
        "missing wire code: {body}"
    );
}

#[test]
fn credit_exceeds_wallet_carries_aborted_reason_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::CreditExceedsWallet(
        "debit > wallet".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CREDIT_EXCEEDS_WALLET"),
        "missing wire code: {body}"
    );
}

#[test]
fn chargeback_exceeds_settled_carries_precondition_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::ChargebackExceedsSettled(
        "clawback > settled".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CHARGEBACK_EXCEEDS_SETTLED"),
        "missing wire code: {body}"
    );
}

#[test]
fn chargeback_on_refunded_carries_precondition_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::ChargebackOnRefunded(
        "lost on refunded".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CHARGEBACK_ON_REFUNDED"),
        "missing wire code: {body}"
    );
}

#[test]
fn idempotency_conflict_carries_aborted_reason() {
    let problem = Problem::from(CanonicalError::from(DomainError::IdempotencyConflict(
        "dup".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("IDEMPOTENCY_PAYLOAD_CONFLICT"),
        "missing reason: {body}"
    );
}

#[test]
fn over_recognition_carries_aborted_reason() {
    let problem = Problem::from(CanonicalError::from(DomainError::OverRecognition(
        "release > deferred".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(body.contains("OVER_RECOGNITION"), "missing reason: {body}");
}

#[test]
fn refund_clawback_deferred_carries_aborted_reason() {
    // Group E: a deferred claw-back surfaces the REFUND_CLAWBACK_DEFERRED abort
    // reason (409 — accepted-but-queued, the caller retries / observes).
    let problem = Problem::from(CanonicalError::from(DomainError::RefundClawbackDeferred(
        "out-of-order claw-back deferred".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("REFUND_CLAWBACK_DEFERRED"),
        "missing reason: {body}"
    );
}

#[test]
fn currency_mismatch_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::CurrencyMismatch(
        "EUR != USD".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("CURRENCY_MISMATCH"),
        "missing wire code: {body}"
    );
}

#[test]
fn schedule_too_long_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::ScheduleTooLong(
        "121 > 120".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("SCHEDULE_TOO_LONG"),
        "missing wire code: {body}"
    );
}

#[test]
fn ssp_snapshot_required_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::SspSnapshotRequired(
        "multi-PO without snapshot".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("SSP_SNAPSHOT_REQUIRED"),
        "missing wire code: {body}"
    );
}

#[test]
fn missing_po_allocation_group_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(DomainError::MissingPoAllocationGroup(
        "deferring line without a PO group".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("MISSING_PO_ALLOCATION_GROUP"),
        "missing wire code: {body}"
    );
}

#[test]
fn recognition_policy_conflict_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(
        DomainError::RecognitionPolicyConflict("Contract vs Catalog ambiguous".into()),
    ));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("RECOGNITION_POLICY_CONFLICT"),
        "missing wire code: {body}"
    );
}

#[test]
fn missing_investigation_reason_carries_precondition_wire_code() {
    let problem = Problem::from(CanonicalError::from(
        DomainError::MissingInvestigationReason("reason required".into()),
    ));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("MISSING_INVESTIGATION_REASON"),
        "missing wire code: {body}"
    );
}

#[test]
fn modification_treatment_review_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(
        DomainError::ModificationTreatmentReview("catch_up needs review".into()),
    ));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("MODIFICATION_TREATMENT_REVIEW"),
        "missing wire code: {body}"
    );
}

#[test]
fn cross_tenant_access_denied_is_403_with_reason() {
    let err = CanonicalError::from(DomainError::CrossTenantAccessDenied(
        "role not authorized".into(),
    ));
    assert_eq!(
        err.status_code(),
        403,
        "an unauthorized cross-tenant elevation must be a 403 PermissionDenied"
    );
    let body = serde_json::to_string(&Problem::from(err)).unwrap();
    assert!(
        body.contains("CROSS_TENANT_ACCESS_DENIED"),
        "missing reason: {body}"
    );
}

#[test]
fn recognition_without_invoice_link_carries_field_violation_wire_code() {
    let problem = Problem::from(CanonicalError::from(
        DomainError::RecognitionWithoutInvoiceLink("deferred line without invoice_item_ref".into()),
    ));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("RECOGNITION_WITHOUT_INVOICE_LINK"),
        "missing wire code: {body}"
    );
}

#[test]
fn policy_version_violation_is_409_with_aborted_reason() {
    let err = CanonicalError::from(DomainError::PolicyVersionViolation(
        "correction invented a pinned ref".into(),
    ));
    assert_eq!(
        err.status_code(),
        409,
        "a policy-version violation must be a 409 Conflict"
    );
    let body = serde_json::to_string(&Problem::from(err)).unwrap();
    assert!(
        body.contains("POLICY_VERSION_VIOLATION"),
        "missing reason: {body}"
    );
}

#[test]
fn tamper_verification_failed_is_409_with_aborted_reason() {
    let err = CanonicalError::from(DomainError::TamperVerificationFailed("frozen".into()));
    assert_eq!(
        err.status_code(),
        409,
        "tamper freeze must be a 409 Conflict"
    );
    let body = serde_json::to_string(&Problem::from(err)).unwrap();
    assert!(
        body.contains("TAMPER_VERIFICATION_FAILED"),
        "missing reason: {body}"
    );
}

#[test]
fn fx_rate_unavailable_is_409_with_aborted_reason() {
    // No acceptable local FX rate for the pair → a transient conflict the caller
    // may retry once the sync job catches up → ABORTED 409.
    let err = CanonicalError::from(DomainError::FxRateUnavailable(
        "no rate for EUR->USD".into(),
    ));
    assert_eq!(
        err.status_code(),
        409,
        "no acceptable FX rate must be a 409 Conflict"
    );
    let body = serde_json::to_string(&Problem::from(err)).unwrap();
    assert!(
        body.contains("FX_RATE_UNAVAILABLE"),
        "missing reason: {body}"
    );
}

#[test]
fn fx_rate_stale_not_allowed_carries_field_violation_wire_code() {
    // Design-422 → InvalidArgument 400 (no platform 422): a field violation, not
    // the ABORTED reason the transient FX-unavailable conflict uses.
    let problem = Problem::from(CanonicalError::from(DomainError::FxRateStaleNotAllowed(
        "stale 8d, tenant forbids fallback".into(),
    )));
    let body = serde_json::to_string(&problem).unwrap();
    assert!(
        body.contains("FX_RATE_STALE_NOT_ALLOWED"),
        "missing wire code: {body}"
    );
}
