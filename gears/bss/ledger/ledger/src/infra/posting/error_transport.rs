//! Typed business errors across legacy transaction and sidecar interfaces.
use crate::domain::error::DomainError;
use sea_orm::DbErr;
use toolkit_db::DbError;
const SENTINEL_TAG: &str = "LEDGER_POST_ERR";
const SENTINEL_SEP: char = '\u{1f}';

/// Map a [`RepoError`](crate::domain::model::RepoError) to its typed
/// [`DomainError`]: a repo db/row failure is an infrastructure fault; a
/// scale-locked / out-of-range rejection maps to its domain variant.
///
/// The one mapping table: [`repo_to_db`] wraps it for transaction closures, and
/// in-process callers use it directly instead of a sentinel round trip.
pub(crate) fn repo_to_domain(e: crate::domain::model::RepoError) -> DomainError {
    use crate::domain::model::RepoError;
    match e {
        RepoError::InvalidRequest(detail) => DomainError::InvalidRequest(detail),
        RepoError::ApprovalPolicyOutOfRange(detail) => {
            DomainError::DualControlPolicyOutOfRange(detail)
        }
        RepoError::Conflict(detail) => DomainError::ConcurrentModification(detail),
        RepoError::DisputeNotOpen(detail) => DomainError::InvalidDisputeTransition(detail),
        RepoError::RecognitionPolicyConflict(detail) => {
            DomainError::RecognitionPolicyConflict(detail)
        }
        RepoError::Money(error) => crate::domain::exact_money::map_money_error(error),
        // Corrupt row text can contain DB classifier tokens. Keep its known
        // internal category before anything inspects the text.
        invalid @ RepoError::InvalidStoredMoney(_) => DomainError::Internal(invalid.to_string()),
        RepoError::CurrencyScaleLocked(c) => {
            DomainError::CurrencyScaleLocked(format!("currency scale locked: {c}"))
        }
        // A wrong per-line scale changes the implied magnitude → out-of-range
        // (wire `AMOUNT_OUT_OF_RANGE`), preserving the prior posting contract.
        RepoError::ScaleOutOfRange(c) => {
            DomainError::AmountOutOfRange(format!("scale out of range: {c}"))
        }
        other => DomainError::Internal(other.to_string()),
    }
}

/// Map a currency-scale resolution failure to its typed [`DomainError`]: an
/// unregistered non-ISO currency is the caller's `InvalidRequest`, a storage
/// failure goes through [`repo_to_domain`], and a corrupt registry row is an
/// internal fault (the row must be repaired, the caller cannot fix it).
pub(crate) fn scale_error_to_domain(e: crate::domain::money::ScaleError) -> DomainError {
    use crate::domain::money::ScaleError;
    match e {
        ScaleError::UnknownCurrencyScale(currency) => {
            DomainError::InvalidRequest(format!("no scale for currency: {currency}"))
        }
        ScaleError::Repo(error) => repo_to_domain(error),
        corrupt @ ScaleError::CorruptStoredScale { .. } => {
            DomainError::Internal(corrupt.to_string())
        }
    }
}

/// Map a [`RepoError`](crate::domain::model::RepoError) into the sentinel
/// `DbError` a transaction closure returns: [`business`] over [`repo_to_domain`].
pub(crate) fn repo_to_db(e: crate::domain::model::RepoError) -> DbError {
    business(repo_to_domain(e))
}

/// Encode a business [`DomainError`] as a sentinel `DbError` so the transaction
/// closure (whose error type is fixed to `DbError`) rolls back yet preserves
/// the rejection for decoding after `transaction()` returns. The payload is a
/// `DbErr::Custom`; the ledger retry boundary decodes it before classification.
/// Only the explicit concurrency variant retries; other business errors stop.
pub(crate) fn business(err: DomainError) -> DbError {
    let (tag, detail) = domain_parts(err);
    DbError::Sea(DbErr::Custom(format!(
        "{SENTINEL_TAG}{SENTINEL_SEP}{tag}{SENTINEL_SEP}{detail}"
    )))
}

/// Encode an internal (infrastructure) failure as a non-sentinel `DbError`.
pub(crate) fn infra(message: impl Into<String>) -> DbError {
    DbError::Sea(DbErr::Custom(message.into()))
}

