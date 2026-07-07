//! REST error mapping for the `bss-ledger` gear: translates authz-gate and
//! body-rejection errors into `CanonicalError`, which the canonical-error
//! middleware renders as an RFC 9457 `Problem`. Domain rejections reach the
//! wire through the single `From<DomainError> for CanonicalError` ladder
//! (`crate::infra::error_mapping`), not here.
//!
//! The `#[resource_error(...)]` GTS id below is cosmetic in P4 — it stamps
//! `context.resource_type` (the error renders regardless). It is reconciled
//! with the gear's GTS registration in P7.

use toolkit::api::canonical_prelude::{CanonicalError, resource_error};

use crate::authz::AuthzError;

/// Stamps `context.resource_type` on the canonical error. GTS id reconciled
/// with the gear's GTS registration in P7; cosmetic here.
#[resource_error("gts.cf.bss.ledger.ledger.v1~")]
struct LedgerResourceError;

/// Map an [`AuthzError`] from the PEP gate to a [`CanonicalError`].
/// `Denied` becomes a 403 carrying the deny reason; `Unavailable` becomes a
/// fail-closed 503 whose diagnostic stays server-side (the wire `Problem` is a
/// generic 503).
pub(crate) fn authz_error_to_canonical(err: AuthzError) -> CanonicalError {
    match err {
        AuthzError::Denied(reason) => LedgerResourceError::permission_denied()
            .with_reason(reason)
            .create(),
        AuthzError::Unavailable(detail) => {
            tracing::error!(detail, "Authorization service unavailable");
            CanonicalError::service_unavailable().create()
        }
    }
}

/// Build a 401 `CanonicalError` for requests without an authenticated
/// `SecurityContext` — distinct from a permission denial (403).
pub(crate) fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason("AUTHENTICATION_REQUIRED")
        .create()
}

