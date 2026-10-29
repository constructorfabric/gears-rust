#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use uuid::Uuid;

use super::{PageEntry, TARGET_UNKNOWN, exception_key, same_digest};
use crate::domain::exception::SubjectKind;
use crate::domain::usage::entry::{CURRENT_DIGEST_VERSION, DigestVersion, EntryIdentity};
use crate::domain::usage::normalize::RejectReason;
use crate::domain::usage::{StoredDigest, UsageException, entry};

fn stored(raw: &Value, version: Option<DigestVersion>) -> StoredDigest {
    StoredDigest {
        identity: EntryIdentity {
            tenant_id: Uuid::from_u128(1),
            usage_record_id: Uuid::from_u128(2),
        },
        raw_sha256: entry::digest(raw, CURRENT_DIGEST_VERSION),
        raw_digest_version: version,
    }
}

#[test]
fn a_reread_is_compared_under_the_stored_digest_version() {
    let raw = json!({"id": "a", "quantity": "8"});
    assert!(same_digest(
        &stored(&raw, Some(CURRENT_DIGEST_VERSION)),
        &raw
    ));
    assert!(!same_digest(
        &stored(&raw, Some(CURRENT_DIGEST_VERSION)),
        &json!({"id": "a", "quantity": "9"})
    ));
    // A version this build does not know cannot be compared: a conflict, never a silent duplicate.
    assert!(!same_digest(&stored(&raw, None), &raw));
}

#[test]
fn an_exception_is_keyed_on_the_record() {
    let identity = EntryIdentity {
        tenant_id: Uuid::from_u128(1),
        usage_record_id: Uuid::from_u128(2),
    };
    let key = exception_key(identity, UsageException::TargetUnknown);
    assert_eq!(key.tenant_id(), identity.tenant_id);
    assert_eq!(key.subject_kind(), SubjectKind::UsageRecord);
    assert_eq!(key.subject_ref(), identity.usage_record_id.to_string());
    assert_eq!(key.reason_code(), TARGET_UNKNOWN);
}

#[test]
fn every_usage_exception_reason_is_a_valid_reason_code() {
    let mut exceptions = vec![
        UsageException::ContentConflict,
        UsageException::TargetUnknown,
    ];
    exceptions.extend(RejectReason::ALL.map(UsageException::Rejected));
    for exception in exceptions {
        let parsed = crate::domain::exception::ExceptionReason::parse(exception.code()).unwrap();
        assert_eq!(parsed, exception.reason(), "{exception:?}");
    }
}

#[test]
fn an_identified_page_entry_hands_out_its_identity_and_raw_entry() {
    let identity = EntryIdentity {
        tenant_id: Uuid::from_u128(1),
        usage_record_id: Uuid::from_u128(2),
    };
    let raw = json!({"id": "x"});
    let entry = PageEntry::Identified {
        identity,
        raw: raw.clone(),
    };
    assert_eq!(entry.identified(), Some((identity, &raw)));
    assert_eq!(PageEntry::Unidentifiable(raw.clone()).identified(), None);
    assert_eq!(PageEntry::OutsideSubscription(raw).identified(), None);
}