/// Decode a `DbError` returned from a `transaction_with_retry` back into a
/// [`DomainError`]: a sentinel-tagged `DbErr::Custom` (written by [`business`])
/// yields the original business rejection; any other `DbError` is an
/// infrastructure fault ([`DomainError::Internal`]). Shared by service paths
/// that run their own sentinel-carrying transaction (e.g. the audit-surface
/// cross-tenant elevation txn) and need the post path's decode semantics.
pub(crate) fn decode_business_error(db_err: &DbError) -> DomainError {
    decode_post_error(db_err)
}

/// Decode a `DbError` returned from `transaction()` back into a [`DomainError`]:
/// a sentinel-tagged `DbErr::Custom` yields the original business rejection; any
/// other `DbError` is an infrastructure fault ([`DomainError::Internal`]).
pub(crate) fn decode_post_error(db_err: &DbError) -> DomainError {
    if let Some(error) = decode_sentinel(db_err) {
        return error;
    }
    // A concurrent wallet over-draw that slips past the app-level pre-check trips
    // the DB no-negative CHECK on reusable_credit_subbalance; surface it as the
    // clean CreditExceedsWallet (→409) rather than an opaque Internal (500). No
    // money is lost (the CHECK held); only the wire surface is corrected.
    if db_err
        .to_string()
        .contains("chk_reusable_credit_subbalance_no_negative")
    {
        return DomainError::CreditExceedsWallet(
            "concurrent wallet over-draw rejected by the no-negative guard".to_owned(),
        );
    }
    DomainError::Internal(db_err.to_string())
}

/// Decode only our business envelope, before any database classification.
pub(crate) fn decode_sentinel(db_err: &DbError) -> Option<DomainError> {
    let DbError::Sea(DbErr::Custom(payload)) = db_err else {
        return None;
    };
    let rest = payload.strip_prefix(&format!("{SENTINEL_TAG}{SENTINEL_SEP}"))?;
    let Some((tag, detail)) = rest.split_once(SENTINEL_SEP) else {
        return Some(DomainError::Internal(
            "malformed posting error envelope".to_owned(),
        ));
    };
    Some(domain_from_parts(tag, detail.to_owned()))
}