/// Build a 400 `InvalidArgument` `CanonicalError` carrying a single `body`
/// field-violation for a malformed JSON request body. `code` is the
/// machine-readable reason (`json_syntax_error`, …); `message` is the
/// human-readable diagnostic.
pub(crate) fn json_rejection_canonical(code: &str, message: String) -> CanonicalError {
    LedgerResourceError::invalid_argument()
        .with_field_violation("body", message, code.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) journal entry. The scoped read yields `None` both when the entry
/// truly does not exist and when it lies outside the caller's authorized subtree
/// — the same 404 in either case (no existence leak).
pub(crate) fn entry_not_found(entry_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("journal entry {entry_id} not found"))
        .with_resource(entry_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) recognition schedule (the `GET /recognition-schedules/{id}`
/// miss). The scoped read yields `None` both when no such schedule exists for
/// `(tenant, schedule_id)` and when it lies outside the caller's authorized
/// subtree — the same 404 in either case (no existence leak). Mirrors
/// [`entry_not_found`]; uses the canonical `not_found` problem+json builder
/// (NOT the fiscal-period `PeriodNotFound` domain variant).
pub(crate) fn recognition_schedule_not_found(schedule_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("recognition schedule {schedule_id} not found"))
        .with_resource(schedule_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) audit-pack export. Same 404 whether the export truly does not
/// exist or lies outside the caller's scope (no existence leak).
pub(crate) fn pack_export_not_found(export_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("audit-pack export {export_id} not found"))
        .with_resource(export_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) invoice exposure (the `GET /invoices/{invoice_id}/exposure` miss).
/// The scoped read yields `None` both when no credit/debit note has ever touched
/// the invoice (so the `invoice_exposure` row was never seeded) and when it lies
/// outside the caller's authorized subtree — the same 404 in either case (no
/// existence leak). Mirrors [`recognition_schedule_not_found`].
pub(crate) fn invoice_exposure_not_found(invoice_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("invoice exposure for {invoice_id} not found"))
        .with_resource(invoice_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) refund (the `GET /refunds/{refund_id}` miss, Group G). The scoped
/// read yields `None` both when no refund with that id exists for `(tenant,
/// refund_id)` and when it lies outside the caller's authorized subtree — the same
/// 404 in either case (no existence leak). Mirrors [`invoice_exposure_not_found`].
pub(crate) fn refund_not_found(refund_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("refund {refund_id} not found"))
        .with_resource(refund_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) credit note (the `GET /credit-notes/{credit_note_id}` miss, read
/// surface R2). The scoped read yields `None` both when no credit note with that
/// id exists for `(tenant, credit_note_id)` and when it lies outside the caller's
/// authorized subtree — the same 404 in either case (no existence leak). Mirrors
/// [`refund_not_found`].
pub(crate) fn credit_note_not_found(credit_note_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("credit note {credit_note_id} not found"))
        .with_resource(credit_note_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) debit note (the `GET /debit-notes/{debit_note_id}` miss, read
/// surface R2). The scoped read yields `None` both when no debit note with that
/// id exists for `(tenant, debit_note_id)` and when it lies outside the caller's
/// authorized subtree — the same 404 in either case (no existence leak). Mirrors
/// [`credit_note_not_found`].
pub(crate) fn debit_note_not_found(debit_note_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("debit note {debit_note_id} not found"))
        .with_resource(debit_note_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) dispute (the `GET /disputes/{dispute_id}` miss, read surface R3).
/// The scoped read yields `None` both when no dispute with that id was ever opened
/// for `(tenant, dispute_id)` and when it lies outside the caller's authorized
/// subtree — the same 404 in either case (no existence leak). Mirrors
/// [`refund_not_found`].
pub(crate) fn dispute_not_found(dispute_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("dispute {dispute_id} not found"))
        .with_resource(dispute_id.to_owned())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) recognition run (the `GET /recognition-runs/{run_id}` miss, read
/// surface R4). The scoped read yields `None` both when no run with that id exists
/// for the tenant and when it lies outside the caller's authorized subtree — the
/// same 404 in either case (no existence leak). Takes the `run_id` `Uuid` (the
/// surrogate run id) and formats it. Mirrors [`refund_not_found`] /
/// [`entry_not_found`].
pub(crate) fn recognition_run_not_found(run_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("recognition run {run_id} not found"))
        .with_resource(run_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) payment settlement (the `GET /payments/{payment_id}/settlement`
/// miss, read surface R4). The scoped read yields `None` both when the payment was
/// never settled (so no `payment_settlement` row exists for `(tenant, payment_id)`)
/// and when it lies outside the caller's authorized subtree — the same 404 in
/// either case (no existence leak). Mirrors [`refund_not_found`].
pub(crate) fn settlement_not_found(payment_id: &str) -> CanonicalError {
    LedgerResourceError::not_found(format!("settlement for payment {payment_id} not found"))
        .with_resource(payment_id.to_owned())
        .create()
}

/// Build a 404 for an unknown (or scoped-out) payer lifecycle state (the
/// `GET /payers/{payer_tenant_id}/state` miss). Tenant-scoped (SQL-level BOLA): a
/// payer with no recorded state — or outside the caller's subtree — is the same
/// 404 (no existence leak). Mirrors [`refund_not_found`].
pub(crate) fn payer_state_not_found(payer_tenant_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("payer state for {payer_tenant_id} not found"))
        .with_resource(payer_tenant_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) FX rate snapshot (the `GET /fx/rate-snapshots/{rateId}` miss,
/// Slice 5). The scoped read yields `None` both when no snapshot with that id
/// exists for `(tenant, rate_id)` and when it lies outside the caller's authorized
/// subtree — the same 404 in either case (no existence leak). Mirrors
/// [`refund_not_found`].
pub(crate) fn rate_snapshot_not_found(rate_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("fx rate snapshot {rate_id} not found"))
        .with_resource(rate_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) reconciliation run (the `GET /reconciliation-runs/{run_id}` miss,
/// Slice 7 Phase 3). The scoped read yields `None` both when no run with that id
/// exists for the tenant and when it lies outside the caller's authorized subtree
/// — the same 404 in either case (no existence leak). Mirrors
/// [`recognition_run_not_found`] / [`exception_not_found`].
pub(crate) fn reconciliation_run_not_found(run_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("reconciliation run {run_id} not found"))
        .with_resource(run_id.to_string())
        .create()
}

/// Build a 404 `NotFound` `CanonicalError` for an absent (or foreign-owned,
/// scoped-out) exception-queue row (the `POST /exceptions/{id}/resolution` miss,
/// Slice 7 Phase 2). The scoped read yields `None` both when no exception with that
/// id exists for the tenant and when it lies outside the caller's authorized
/// subtree — the same 404 in either case (no existence leak). Mirrors
/// [`refund_not_found`].
pub(crate) fn exception_not_found(exception_id: uuid::Uuid) -> CanonicalError {
    LedgerResourceError::not_found(format!("exception {exception_id} not found"))
        .with_resource(exception_id.to_string())
        .create()
}

/// Map a [`ReversalError`] from the reversal/mapping-correction handlers to a
/// [`CanonicalError`] — both variants are client errors ⇒ a 400
/// `InvalidArgument`. Kept here (not in the `DomainError` ladder) because
/// `ReversalError` is a distinct pure-domain error, raised before any
/// `DomainError` is produced.
pub(crate) fn reversal_error_to_canonical(
    err: crate::domain::invoice::reversal::ReversalError,
) -> CanonicalError {
    use crate::domain::invoice::reversal::ReversalError;
    match err {
        ReversalError::CannotReverseReversal => LedgerResourceError::invalid_argument()
            .with_field_violation(
                "entry_id",
                "cannot reverse an entry that is itself a reversal",
                "CANNOT_REVERSE_REVERSAL",
            )
            .create(),
        ReversalError::CreditGrantNotReconstructible => LedgerResourceError::invalid_argument()
            .with_field_violation(
                "entry_id",
                "cannot reverse an entry with a REUSABLE_CREDIT line",
                "CANNOT_REVERSE_CREDIT_GRANT",
            )
            .create(),
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
