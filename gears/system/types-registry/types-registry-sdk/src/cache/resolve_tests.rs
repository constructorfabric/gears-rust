//! Platform identity resolution uses a separate positive LRU without expiry.
//! Misses reach batch resolution without reading snapshots.

use gts::{GtsId, GtsInstanceId, GtsTypeId};
use serde_json::json;

use super::*;
use crate::PlatformTypesRegistryApiExt;
use crate::models::MAX_BATCH_GET_KEYS;

const UNREGISTERED: &str = "gts.cf.cache.client.absent.v1~";
const TAIL: &str = "7a1d2f34-5678-49ab-9012-abcdef123456";

fn type_id(id: &str) -> GtsTypeId {
    GtsTypeId::try_new(id).expect("a Type Schema identifier")
}

fn instance_id(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("an Instance identifier")
}

fn uuid(id: &str) -> Uuid {
    GtsId::try_new(id).expect("identifier").to_uuid()
}

/// The keys of batch resolution `n`, in the order sent.
fn resolved(f: &Fixture, n: usize) -> Vec<EntityKey> {
    f.recorder.resolutions()[n].clone()
}

fn fixture_bound(window: Duration, max_identity_entries: usize) -> Fixture {
    fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            freshness_window: window,
            max_identity_entries,
            ..CacheSettings::default()
        },
    )
}

/// A zero window revalidates every snapshot read; a known identity needs no read at all,
/// and resolves nothing under the other kind.
#[tokio::test(start_paused = true)]
async fn a_known_identity_resolves_without_a_read_whatever_the_window() {
    let f = fixture(Duration::ZERO);
    let uuid_of = f
        .cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("registered");
    assert_eq!(uuid_of, uuid(TYPE));
    assert_eq!(resolved(&f, 0), [gts_key(TYPE)]);
    assert_eq!(f.recorder.batch_count(), 0, "no snapshot is read");

    assert_eq!(
        f.cache
            .resolve_type_id(&ctx(), uuid(TYPE))
            .await
            .expect("known"),
        type_id(TYPE)
    );
    assert_eq!(
        f.cache
            .resolve_type_uuids(&ctx(), &[type_id(TYPE)])
            .await
            .expect("known")[&type_id(TYPE)],
        Some(uuid(TYPE))
    );
    assert_eq!(
        f.cache
            .resolve_type_ids(&ctx(), &[uuid(TYPE)])
            .await
            .expect("known")[&uuid(TYPE)],
        Some(type_id(TYPE))
    );
    assert!(matches!(
        f.cache.resolve_instance_id(&ctx(), uuid(TYPE)).await,
        Err(CanonicalError::NotFound { .. })
    ));
    assert_eq!(
        f.cache
            .resolve_instance_ids(&ctx(), &[uuid(TYPE)])
            .await
            .expect("known")[&uuid(TYPE)],
        None
    );
    tokio::time::advance(WINDOW * 10).await;
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("known");
    assert_eq!(
        f.recorder.resolution_count(),
        1,
        "every later resolution was known"
    );
}