/// Split a [`DomainError`] into a stable per-variant tag + its detail for the
/// sentinel round-trip. Exhaustive, so a new variant forces a tag here; the
/// tags are internal to the post txn (encoded and decoded in this module only).
fn domain_parts(err: DomainError) -> (&'static str, String) {
    use DomainError as D;
    match err {
        D::ConcurrentModification(d) => ("ConcurrentModification", d),
        D::InvalidPostingIncrement(d) => ("InvalidPostingIncrement", d),
        D::Unbalanced(d) => ("Unbalanced", d),
        D::Empty(d) => ("Empty", d),
        D::MixedPayer(d) => ("MixedPayer", d),
        D::MissingPayer(d) => ("MissingPayer", d),
        D::MixedLegalEntity(d) => ("MixedLegalEntity", d),
        D::InconsistentScale(d) => ("InconsistentScale", d),
        D::AmountOutOfRange(d) => ("AmountOutOfRange", d),
        D::EntryTooLarge(d) => ("EntryTooLarge", d),
        D::InvalidRequest(d) => ("InvalidRequest", d),
        D::ScaleOutOfRange(d) => ("ScaleOutOfRange", d),
        D::CreditResidualUndisposed(d) => ("CreditResidualUndisposed", d),
        D::MoneyOutCapExceeded(d) => ("MoneyOutCapExceeded", d),
        D::AllocationTooLarge(d) => ("AllocationTooLarge", d),
        D::AllocationCurrencyMismatch(d) => ("AllocationCurrencyMismatch", d),
        D::CurrencyMismatch(d) => ("CurrencyMismatch", d),
        // FX rate errors are raised pre-post (rate-lock); listed for the
        // exhaustive-match contract only (they never ride the sentinel).
        D::FxRateUnavailable(d) => ("FxRateUnavailable", d),
        D::FxRateStaleNotAllowed(d) => ("FxRateStaleNotAllowed", d),
        D::AllocationSplitInvalid(d) => ("AllocationSplitInvalid", d),
        D::GrantExceedsUnallocated(d) => ("GrantExceedsUnallocated", d),
        D::CreditExceedsOpenAr(d) => ("CreditExceedsOpenAr", d),
        D::CreditExceedsWallet(d) => ("CreditExceedsWallet", d),
        D::ScheduleTooLong(d) => ("ScheduleTooLong", d),
        D::SspSnapshotRequired(d) => ("SspSnapshotRequired", d),
        D::MissingPoAllocationGroup(d) => ("MissingPoAllocationGroup", d),
        D::RecognitionPolicyConflict(d) => ("RecognitionPolicyConflict", d),
        D::CreditNoteSplitAmbiguous(d) => ("CreditNoteSplitAmbiguous", d),
        D::CreditNoteExceedsHeadroom(d) => ("CreditNoteExceedsHeadroom", d),
        // The refund cap CHECKs fire INSIDE the post txn (the RefundPostSidecar's
        // counter increments), so these ride the sentinel to surface to the caller.
        D::RefundExceedsSettled(d) => ("RefundExceedsSettled", d),
        D::RefundExceedsAllocated(d) => ("RefundExceedsAllocated", d),
        D::ModificationTreatmentReview(d) => ("ModificationTreatmentReview", d),
        D::RecognitionWithoutInvoiceLink(d) => ("RecognitionWithoutInvoiceLink", d),
        D::PiiInMetadataValue(d) => ("PiiInMetadataValue", d),
        D::MissingInvestigationReason(d) => ("MissingInvestigationReason", d),
        D::CrossTenantAccessDenied(d) => ("CrossTenantAccessDenied", d),
        // Governed manual adjustment rejected by the §4.6 governor (allow-list /
        // write-off guard). Decided BEFORE the post (the handler runs `govern`
        // out-of-txn), so it never actually rides the sentinel — listed for the
        // exhaustive match contract.
        D::ManualAdjustmentNotAllowed(d) => ("ManualAdjustmentNotAllowed", d),
        D::PeriodClosed(d) => ("PeriodClosed", d),
        D::AccountClosed(d) => ("AccountClosed", d),
        D::PayerClosed(d) => ("PayerClosed", d),
        D::AccountMappingMissing(d) => ("AccountMappingMissing", d),
        D::NegativeBalance(d) => ("NegativeBalance", d),
        D::SettlementReturnOverAllocated(d) => ("SettlementReturnOverAllocated", d),
        D::InvalidDisputeTransition(d) => ("InvalidDisputeTransition", d),
        D::ChargebackExceedsSettled(d) => ("ChargebackExceedsSettled", d),
        D::ChargebackOnRefunded(d) => ("ChargebackOnRefunded", d),
        D::ClockSkewQuarantine(d) => ("ClockSkewQuarantine", d),
        D::PeriodNotOpen(d) => ("PeriodNotOpen", d),
        D::PeriodCloseBlocked(d) => ("PeriodCloseBlocked", d),
        D::PeriodCloseInProgress(d) => ("PeriodCloseInProgress", d),
        D::IdempotencyConflict(d) => ("IdempotencyConflict", d),
        D::CurrencyScaleLocked(d) => ("CurrencyScaleLocked", d),
        D::OverRecognition(d) => ("OverRecognition", d),
        // Group E: a claw-back whose money-out decrement would underflow is raised
        // by the refund post sidecar and MUST round-trip unchanged (the handler
        // matches on it to DEFER the claw-back to the queue, not hard-fail).
        D::RefundClawbackDeferred(d) => ("RefundClawbackDeferred", d),
        // Cross-currency unsupported-op reject (Slice 5): guarded BEFORE the post
        // (claw-back in the refund handler, mapping-correction in the REST handler),
        // so it never rides the sentinel — listed for the exhaustive match contract.
        D::FxOperationUnsupported(d) => ("FxOperationUnsupported", d),
        // The dispute-hold gate runs OUT-OF-TXN in the refund handler BEFORE the
        // post (the open dispute is read out-of-txn), so it never actually rides the
        // sentinel; listed for the exhaustive match contract (Z5-2).
        D::RefundDisputeHeld(d) => ("RefundDisputeHeld", d),
        D::DualControlRequired(d) => ("DualControlRequired", d),
        D::SelfApprovalForbidden(d) => ("SelfApprovalForbidden", d),
        D::ApprovalNotActionable(d) => ("ApprovalNotActionable", d),
        D::DualControlPolicyOutOfRange(d) => ("DualControlPolicyOutOfRange", d),
        D::TamperVerificationFailed(d) => ("TamperVerificationFailed", d),
        D::PolicyVersionViolation(d) => ("PolicyVersionViolation", d),
        D::TenantPostingLocked(d) => ("TenantPostingLocked", d),
        D::PeriodNotFound(d) => ("PeriodNotFound", d),
        D::ApprovalNotFound(d) => ("ApprovalNotFound", d),
        D::PayerPiiNotFound(d) => ("PayerPiiNotFound", d),
        // Guarded in the credit/debit-note handlers BEFORE the post, so it never
        // actually rides the sentinel — listed for the exhaustive match contract.
        D::NoteInvoiceNotFound(d) => ("NoteInvoiceNotFound", d),
        // Likewise guarded in the refund handler BEFORE the post (the origin
        // settlement is resolved out-of-txn); listed for the exhaustive contract.
        D::RefundOriginNotFound(d) => ("RefundOriginNotFound", d),
        D::Internal(d) => ("Internal", d),
    }
}

