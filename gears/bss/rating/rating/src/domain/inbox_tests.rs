#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    InboundFact, InboxEntry, InboxKey, InboxSource, InboxState, InvalidInboxKey, ProducerOrigin,
    ProducerPolicy, Quarantine, QuarantineReason, ReceivedVia, ReleaseResolution, SchemaSupport,
};
use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;

fn key() -> InboxKey {
    InboxKey::new(Uuid::from_u128(7), InboxSource::Fact, "fact-1", 3).unwrap()
}

#[test]
fn every_source_round_trips_its_name() {
    for source in InboxSource::ALL {
        assert_eq!(InboxSource::parse(source.as_str()), Some(source));
    }
    assert_eq!(InboxSource::parse("usage"), None);
}

#[test]
fn only_a_quarantined_entry_is_released() {
    let id = Uuid::from_u128(1);
    assert_eq!(
        InboxState::Quarantined.release(id, ReleaseResolution::Apply),
        Ok(InboxState::Applied)
    );
    assert_eq!(
        InboxState::Quarantined.release(id, ReleaseResolution::Discard),
        Ok(InboxState::Discarded)
    );
    for state in [
        InboxState::Accepted,
        InboxState::Applied,
        InboxState::Discarded,
    ] {
        assert_eq!(
            state.release(id, ReleaseResolution::Apply),
            Err(DomainError::NotQuarantined { entry_id: id })
        );
    }
}

#[test]
fn one_arrival_names_one_entry() {
    let a = Sha256Hex::of_json(&json!({"v": 1}));
    let b = Sha256Hex::of_json(&json!({"v": 2}));
    assert_eq!(key().entry_id(&a), key().entry_id(&a));
    assert_ne!(key().entry_id(&a), key().entry_id(&b));
    let other_version = InboxKey::new(Uuid::from_u128(7), InboxSource::Fact, "fact-1", 4).unwrap();
    assert_ne!(key().entry_id(&a), other_version.entry_id(&a));
}

#[test]
fn the_entry_id_of_a_key_and_digest_is_pinned() {
    // The entry id is stored and is the `{id}` of the release route: a change to the namespace,
    // the separator or the field order would re-key every stored entry, so one vector is pinned.
    // Computed outside Rating: `shasum -a 256` of `{"v":1}`, and Python's `uuid.uuid5` over
    // `00000000-0000-0000-0000-000000000007|fact|fact-1|3|<digest>`.
    let digest = Sha256Hex::of_json(&json!({"v": 1}));
    assert_eq!(
        digest.as_str(),
        "afbf9d0f3560b0fd7795e81c42a0a79ee6b6fc67e064f77826aee642cad28d91"
    );
    assert_eq!(
        key().entry_id(&digest),
        Uuid::parse_str("95eec800-d8fb-52fe-ae5a-6f68d1aa6a8b").unwrap()
    );
}

#[test]
fn keys_are_validated_and_name_the_part_out_of_bounds() {
    let tenant = Uuid::from_u128(7);
    assert_eq!(
        InboxKey::new(tenant, InboxSource::Fact, "", 1),
        Err(InvalidInboxKey::BusinessId)
    );
    assert_eq!(
        InboxKey::new(tenant, InboxSource::Fact, "x".repeat(256), 1),
        Err(InvalidInboxKey::BusinessId)
    );
    assert_eq!(
        InboxKey::new(tenant, InboxSource::Fact, "f", -1),
        Err(InvalidInboxKey::Version)
    );
    assert!(InboxKey::new(tenant, InboxSource::Fact, "f", 0).is_ok());
    assert!(matches!(
        DomainError::from(InvalidInboxKey::Version),
        DomainError::InvalidArgument {
            field: "version",
            ..
        }
    ));
}

#[test]
fn a_pull_read_is_verified_by_construction() {
    let policy = ProducerPolicy::default();
    assert_eq!(
        policy.verify(InboxSource::Fact, &ProducerOrigin::PullRead),
        Ok(())
    );
}

