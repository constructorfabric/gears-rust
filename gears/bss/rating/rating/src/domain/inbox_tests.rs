#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;

use super::*;

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
fn keys_are_validated() {
    let tenant = Uuid::from_u128(7);
    assert!(InboxKey::new(tenant, InboxSource::Fact, "", 1).is_err());
    assert!(InboxKey::new(tenant, InboxSource::Fact, "x".repeat(256), 1).is_err());
    assert!(InboxKey::new(tenant, InboxSource::Fact, "f", -1).is_err());
    assert!(InboxKey::new(tenant, InboxSource::Fact, "f", 0).is_ok());
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
    let fact = InboundFact::new(key(), json!({"v": 1}), ProducerOrigin::PullRead, true);
    assert_eq!(fact.digest(), &Sha256Hex::of_json(&json!({"v": 1})));
    let at = OffsetDateTime::UNIX_EPOCH;
    let accepted = InboxEntry::arriving(&fact, at, None);
    assert_eq!(accepted.state, InboxState::Accepted);
    assert_eq!(accepted.received_via, ReceivedVia::Pull);
    assert_eq!(accepted.entry_id, fact.key.entry_id(fact.digest()));
    let primary = Uuid::from_u128(9);
    let quarantine = Quarantine::DigestConflict {
        conflict_of: primary,
    };
    let conflict = InboxEntry::arriving(&fact, at, Some(quarantine));
    assert_eq!(conflict.state, InboxState::Quarantined);
    assert_eq!(conflict.quarantine, Some(quarantine));
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