#[tokio::test(start_paused = true)]
async fn an_ordinary_read_teaches_the_identity_under_either_key_form() {
    let f = fixture(Duration::ZERO);
    f.title(uuid_key(OTHER), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 1);

    assert_eq!(
        f.cache
            .resolve_type_uuid(&ctx(), &type_id(OTHER))
            .await
            .expect("known"),
        uuid(OTHER)
    );
    assert_eq!(
        f.cache
            .resolve_type_id(&ctx(), uuid(OTHER))
            .await
            .expect("known"),
        type_id(OTHER)
    );
    assert_eq!(f.recorder.batch_count(), 1);
    assert_eq!(f.recorder.resolution_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_mixed_batch_resolves_only_the_unknown_keys() {
    let f = fixture(Duration::ZERO);
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("registered");

    let resolved = f
        .cache
        .resolve_type_uuids(
            &ctx(),
            &[
                type_id(TYPE),
                type_id(OTHER),
                type_id(UNREGISTERED),
                type_id(TYPE),
            ],
        )
        .await
        .expect("read");

    assert_eq!(resolved[&type_id(TYPE)], Some(uuid(TYPE)));
    assert_eq!(resolved[&type_id(OTHER)], Some(uuid(OTHER)));
    assert_eq!(resolved[&type_id(UNREGISTERED)], None);
    assert_eq!(f.recorder.resolution_count(), 2);
    assert_eq!(
        self::resolved(&f, 1),
        [gts_key(OTHER), gts_key(UNREGISTERED)]
    );
    let identities = f
        .cache
        .batch_resolve_identities(&ctx(), vec![gts_key(OTHER), uuid_key(TYPE)])
        .await
        .expect("known");
    assert_eq!(identities.len(), 2);
    assert_eq!(f.recorder.resolution_count(), 2, "both learned or known");
    assert_eq!(f.recorder.batch_count(), 0);
}

/// Without an identity LRU, a fresh snapshot resolves either key.
/// An expired one sends the key to batch resolution without refreshing the snapshot.
#[tokio::test(start_paused = true)]
async fn with_no_identity_store_only_a_fresh_resident_snapshot_answers() {
    let f = fixture_bound(WINDOW, 0);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.cache
            .resolve_type_uuid(&ctx(), &type_id(TYPE))
            .await
            .expect("resident"),
        uuid(TYPE)
    );
    assert_eq!(
        f.cache
            .resolve_type_id(&ctx(), uuid(TYPE))
            .await
            .expect("resident"),
        type_id(TYPE)
    );
    assert_eq!(f.recorder.resolution_count(), 0);

    tokio::time::advance(WINDOW).await;
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("resolved");
    assert_eq!(
        resolved(&f, 0),
        [gts_key(TYPE)],
        "an expired entry answers nothing"
    );
    assert_eq!(f.recorder.batch_count(), 1, "nothing was revalidated");
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("resolved");
    assert_eq!(f.recorder.resolution_count(), 2, "nothing was learned");
}

#[tokio::test(start_paused = true)]
async fn absence_is_never_kept() {
    let f = fixture(WINDOW);
    for _ in 0..2 {
        assert!(matches!(
            f.cache
                .resolve_type_uuid(&ctx(), &type_id(UNREGISTERED))
                .await,
            Err(CanonicalError::NotFound { .. })
        ));
    }
    assert_eq!(f.recorder.resolution_count(), 2);

    f.registry.seed(UNREGISTERED, schema(UNREGISTERED, "v1"));
    assert_eq!(
        f.cache
            .resolve_type_uuid(&ctx(), &type_id(UNREGISTERED))
            .await
            .expect("registered since"),
        uuid(UNREGISTERED)
    );
}

/// A revision and a deletion change no identity: both stay known, and a deleted entity
/// still resolves.
#[tokio::test(start_paused = true)]
async fn an_observed_registration_or_deletion_leaves_a_known_identity_alone() {
    let f = fixture(WINDOW);
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("registered");

    let registered = f.register(registration(TYPE, "v2", Some(1))).await;
    assert!(matches!(
        registered.items[0].outcome,
        RegistrationOutcome::Succeeded { .. }
    ));
    let deleted = f.delete(deletion(&[(TYPE, 2)], false)).await;
    assert!(matches!(
        deleted.items[0].outcome,
        crate::models::DeletionOutcome::Succeeded { .. }
    ));
    let before = f.recorder.batch_count();
    assert_eq!(f.recorder.resolution_count(), 1);

    assert_eq!(
        f.cache
            .resolve_type_id(&ctx(), uuid(TYPE))
            .await
            .expect("known"),
        type_id(TYPE)
    );
    assert_eq!(f.recorder.resolution_count(), 1);
    assert_eq!(f.recorder.batch_count(), before);
    assert!(
        f.lookup(gts_key(TYPE), full(), false).await != EntityLookup::NotFound,
        "the snapshot entry was invalidated and is read anew"
    );
    assert_eq!(f.recorder.batch_count(), before + 1);
}

/// An identity hit leaves snapshot recency unchanged, so the next fill evicts
/// the least-recently-used snapshot.
#[tokio::test(start_paused = true)]
async fn a_known_identity_does_not_extend_its_snapshot_s_recency() {
    let registry = MockTypesRegistry::new().completing_after(0);
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
    }
    let f = fixture_with(
        registry,
        CacheSettings {
            freshness_window: WINDOW,
            max_entries: 2,
            ..CacheSettings::default()
        },
    );
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(OTHER), full(), false).await;

    f.cache
        .resolve_type_id(&ctx(), uuid(TYPE))
        .await
        .expect("known");
    f.title(gts_key(THIRD), full(), false).await;
    assert_eq!(f.recorder.batch_count(), 3);

    f.title(gts_key(OTHER), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        3,
        "the more recent snapshot stayed"
    );
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        4,
        "the known identity's snapshot was evicted"
    );
}

