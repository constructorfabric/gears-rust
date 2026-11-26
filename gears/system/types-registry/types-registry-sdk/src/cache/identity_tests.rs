use gts::GtsId;

use super::Identities;
use crate::models::{EntityIdentity, EntityKey};

const TAIL: &str = "7a1d2f34-5678-49ab-9012-abcdef123456";

fn id(raw: &str) -> GtsId {
    GtsId::try_new(raw).expect("valid GTS identifier")
}

fn learned(bound: usize, ids: &[&GtsId]) -> Identities {
    let mut identities = Identities::new(bound);
    for id in ids {
        identities.learn(id, id.to_uuid());
    }
    identities
}

#[test]
fn a_learned_identity_answers_its_identifier_and_its_registry_reference() {
    let thing = id("gts.cf.test.identity.thing.v1~");
    let issued = EntityIdentity {
        gts_id: thing.clone(),
        gts_uuid: uuid::Uuid::from_u128(137),
    };
    let mut identities = Identities::new(1);
    identities.learn(&thing, issued.gts_uuid);

    assert_eq!(
        identities.get(&EntityKey::GtsId(thing.clone())),
        Some(issued.clone())
    );
    assert_eq!(
        identities.get(&EntityKey::GtsUuid(issued.gts_uuid)),
        Some(issued.clone())
    );
    assert_eq!(
        identities.get(&EntityKey::GtsId(id("gts.cf.test.identity.other.v1~"))),
        None
    );
    let other = id("gts.cf.test.identity.other.v1~");
    identities.learn(&other, uuid::Uuid::from_u128(138));
    assert_eq!(identities.get(&EntityKey::GtsId(thing)), None);
    assert_eq!(identities.get(&EntityKey::GtsUuid(issued.gts_uuid)), None);
}

/// UUID text inside an identifier cannot establish a mapping. Even two identifiers
/// with the same tail can be distinguished by their server-issued references.
#[test]
fn identical_uuid_tails_do_not_alias_two_registry_issued_pairs() {
    let registered = id(&format!("gts.cf.test.identity.thing.v1~{TAIL}"));
    let other = id(&format!("gts.cf.test.identity.other.v1~{TAIL}"));
    let first = uuid::Uuid::from_u128(31);
    let second = uuid::Uuid::from_u128(32);
    let mut identities = Identities::new(2);
    identities.learn(&registered, first);

    assert_eq!(identities.get(&EntityKey::GtsId(other.clone())), None);
    assert!(!identities.agrees(&other, first));
    assert!(!identities.agrees(&registered, second));
    identities.learn(&other, second);
    for (id, uuid) in [(registered, first), (other, second)] {
        let pair = EntityIdentity {
            gts_id: id.clone(),
            gts_uuid: uuid,
        };
        assert_eq!(identities.get(&EntityKey::GtsId(id)), Some(pair.clone()));
        assert_eq!(identities.get(&EntityKey::GtsUuid(uuid)), Some(pair));
    }
}

#[test]
fn the_bound_counts_entries_and_evicts_the_least_recently_used() {
    let first = id("gts.cf.test.identity.first.v1~");
    let second = id("gts.cf.test.identity.second.v1~");
    let third = id("gts.cf.test.identity.third.v1~");
    let mut identities = learned(2, &[&first, &second]);

    assert!(identities.get(&EntityKey::GtsId(first.clone())).is_some());
    identities.learn(&third, third.to_uuid());

    assert!(identities.get(&EntityKey::GtsId(first)).is_some());
    assert_eq!(identities.get(&EntityKey::GtsId(second)), None);
    assert!(identities.get(&EntityKey::GtsId(third)).is_some());
}

#[test]
fn a_zero_bound_keeps_nothing() {
    let thing = id("gts.cf.test.identity.thing.v1~");
    let mut identities = learned(0, &[&thing]);

    assert_eq!(identities.get(&EntityKey::GtsId(thing)), None);
}

/// Another identifier sharing a learned Registry Reference misses without making the
/// learned identity recently used, so it is evicted as if never asked.
#[test]
fn a_miss_by_another_spelling_leaves_the_learned_identity_s_recency() {
    let first = id(&format!("gts.cf.test.identity.first.v1~{TAIL}"));
    let alias = id(&format!("gts.cf.test.identity.alias.v1~{TAIL}"));
    let second = id("gts.cf.test.identity.second.v1~");
    let third = id("gts.cf.test.identity.third.v1~");
    let mut identities = learned(2, &[&first, &second]);

    assert_eq!(identities.get(&EntityKey::GtsId(alias)), None);
    identities.learn(&third, third.to_uuid());

    assert_eq!(identities.get(&EntityKey::GtsId(first)), None, "evicted");
    assert!(identities.get(&EntityKey::GtsId(second)).is_some());
    assert!(identities.get(&EntityKey::GtsId(third)).is_some());
}
