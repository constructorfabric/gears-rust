// Created: 2026-09-15 by Constructor Tech
//! What a batch change may name in place of a secret, and who may claim it.

use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::{Claim, PendingSecret, check_claim, pending_id_of};
use crate::domain::error::DomainError;
use crate::field;

fn row(now: OffsetDateTime) -> PendingSecret {
    PendingSecret {
        id: Uuid::new_v4(),
        declaration_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4().to_string(),
        secret_ref: "cf-settings-x-y-z".to_owned(),
        created_at: now,
        expires_at: now + Duration::minutes(10),
    }
}

fn claim_of(row: &PendingSecret, now: OffsetDateTime) -> Claim<'_> {
    Claim {
        declaration_id: row.declaration_id,
        tenant_id: row.tenant_id,
        subject_id: &row.subject_id,
        now,
    }
}

fn is_invalid(err: &DomainError) -> bool {
    matches!(
        err,
        DomainError::Validation { code, field, .. }
            if *code == field::PENDING_SECRET_INVALID && field == "value.pending_id"
    )
}

#[test]
fn only_an_object_with_the_one_member_and_a_uuid_in_it_is_a_pending_id() {
    let id = Uuid::new_v4();
    assert_eq!(
        pending_id_of(&json!({ "pending_id": id.to_string() })),
        Some(id)
    );
    // Anything else is a value: a string, an object with more members, a
    // member that is not a UUID, or the member under another name.
    assert_eq!(pending_id_of(&json!(id.to_string())), None);
    assert_eq!(
        pending_id_of(&json!({ "pending_id": id.to_string(), "extra": 1 })),
        None
    );
    assert_eq!(pending_id_of(&json!({ "pending_id": "not-a-uuid" })), None);
    assert_eq!(pending_id_of(&json!({ "pendingId": id.to_string() })), None);
    assert_eq!(pending_id_of(&json!(null)), None);
}

#[test]
fn the_staging_subject_claims_its_own_row_for_the_same_pair_before_expiry() {
    let now = OffsetDateTime::now_utc();
    let row = row(now);
    assert!(check_claim(&row, &claim_of(&row, now)).is_ok());
    assert!(
        check_claim(&row, &claim_of(&row, now + Duration::minutes(9))).is_ok(),
        "still inside the window"
    );
}

#[test]
fn every_mismatch_is_the_same_invalid_refusal() {
    let now = OffsetDateTime::now_utc();
    let row = row(now);
    let other_subject = Uuid::new_v4().to_string();

    let wrong_declaration = Claim {
        declaration_id: Uuid::new_v4(),
        ..claim_of(&row, now)
    };
    let wrong_tenant = Claim {
        tenant_id: Uuid::new_v4(),
        ..claim_of(&row, now)
    };
    let wrong_subject = Claim {
        subject_id: &other_subject,
        ..claim_of(&row, now)
    };
    let expired = claim_of(&row, row.expires_at);

    let mut messages = Vec::new();
    for (name, claim) in [
        ("declaration", wrong_declaration),
        ("tenant", wrong_tenant),
        ("subject", wrong_subject),
        ("expiry", expired),
    ] {
        let err = check_claim(&row, &claim).expect_err(name);
        assert!(is_invalid(&err), "{name}: {err:?}");
        messages.push(err.to_string());
    }
    // Byte-identical: the answer does not say which condition failed.
    messages.dedup();
    assert_eq!(messages.len(), 1, "{messages:?}");
}