/// An unregistered ID can share a registered entity's UUID tail without a hash
/// collision. The alias still misses and leaves both cached records intact.
#[tokio::test(start_paused = true)]
async fn an_identifier_sharing_a_resident_registry_reference_is_read_and_stays_absent() {
    let f = fixture(WINDOW);
    let registered = format!("{TYPE}{TAIL}");
    let alias = format!("{OTHER}{TAIL}");
    assert_eq!(uuid(&registered), uuid(&alias));
    f.registry
        .seed(&registered, json!({ "title": "registered" }));
    assert!(matches!(
        f.lookup(gts_key(&registered), full(), false).await,
        EntityLookup::Found { .. }
    ));
    f.cache
        .resolve_instance_uuid(&ctx(), &instance_id(&registered))
        .await
        .expect("known");
    assert_eq!(f.recorder.batch_count(), 1);
    assert_eq!(f.recorder.resolution_count(), 0);

    assert!(matches!(
        f.cache
            .resolve_instance_uuid(&ctx(), &instance_id(&alias))
            .await,
        Err(CanonicalError::NotFound { .. })
    ));
    assert_eq!(resolved(&f, 0), [gts_key(&alias)]);
    assert_eq!(
        f.cache
            .resolve_instance_uuids(&ctx(), &[instance_id(&alias)])
            .await
            .expect("read")[&instance_id(&alias)],
        None
    );
    assert_eq!(
        f.lookup(gts_key(&alias), full(), false).await,
        EntityLookup::NotFound,
        "the resident snapshot does not answer another identifier"
    );
    assert_eq!(f.recorder.resolution_count(), 2);
    assert_eq!(f.recorder.batch_count(), 2);

    assert!(matches!(
        f.lookup(gts_key(&registered), full(), false).await,
        EntityLookup::Found { .. }
    ));
    assert_eq!(
        f.cache
            .resolve_instance_id(&ctx(), uuid(&registered))
            .await
            .expect("known"),
        instance_id(&registered)
    );
    assert_eq!(
        f.cache
            .resolve_instance_uuid(&ctx(), &instance_id(&registered))
            .await
            .expect("known"),
        uuid(&registered)
    );
    assert_eq!(
        (f.recorder.batch_count(), f.recorder.resolution_count()),
        (2, 2),
        "the alias's absence left the snapshot and the identity in place"
    );
}

/// The byte budget fits two of these three equal-weight snapshots.
/// An alias miss leaves the old snapshot least recently used.
#[tokio::test(start_paused = true)]
async fn a_miss_by_another_spelling_leaves_the_resident_snapshot_s_recency() {
    let registered = format!("{TYPE}{TAIL}");
    let recent = format!("{TYPE}0b7a5e1c-2d3f-4a5b-8c6d-7e8f9a0b1c2d");
    let filling = format!("{TYPE}1c8b6f2d-3e4a-4b5c-9d7e-8f9a0b1c2d3e");
    let alias = format!("{OTHER}{TAIL}");
    let light = || Projection::Select(FieldSelection::light());
    let registry = MockTypesRegistry::new().completing_after(0);
    for id in [&registered, &recent, &filling] {
        registry.seed(id, json!({}));
    }
    let f = fixture_with(
        registry,
        CacheSettings {
            freshness_window: WINDOW,
            max_entries: 2,
            ..CacheSettings::default()
        },
    );
    let read = |id: &str| f.lookup(gts_key(id), light(), false);
    assert!(matches!(
        read(&registered).await,
        EntityLookup::Found { .. }
    ));
    assert!(matches!(read(&recent).await, EntityLookup::Found { .. }));

    assert_eq!(read(&alias).await, EntityLookup::NotFound);
    assert!(matches!(read(&filling).await, EntityLookup::Found { .. }));
    assert_eq!(f.recorder.batch_count(), 4);

    read(&recent).await;
    assert_eq!(
        f.recorder.batch_count(),
        4,
        "the more recent snapshot stayed"
    );
    read(&registered).await;
    assert_eq!(f.recorder.batch_count(), 5, "the resident was evicted");
}