#[test]
fn an_event_needs_its_sources_configured_producer() {
    let event = ProducerOrigin::Event {
        producer: "bss-subscriptions".to_owned(),
    };
    // Event intake is disabled by default: no producer is configured for any source.
    assert_eq!(
        ProducerPolicy::default().verify(InboxSource::Fact, &event),
        Err(DomainError::ProducerNotVerified {
            inbox_source: InboxSource::Fact
        })
    );
    let policy =
        ProducerPolicy::default().with_event_producer(InboxSource::Fact, "bss-subscriptions");
    assert_eq!(policy.verify(InboxSource::Fact, &event), Ok(()));
    assert!(policy.verify(InboxSource::Segment, &event).is_err());
    let impostor = ProducerOrigin::Event {
        producer: "someone-else".to_owned(),
    };
    assert!(policy.verify(InboxSource::Fact, &impostor).is_err());
}

#[test]
fn an_arriving_fact_is_accepted_or_quarantined() {
    let fact = InboundFact::new(
        key(),
        json!({"v": 1}),
        ProducerOrigin::PullRead,
        SchemaSupport::Supported,
    );
    assert_eq!(fact.digest(), &Sha256Hex::of_json(&json!({"v": 1})));
    let at = OffsetDateTime::UNIX_EPOCH;
    let accepted = InboxEntry::arriving(&fact, at, None);
    assert_eq!(accepted.state(), InboxState::Accepted);
    assert_eq!(accepted.received_via(), ReceivedVia::Pull);
    assert_eq!(accepted.entry_id(), fact.key.entry_id(fact.digest()));
    let primary = Uuid::from_u128(9);
    let quarantine = Quarantine::DigestConflict {
        conflict_of: primary,
    };
    let conflict = InboxEntry::arriving(&fact, at, Some(quarantine));
    assert_eq!(conflict.state(), InboxState::Quarantined);
    assert_eq!(conflict.quarantine(), Some(quarantine));
    assert_eq!(quarantine.conflict_of(), Some(primary));
    assert_eq!(quarantine.reason(), QuarantineReason::DigestConflict);
}

#[test]
fn a_quarantine_is_read_back_only_from_a_pair_it_produces() {
    let id = Uuid::from_u128(9);
    assert_eq!(
        Quarantine::from_parts(QuarantineReason::DigestConflict, Some(id)),
        Some(Quarantine::DigestConflict { conflict_of: id })
    );
    assert_eq!(
        Quarantine::from_parts(QuarantineReason::UnsupportedSchemaVersion, None),
        Some(Quarantine::UnsupportedSchemaVersion)
    );
    assert_eq!(
        Quarantine::from_parts(QuarantineReason::DigestConflict, None),
        None
    );
    assert_eq!(
        Quarantine::from_parts(QuarantineReason::UnsupportedSchemaVersion, Some(id)),
        None
    );
}

#[test]
fn every_quarantine_reason_round_trips_through_its_stored_name() {
    for reason in [
        QuarantineReason::DigestConflict,
        QuarantineReason::UnsupportedSchemaVersion,
    ] {
        assert_eq!(QuarantineReason::parse(reason.as_str()), Some(reason));
    }
    assert_eq!(QuarantineReason::parse("digest-conflict"), None);
}

#[test]
fn a_business_id_with_a_control_character_is_refused() {
    for bad in ["fact\u{0}1", "fact\n1", "\u{1b}[31m"] {
        assert!(
            InboxKey::new(Uuid::nil(), InboxSource::Fact, bad, 1).is_err(),
            "{bad:?}"
        );
    }
    assert!(
        InboxKey::new(
            Uuid::nil(),
            InboxSource::Fact,
            "fact-1:\u{fc}n\u{ef}code",
            1
        )
        .is_ok()
    );
}
