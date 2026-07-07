//! Unit checks for the REST error constructors: each builds a `CanonicalError`
//! of the right HTTP category, and the not-found family stamps the resource id
//! into the rendered `Problem` (so the wire 404 names what was missing without
//! leaking existence — same 404 for absent vs scoped-out).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use toolkit::api::canonical_prelude::{CanonicalError, Problem};
use uuid::Uuid;

use super::{
    authz_error_to_canonical, credit_note_not_found, debit_note_not_found, dispute_not_found,
    entry_not_found, invoice_exposure_not_found, json_rejection_canonical, pack_export_not_found,
    payer_state_not_found, rate_snapshot_not_found, recognition_run_not_found,
    recognition_schedule_not_found, refund_not_found, reversal_error_to_canonical,
    settlement_not_found, unauthenticated,
};
use crate::authz::AuthzError;
use crate::domain::invoice::reversal::ReversalError;

/// The rendered RFC 9457 `Problem` body as a string (for wire-code assertions).
fn body(err: CanonicalError) -> String {
    serde_json::to_string(&Problem::from(err)).unwrap()
}

#[test]
fn authz_denied_is_403_carrying_the_deny_reason() {
    let err = authz_error_to_canonical(AuthzError::Denied("LEDGER_PROVISION_DENIED".to_owned()));
    assert_eq!(err.status_code(), 403, "a PEP deny must be a 403");
    assert!(
        body(err).contains("LEDGER_PROVISION_DENIED"),
        "403 must carry the deny reason"
    );
}

#[test]
fn authz_unavailable_fails_closed_to_503() {
    let err = authz_error_to_canonical(AuthzError::Unavailable("pdp rpc timeout".to_owned()));
    assert_eq!(
        err.status_code(),
        503,
        "an unreachable PDP must fail closed to 503 (diagnostic stays server-side)"
    );
}

#[test]
fn unauthenticated_is_401() {
    let err = unauthenticated();
    assert_eq!(err.status_code(), 401);
    assert!(body(err).contains("AUTHENTICATION_REQUIRED"));
}

#[test]
fn json_rejection_is_400_carrying_the_machine_code() {
    let err = json_rejection_canonical("json_syntax_error", "expected `,` at line 1".to_owned());
    assert_eq!(err.status_code(), 400);
    assert!(
        body(err).contains("json_syntax_error"),
        "the field-violation must carry the machine code"
    );
}

#[test]
fn not_found_family_is_404_and_stamps_the_resource_id() {
    let entry = Uuid::now_v7();
    assert_eq!(entry_not_found(entry).status_code(), 404);
    assert!(body(entry_not_found(entry)).contains(&entry.to_string()));

    assert_eq!(recognition_schedule_not_found("SCH-1").status_code(), 404);
    assert!(body(recognition_schedule_not_found("SCH-1")).contains("SCH-1"));

    let export = Uuid::now_v7();
    assert_eq!(pack_export_not_found(export).status_code(), 404);
    assert!(body(pack_export_not_found(export)).contains(&export.to_string()));

    assert_eq!(invoice_exposure_not_found("INV-1").status_code(), 404);
    assert!(body(invoice_exposure_not_found("INV-1")).contains("INV-1"));

    assert_eq!(refund_not_found("REF-1").status_code(), 404);
    assert!(body(refund_not_found("REF-1")).contains("REF-1"));

    assert_eq!(credit_note_not_found("CN-1").status_code(), 404);
    assert_eq!(debit_note_not_found("DN-1").status_code(), 404);
    assert_eq!(dispute_not_found("DSP-1").status_code(), 404);

    let run = Uuid::now_v7();
    assert_eq!(recognition_run_not_found(run).status_code(), 404);
    assert!(body(recognition_run_not_found(run)).contains(&run.to_string()));

    assert_eq!(settlement_not_found("PAY-1").status_code(), 404);
    assert!(body(settlement_not_found("PAY-1")).contains("PAY-1"));

    let payer = Uuid::now_v7();
    assert_eq!(payer_state_not_found(payer).status_code(), 404);

    let rate = Uuid::now_v7();
    assert_eq!(rate_snapshot_not_found(rate).status_code(), 404);
    assert!(body(rate_snapshot_not_found(rate)).contains(&rate.to_string()));
}

#[test]
fn reversal_errors_are_400_with_their_wire_codes() {
    let cannot = reversal_error_to_canonical(ReversalError::CannotReverseReversal);
    assert_eq!(cannot.status_code(), 400);
    assert!(body(cannot).contains("CANNOT_REVERSE_REVERSAL"));

    let credit = reversal_error_to_canonical(ReversalError::CreditGrantNotReconstructible);
    assert_eq!(credit.status_code(), 400);
    assert!(body(credit).contains("CANNOT_REVERSE_CREDIT_GRANT"));
}