/// Reject a conflicting reply against a pinned pair even if both stores evicted
/// it during resolution.
#[tokio::test(start_paused = true)]
async fn a_resolution_answer_contradicting_an_identity_it_holds_fails() {
    let f = fixture_with(
        MockTypesRegistry::new().completing_after(0),
        CacheSettings {
            freshness_window: WINDOW,
            max_size_bytes: 0,
            max_identity_entries: 1,
            ..CacheSettings::default()
        },
    );
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("registered");
    let held = uuid(TYPE);
    let release = f.recorder.hold_next();
    f.recorder.tamper_next_resolution(move |response| {
        if let Some(Some(identity)) = response.get_mut(&gts_key(THIRD)) {
            identity.gts_uuid = held;
        }
    });
    let caller = ctx();
    let (resolution, ()) = tokio::join!(
        f.cache
            .batch_resolve_identities(&caller, vec![gts_key(TYPE), gts_key(THIRD)]),
        async {
            f.cache
                .resolve_type_uuid(&ctx(), &type_id(OTHER))
                .await
                .expect("registered");
            assert!(f.cache.identity(&gts_key(TYPE)).is_none(), "evicted");
            release.send(()).expect("held read");
        }
    );
    let error = resolution.expect_err("two identifiers for one Registry Reference");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    assert_eq!(
        resolved(&f, 1),
        [gts_key(THIRD)],
        "only the unknown key was resolved"
    );
    assert!(f.cache.identity(&gts_key(THIRD)).is_none());
    assert!(f.cache.identity(&EntityKey::GtsUuid(held)).is_none());
}

/// A malformed resolution fails whole before any learning. Every malformed shape is
/// covered where it is checked (`ext::checked_resolutions`); this pins that the
/// cache checks before it learns.
#[tokio::test(start_paused = true)]
async fn a_malformed_resolution_fails_whole_and_teaches_nothing() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next_resolution(|answers| {
        answers.remove(&gts_key(OTHER));
    });
    let error = f
        .cache
        .batch_resolve_identities(&ctx(), vec![gts_key(TYPE), gts_key(OTHER)])
        .await
        .expect_err("an unanswered key");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
    for key in [
        gts_key(TYPE),
        gts_key(OTHER),
        uuid_key(TYPE),
        uuid_key(OTHER),
    ] {
        assert!(f.cache.identity(&key).is_none(), "{key}");
    }
    f.cache
        .batch_resolve_identities(&ctx(), vec![gts_key(TYPE), gts_key(OTHER)])
        .await
        .expect("an honest resolution");
    assert_eq!(f.recorder.resolution_count(), 2);
}

/// Resident snapshots reject conflicting pairs even without an identity LRU.
#[tokio::test(start_paused = true)]
async fn a_resolution_contradicting_a_resident_snapshot_fails() {
    let f = fixture_bound(WINDOW, 0);
    f.title(gts_key(TYPE), full(), false).await;
    let resident = uuid(TYPE);
    f.recorder.tamper_next_resolution(move |answers| {
        if let Some(Some(identity)) = answers.get_mut(&gts_key(OTHER)) {
            identity.gts_uuid = resident;
        }
    });
    let error = f
        .cache
        .batch_resolve_identities(&ctx(), vec![gts_key(OTHER)])
        .await
        .expect_err("two identifiers for one Registry Reference");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

/// Forward an invalid-size batch intact, including known keys.
#[tokio::test(start_paused = true)]
async fn an_over_long_or_empty_resolution_is_sent_whole_for_the_registry_to_refuse() {
    let f = fixture(WINDOW);
    f.cache
        .resolve_type_uuid(&ctx(), &type_id(TYPE))
        .await
        .expect("known");
    let mut keys = vec![gts_key(TYPE); MAX_BATCH_GET_KEYS];
    keys.push(gts_key(OTHER));
    for keys in [keys, Vec::new()] {
        let sent = keys.len();
        let error = f
            .cache
            .batch_resolve_identities(&ctx(), keys)
            .await
            .expect_err("refused");
        assert!(
            matches!(error, CanonicalError::InvalidArgument { .. }),
            "{error:?}"
        );
        assert_eq!(f.recorder.resolutions().last().map(Vec::len), Some(sent));
    }
}