/// Reconstruct a [`DomainError`] from a sentinel tag + detail; an unrecognised
/// tag degrades to [`DomainError::Internal`] (never silently dropped).
fn domain_from_parts(tag: &str, detail: String) -> DomainError {
    use DomainError as D;
    match tag {
        "ConcurrentModification" => D::ConcurrentModification(detail),
        "InvalidPostingIncrement" => D::InvalidPostingIncrement(detail),
        "Unbalanced" => D::Unbalanced(detail),
        "Empty" => D::Empty(detail),
        "MixedPayer" => D::MixedPayer(detail),
        "MissingPayer" => D::MissingPayer(detail),
        "MixedLegalEntity" => D::MixedLegalEntity(detail),
        "InconsistentScale" => D::InconsistentScale(detail),
        "AmountOutOfRange" => D::AmountOutOfRange(detail),
        "EntryTooLarge" => D::EntryTooLarge(detail),
        "InvalidRequest" => D::InvalidRequest(detail),
        "ScaleOutOfRange" => D::ScaleOutOfRange(detail),
        "CreditResidualUndisposed" => D::CreditResidualUndisposed(detail),
        "MoneyOutCapExceeded" => D::MoneyOutCapExceeded(detail),
        "AllocationTooLarge" => D::AllocationTooLarge(detail),
        "AllocationCurrencyMismatch" => D::AllocationCurrencyMismatch(detail),
        "CurrencyMismatch" => D::CurrencyMismatch(detail),
        "FxRateUnavailable" => D::FxRateUnavailable(detail),
        "FxRateStaleNotAllowed" => D::FxRateStaleNotAllowed(detail),
        "AllocationSplitInvalid" => D::AllocationSplitInvalid(detail),
        "GrantExceedsUnallocated" => D::GrantExceedsUnallocated(detail),
        "CreditExceedsOpenAr" => D::CreditExceedsOpenAr(detail),
        "CreditExceedsWallet" => D::CreditExceedsWallet(detail),
        "ScheduleTooLong" => D::ScheduleTooLong(detail),
        "SspSnapshotRequired" => D::SspSnapshotRequired(detail),
        "MissingPoAllocationGroup" => D::MissingPoAllocationGroup(detail),
        "RecognitionPolicyConflict" => D::RecognitionPolicyConflict(detail),
        "CreditNoteSplitAmbiguous" => D::CreditNoteSplitAmbiguous(detail),
        "CreditNoteExceedsHeadroom" => D::CreditNoteExceedsHeadroom(detail),
        "RefundExceedsSettled" => D::RefundExceedsSettled(detail),
        "RefundExceedsAllocated" => D::RefundExceedsAllocated(detail),
        "ModificationTreatmentReview" => D::ModificationTreatmentReview(detail),
        "RecognitionWithoutInvoiceLink" => D::RecognitionWithoutInvoiceLink(detail),
        "PiiInMetadataValue" => D::PiiInMetadataValue(detail),
        "MissingInvestigationReason" => D::MissingInvestigationReason(detail),
        "CrossTenantAccessDenied" => D::CrossTenantAccessDenied(detail),
        "PeriodClosed" => D::PeriodClosed(detail),
        "AccountClosed" => D::AccountClosed(detail),
        "PayerClosed" => D::PayerClosed(detail),
        "AccountMappingMissing" => D::AccountMappingMissing(detail),
        "NegativeBalance" => D::NegativeBalance(detail),
        "SettlementReturnOverAllocated" => D::SettlementReturnOverAllocated(detail),
        "InvalidDisputeTransition" => D::InvalidDisputeTransition(detail),
        "ChargebackExceedsSettled" => D::ChargebackExceedsSettled(detail),
        "ChargebackOnRefunded" => D::ChargebackOnRefunded(detail),
        "ClockSkewQuarantine" => D::ClockSkewQuarantine(detail),
        "PeriodNotOpen" => D::PeriodNotOpen(detail),
        "PeriodCloseBlocked" => D::PeriodCloseBlocked(detail),
        "PeriodCloseInProgress" => D::PeriodCloseInProgress(detail),
        "IdempotencyConflict" => D::IdempotencyConflict(detail),
        "CurrencyScaleLocked" => D::CurrencyScaleLocked(detail),
        "OverRecognition" => D::OverRecognition(detail),
        "RefundClawbackDeferred" => D::RefundClawbackDeferred(detail),
        "FxOperationUnsupported" => D::FxOperationUnsupported(detail),
        "RefundDisputeHeld" => D::RefundDisputeHeld(detail),
        "DualControlRequired" => D::DualControlRequired(detail),
        "SelfApprovalForbidden" => D::SelfApprovalForbidden(detail),
        "ApprovalNotActionable" => D::ApprovalNotActionable(detail),
        "DualControlPolicyOutOfRange" => D::DualControlPolicyOutOfRange(detail),
        "TamperVerificationFailed" => D::TamperVerificationFailed(detail),
        "PolicyVersionViolation" => D::PolicyVersionViolation(detail),
        "TenantPostingLocked" => D::TenantPostingLocked(detail),
        "PeriodNotFound" => D::PeriodNotFound(detail),
        "ApprovalNotFound" => D::ApprovalNotFound(detail),
        "PayerPiiNotFound" => D::PayerPiiNotFound(detail),
        "NoteInvoiceNotFound" => D::NoteInvoiceNotFound(detail),
        "RefundOriginNotFound" => D::RefundOriginNotFound(detail),
        "ManualAdjustmentNotAllowed" => D::ManualAdjustmentNotAllowed(detail),
        _ => D::Internal(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::RepoError;
    use bss_ledger_sdk::{LedgerError, MoneyError};
    use toolkit::api::canonical_prelude::CanonicalError;

    #[test]
    fn money_errors_survive_sentinel_and_sdk_mapping() {
        for (error, code) in [
            (MoneyError::ScaleMismatch, "CURRENCY_SCALE_MISMATCH"),
            (
                MoneyError::InvalidPostingIncrement,
                "INVALID_POSTING_INCREMENT",
            ),
            (MoneyError::CurrencyMismatch, "CURRENCY_MISMATCH"),
            (MoneyError::AmountOutOfRange, "AMOUNT_OUT_OF_RANGE"),
        ] {
            let decoded = decode_business_error(&repo_to_db(RepoError::Money(error)));
            let sdk = LedgerError::from(CanonicalError::from(decoded));
            assert!(
                matches!(sdk, LedgerError::InvalidArgument { code: actual, .. } if actual == code)
            );
        }
    }

    /// One of every repository error, with a detail that carries the envelope
    /// separator and contention words, so a transport that split or classified
    /// the text would disagree with the typed mapping.
    fn every_repo_error() -> Vec<RepoError> {
        let detail = || format!("database is locked{SENTINEL_SEP}SQLSTATE 40001");
        let errors = vec![
            RepoError::InvalidRequest(detail()),
            RepoError::Conflict(detail()),
            RepoError::Db(detail()),
            RepoError::Money(MoneyError::InvalidDecimal),
            RepoError::Money(MoneyError::InvalidCurrency),
            RepoError::Money(MoneyError::ScaleOutOfRange),
            RepoError::Money(MoneyError::AmountOutOfRange),
            RepoError::Money(MoneyError::InvalidPostingIncrement),
            RepoError::Money(MoneyError::CurrencyMismatch),
            RepoError::Money(MoneyError::ScaleMismatch),
            RepoError::InvalidStoredMoney(detail()),
            RepoError::ApprovalPolicyOutOfRange(detail()),
            RepoError::RowVanished(detail()),
            RepoError::ScaleOutOfRange(detail()),
            RepoError::CurrencyScaleLocked(detail()),
            RepoError::MoneyOutCapExceeded(detail()),
            RepoError::DisputeNotOpen(detail()),
            RepoError::RecognitionPolicyConflict(detail()),
        ];
        // A new variant fails to compile here until it joins the list above.
        for error in &errors {
            match error {
                RepoError::InvalidRequest(_)
                | RepoError::Conflict(_)
                | RepoError::Db(_)
                | RepoError::Money(_)
                | RepoError::InvalidStoredMoney(_)
                | RepoError::ApprovalPolicyOutOfRange(_)
                | RepoError::RowVanished(_)
                | RepoError::ScaleOutOfRange(_)
                | RepoError::CurrencyScaleLocked(_)
                | RepoError::MoneyOutCapExceeded(_)
                | RepoError::DisputeNotOpen(_)
                | RepoError::RecognitionPolicyConflict(_) => {}
            }
        }
        errors
    }

    #[test]
    fn typed_repo_mapping_equals_the_sentinel_transport_for_every_variant() {
        for (typed, transported) in every_repo_error().into_iter().zip(every_repo_error()) {
            let expected = repo_to_domain(typed);
            assert_eq!(decode_sentinel(&repo_to_db(transported)), Some(expected));
        }
    }

    #[test]
    fn typed_repo_mapping_keeps_categories_without_a_sentinel() {
        assert_eq!(
            repo_to_domain(RepoError::Conflict("stale".into())),
            DomainError::ConcurrentModification("stale".into())
        );
        assert_eq!(
            repo_to_domain(RepoError::DisputeNotOpen("WON".into())),
            DomainError::InvalidDisputeTransition("WON".into())
        );
        assert!(matches!(
            repo_to_domain(RepoError::InvalidStoredMoney("database is locked".into())),
            DomainError::Internal(_)
        ));
        assert!(matches!(
            repo_to_domain(RepoError::ScaleOutOfRange("EUR".into())),
            DomainError::AmountOutOfRange(_)
        ));
        assert!(matches!(
            repo_to_domain(RepoError::Db("connection reset".into())),
            DomainError::Internal(_)
        ));
        assert!(matches!(
            crate::infra::posting::retry::AttemptError::from(RepoError::Conflict("x".into())),
            crate::infra::posting::retry::AttemptError::Conflict
        ));
    }

    #[test]
    fn typed_conflict_survives_sentinel_and_sdk_mapping() {
        let error = DomainError::ConcurrentModification("stale version".to_owned());
        assert_eq!(decode_business_error(&business(error.clone())), error);
        let sdk = LedgerError::from(CanonicalError::from(error));
        assert!(
            matches!(sdk, LedgerError::Aborted { code, .. } if code == "CONCURRENT_MODIFICATION")
        );
    }

    #[test]
    fn invalid_dispute_transition_survives_repository_sentinel() {
        let error = repo_to_db(RepoError::DisputeNotOpen("already WON".into()));
        assert_eq!(
            decode_sentinel(&error),
            Some(DomainError::InvalidDisputeTransition("already WON".into()))
        );
    }

    #[test]
    fn invalid_new_request_preserves_business_sentinel_without_contention() {
        let detail = "database is locked; serialization failure".to_owned();
        let error = repo_to_db(RepoError::InvalidRequest(detail.clone()));
        assert_eq!(
            decode_sentinel(&error),
            Some(DomainError::InvalidRequest(detail))
        );
        assert!(matches!(
            crate::infra::posting::retry::AttemptError::from(error),
            crate::infra::posting::retry::AttemptError::Business(DomainError::InvalidRequest(_))
        ));
        let sdk = LedgerError::from(CanonicalError::from(DomainError::InvalidRequest(
            "invalid check".into(),
        )));
        assert!(matches!(sdk, LedgerError::InvalidArgument { .. }));
    }

    #[test]
    fn stored_corruption_is_internal_even_with_contention_words() {
        let error = repo_to_db(RepoError::InvalidStoredMoney(
            "database is locked".to_owned(),
        ));
        assert!(matches!(
            decode_sentinel(&error),
            Some(DomainError::Internal(_))
        ));
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    #[test]
    fn custom_infrastructure_diagnostic_is_not_a_business_sentinel() {
        assert!(decode_sentinel(&infra("database is locked")).is_none());
    }
    #[test]
    fn malformed_envelope_is_internal() {
        assert!(matches!(
            decode_sentinel(&DbError::Sea(DbErr::Custom(format!(
                "{SENTINEL_TAG}{SENTINEL_SEP}broken"
            )))),
            Some(DomainError::Internal(_))
        ));
    }
}

/// Preserve the real driver cause through in-transaction guard adapters.
pub(crate) fn scope_to_db(error: toolkit_db::secure::ScopeError) -> toolkit_db::DbError {
    match error {
        toolkit_db::secure::ScopeError::Db(error) => toolkit_db::DbError::Sea(error),
        other => infra(other.to_string()),
    }
}
