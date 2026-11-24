use std::sync::Arc;

use gts::GtsId;
use serde_json::json;
use tokio::time::Instant;
use uuid::Uuid;

use super::{ENTRY_ALLOWANCE_BYTES, Store, merge, project, union};
use crate::models::{
    Entity, EntityField, EntityKey, EntityKind, FieldSelection, JsonDocument, LifecycleStatus,
    Origin, Provenance, Validator,
};

const TYPE: &str = "gts.cf.cache.store.entity.v1~";

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid GTS identifier")
}

fn entity(gts_id: &str, content: Option<JsonDocument>) -> Arc<Entity> {
    let gts_id = id(gts_id);
    let gts_uuid = gts_id.to_uuid();
    let kind = EntityKind::of(&gts_id);
    Arc::new(Entity {
        gts_id,
        gts_uuid,
        kind,
        lifecycle_status: LifecycleStatus::Active,
        ownership: crate::models::Ownership::Global,
        origin: None,
        content,
        resolved_schema: None,
        effective_traits: None,
        effective_traits_schema: None,
        provenance: None,
    })
}

fn instance_id(n: usize) -> String {
    format!("{TYPE}cf.cache.store.item{n}.v1")
}

/// The bytes `snapshot` is charged.
fn weight(snapshot: &Entity) -> usize {
    super::weight(snapshot).total()
}

/// Resident bytes with the two-byte validators used by these fixtures.
fn resident_bytes(snapshot: &Entity) -> usize {
    weight(snapshot).saturating_add(2)
}

fn key(snapshot: &Entity) -> EntityKey {
    EntityKey::GtsUuid(snapshot.gts_uuid)
}

fn spelled(snapshot: &Entity) -> EntityKey {
    EntityKey::GtsId(snapshot.gts_id.clone())
}

/// `snapshot` under a Registry Reference the registry issued, not one derived from it.
fn issued(snapshot: &Arc<Entity>, gts_uuid: Uuid) -> Arc<Entity> {
    let mut issued = (**snapshot).clone();
    issued.gts_uuid = gts_uuid;
    Arc::new(issued)
}

fn etag(s: &str) -> Validator {
    Validator::from_bytes(s.as_bytes())
}

fn put(store: &mut Store, snapshot: &Arc<Entity>) {
    store.insert(
        Arc::clone(snapshot),
        FieldSelection::full(),
        super::weight(snapshot),
        etag("v1"),
        Instant::now(),
    );
}

fn fixed(gts_id: &str) -> usize {
    gts_id.len() + ENTRY_ALLOWANCE_BYTES
}

#[test]
fn a_snapshot_without_documents_weighs_its_identifier_and_the_allowance() {
    assert_eq!(weight(&entity(TYPE, None)), TYPE.len() + 256);
    let id = instance_id(1);
    assert_eq!(weight(&entity(&id, None)), id.len() + 256);
}

#[test]
fn documents_are_charged_compact_json_and_other_metadata_only_the_allowance() {
    let bare = {
        let mut bare = (*entity(TYPE, Some(json!({ "type": "object", "title": "T" })))).clone();
        bare.resolved_schema = Some(json!([1, 2]));
        bare.effective_traits = Some(json!({}));
        bare.effective_traits_schema = Some(json!("x\"y"));
        bare
    };
    let documents = [
        r#"{"type":"object","title":"T"}"#,
        "[1,2]",
        "{}",
        r#""x\"y""#,
    ];
    let expected = fixed(TYPE) + documents.iter().map(|d| d.len()).sum::<usize>();
    assert_eq!(weight(&bare), expected);

    let mut annotated = bare;
    annotated.lifecycle_status = LifecycleStatus::Deleted;
    annotated.origin = Some(Origin::Managed {
        resource_version: u64::MAX,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
    });
    annotated.provenance = Some(Provenance {
        compatibility_check_skipped: true,
    });
    assert_eq!(
        weight(&annotated),
        expected,
        "status, origin and provenance are within the allowance"
    );
}

#[test]
fn a_snapshot_exactly_the_bound_fits_and_one_byte_more_does_not() {
    let snapshot = entity(TYPE, Some(json!({ "a": 1 })));
    let mut exact = Store::new(resident_bytes(&snapshot), usize::MAX);
    put(&mut exact, &snapshot);
    assert!(exact.get(&key(&snapshot)).is_some());

    let mut short = Store::new(resident_bytes(&snapshot) - 1, usize::MAX);
    put(&mut short, &snapshot);
    assert!(short.get(&key(&snapshot)).is_none());
}

#[test]
fn replacing_and_removing_keep_the_byte_count_exact() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let small = entity(TYPE, None);
    let large = entity(TYPE, Some(json!({ "a": "b".repeat(100) })));
    let rep = key(&small);

    put(&mut store, &small);
    assert_eq!(store.used(), resident_bytes(&small));
    store.insert(
        Arc::clone(&large),
        FieldSelection::full(),
        super::weight(&large),
        etag("longer-v2"),
        Instant::now(),
    );
    assert_eq!(
        store.used(),
        weight(&large) + "longer-v2".len(),
        "a replacement is counted once"
    );

    store.invalidate(&rep, None);
    assert_eq!(store.used(), 0);
    assert!(store.get(&rep).is_none());
}

#[test]
fn invalidating_an_entity_drops_its_entry_and_nothing_else() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let target = entity(TYPE, Some(json!({})));
    let other = entity(&instance_id(1), None);
    put(&mut store, &target);
    put(&mut store, &other);

    store.invalidate(&key(&target), None);

    assert!(store.get(&key(&target)).is_none());
    assert!(store.get(&key(&other)).is_some());
    assert_eq!(store.used(), resident_bytes(&other));
}

#[test]
fn the_bound_is_bytes_a_thousand_small_entries_fit_and_one_megabyte_evicts() {
    const BOUND: usize = 1024 * 1024;
    let mut store = Store::new(BOUND, usize::MAX);
    let small: Vec<_> = (0..1000).map(|n| entity(&instance_id(n), None)).collect();
    for snapshot in &small {
        put(&mut store, snapshot);
    }
    // The old entry-count bound of 1024 would also admit these; bytes are the point.
    assert!(
        small.iter().all(|s| store.get(&key(s)).is_some()),
        "a thousand small snapshots fit within one megabyte"
    );
    // Promote the first one: it is now the most recently used of the small ones.
    assert!(store.get(&key(&small[0])).is_some());

    let large = entity(TYPE, Some(json!("x".repeat(1_000_000))));
    assert!(resident_bytes(&large) <= BOUND);
    put(&mut store, &large);

    assert!(store.get(&key(&large)).is_some());
    assert!(store.used() <= BOUND);
    assert!(
        store.get(&key(&small[1])).is_none(),
        "the least recently used entry is evicted first"
    );
    assert!(
        store.get(&key(&small[0])).is_some(),
        "the promoted entry outlives the eviction"
    );
}

#[test]
fn an_entry_larger_than_the_bound_is_not_retained_and_evicts_nothing() {
    let mut store = Store::new(4096, usize::MAX);
    let resident = entity(&instance_id(1), None);
    put(&mut store, &resident);
    let stale = entity(TYPE, None);
    put(&mut store, &stale);

    let oversized = entity(TYPE, Some(json!("x".repeat(8192))));
    put(&mut store, &oversized);

    assert!(
        store.get(&key(&oversized)).is_none(),
        "neither the oversized snapshot nor the one it replaces stays resident"
    );
    assert!(store.get(&key(&resident)).is_some());
    assert_eq!(store.used(), resident_bytes(&resident));
}

#[test]
fn an_invalidation_fences_a_ticket_taken_before_it_even_with_nothing_resident() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let uuid = EntityKey::GtsUuid(Uuid::nil());
    let before = store.ticket(&uuid);
    store.invalidate(&uuid, None);
    assert!(!store.quiet_since(&before));
    let after = store.ticket(&uuid);
    assert!(store.quiet_since(&after));
    store.release(&before);
    store.release(&after);
    assert!(
        !store.indexes(&uuid),
        "released tickets leave no bookkeeping"
    );
}

#[test]
fn a_ticket_keeps_its_entitys_history_through_eviction() {
    let target = entity(TYPE, None);
    let filler = entity(&instance_id(1), None);
    // Room for either fixture alone, never both.
    let mut store = Store::new(
        resident_bytes(&target).max(resident_bytes(&filler)),
        usize::MAX,
    );
    put(&mut store, &target);
    let ticket = store.ticket(&key(&target));
    store.invalidate(&key(&target), None);
    put(&mut store, &target);
    put(&mut store, &filler);
    assert!(store.get(&key(&target)).is_none(), "evicted");
    assert!(store.get(&key(&filler)).is_some(), "retained");

    let later = store.ticket(&key(&target));
    assert!(store.quiet_since(&later));
    assert!(
        !store.quiet_since(&ticket),
        "the slot kept the invalidation the old ticket must not see past"
    );
}

#[test]
fn a_confirmation_only_refreshes_the_validator_it_names() {
    let snapshot = entity(TYPE, None);
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let rep = key(&snapshot);
    let at = Instant::now();
    store.insert(
        Arc::clone(&snapshot),
        FieldSelection::full(),
        super::weight(&snapshot),
        etag("v2"),
        at,
    );
    let later = at + std::time::Duration::from_secs(5);
    store.confirm(&rep, &etag("v1"), later);
    assert_eq!(store.peek(&rep).expect("resident").confirmed_at, at);
    store.confirm(&rep, &etag("v2"), later);
    assert_eq!(store.peek(&rep).expect("resident").confirmed_at, later);
    store.confirm(&rep, &etag("v2"), at);
    assert_eq!(
        store.peek(&rep).expect("resident").confirmed_at,
        later,
        "never moved back"
    );
}

#[test]
fn a_saturated_weight_is_never_retained_even_under_an_unlimited_bound() {
    let mut store = Store::new(usize::MAX, usize::MAX);
    let ordinary = entity(&instance_id(1), None);
    put(&mut store, &ordinary);
    let snapshot = entity(TYPE, Some(JsonDocument::Null));
    let saturated = super::Weight {
        fixed: fixed(TYPE),
        documents: [(EntityField::Content, usize::MAX - 1)].into(),
    };
    assert_eq!(saturated.total(), usize::MAX);
    assert!(!saturated.fits(usize::MAX, &etag("")));

    store.insert(
        Arc::clone(&snapshot),
        FieldSelection::full(),
        saturated,
        etag("v1"),
        Instant::now(),
    );
    assert!(store.get(&key(&snapshot)).is_none());
    assert_eq!(store.used(), resident_bytes(&ordinary));
}

/// A Type Schema carrying every field.
fn every_field() -> Entity {
    let mut full = (*entity(TYPE, Some(json!({ "c": 1 })))).clone();
    full.origin = Some(Origin::Managed {
        resource_version: 3,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
    });
    full.resolved_schema = Some(json!({ "r": 1 }));
    full.effective_traits = Some(json!({ "t": 1 }));
    full.effective_traits_schema = Some(json!({ "s": 1 }));
    full.provenance = Some(Provenance {
        compatibility_check_skipped: false,
    });
    full
}

#[test]
fn a_projection_keeps_exactly_the_selected_fields() {
    let full = every_field();
    let content = FieldSelection::with(&[EntityField::Content]);
    let projected = project(&full, &content);
    let mut expected = (*entity(TYPE, None)).clone();
    expected.content = full.content.clone();
    assert_eq!(projected, expected);
    assert_eq!(project(&full, &FieldSelection::full()), full);
}

/// Merge selected replies with the remaining resident fields and reuse their weights.
#[test]
fn a_merge_takes_the_selection_from_the_answer_and_the_rest_from_the_entry() {
    let full = every_field();
    let traits = FieldSelection::with(&[EntityField::EffectiveTraits, EntityField::Origin]);
    let content = FieldSelection::with(&[EntityField::Content]);
    let resident = project(&full, &traits);
    let answer = project(&full, &content);

    let merged = merge(answer, &content, &resident);
    let covers = union(&traits, &content);
    assert_eq!(merged, project(&full, &covers));
    assert_eq!(
        covers,
        FieldSelection::with(&[
            EntityField::Content,
            EntityField::EffectiveTraits,
            EntityField::Origin
        ])
    );
}

#[test]
fn a_merged_selected_null_replaces_the_resident_document_charge() {
    let content = FieldSelection::with(&[EntityField::Content]);
    let resident = entity(TYPE, Some(json!({ "big": "x".repeat(100) })));
    let answer = entity(TYPE, Some(JsonDocument::Null));
    let merged = merge((*answer).clone(), &content, &resident);
    assert_eq!(merged.content, Some(JsonDocument::Null));
    let accounted = super::weight(&answer).merged(&content, &super::weight(&resident));
    assert_eq!(accounted.total(), fixed(TYPE) + "null".len());
    assert_eq!(accounted, super::weight(&merged));
}

/// An omitted selected field stays absent after merging.
#[test]
fn a_merge_never_resurrects_a_selected_field_the_answer_lacks() {
    let full = every_field();
    let resident = full.clone();
    let mut answer = project(&full, &FieldSelection::full());
    answer.resolved_schema = None;
    let merged = merge(answer.clone(), &FieldSelection::full(), &resident);
    assert_eq!(merged, answer);
}

mod weight_properties {
    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::{TYPE, entity, fixed, instance_id, merge, project, weight};
    use crate::models::{EntityField, FieldSelection};

    /// Exercise JSON sizing with quotes, backslashes, control characters and multibyte text.
    fn document() -> impl Strategy<Value = Value> {
        let text = prop_oneof![
            any::<String>(),
            "[\\x00-\\x1f\"\\\\/a-z\u{e9}\u{4e2d}\u{1f600}]{0,12}",
        ];
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            any::<f64>().prop_map(|n| json!(n)),
            text.clone().prop_map(Value::String),
        ];
        leaf.prop_recursive(3, 24, 4, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::vec((text.clone(), inner), 0..4)
                    .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
            ]
        })
    }

    fn snapshot(instance: bool, docs: Vec<Option<Value>>) -> crate::models::Entity {
        let id = if instance {
            instance_id(1)
        } else {
            TYPE.to_owned()
        };
        let mut snapshot = (*entity(&id, None)).clone();
        let [content, resolved, traits, traits_schema] = <[_; 4]>::try_from(docs).expect("four");
        snapshot.content = content;
        snapshot.resolved_schema = resolved;
        snapshot.effective_traits = traits;
        snapshot.effective_traits_schema = traits_schema;
        snapshot
    }

    fn selection() -> impl Strategy<Value = FieldSelection> {
        prop::sample::subsequence(EntityField::ALL.to_vec(), 0..=EntityField::ALL.len())
            .prop_map(|fields| FieldSelection::with(&fields))
    }

    proptest! {
        #[test]
        fn a_snapshot_weighs_its_fixed_charge_and_compact_documents(
            instance in any::<bool>(),
            docs in prop::collection::vec(prop::option::of(document()), 4),
        ) {
            let snapshot = snapshot(instance, docs.clone());
            let documents: usize = docs
                .iter()
                .flatten()
                .map(|doc| serde_json::to_string(doc).expect("serializable").len())
                .sum();
            prop_assert_eq!(
                weight(&snapshot),
                fixed(snapshot.gts_id.as_ref()) + documents
            );
        }

        #[test]
        fn merged_accounting_equals_measuring_the_merge(
            resident in prop::collection::vec(prop::option::of(document()), 4),
            answer in prop::collection::vec(prop::option::of(document()), 4),
            covers in selection(),
            selected in selection(),
        ) {
            let resident = project(&snapshot(false, resident), &covers);
            let answer = project(&snapshot(false, answer), &selected);
            let merged = merge(answer.clone(), &selected, &resident);
            prop_assert_eq!(
                super::super::weight(&answer).merged(&selected, &super::super::weight(&resident)),
                super::super::weight(&merged)
            );
        }
    }
}

#[test]
fn an_identifier_resolves_only_while_its_snapshot_is_resident() {
    let snapshot = issued(&entity(TYPE, None), Uuid::from_u128(7));
    let filler = entity(&instance_id(1), None);
    // Room for either fixture alone, never both.
    let mut store = Store::new(
        resident_bytes(&snapshot).max(resident_bytes(&filler)),
        usize::MAX,
    );
    put(&mut store, &snapshot);
    assert!(store.indexes(&spelled(&snapshot)));

    put(&mut store, &filler);
    assert!(store.get(&key(&snapshot)).is_none(), "evicted");
    assert!(!store.indexes(&spelled(&snapshot)));
    assert!(store.get(&spelled(&snapshot)).is_none());
}

#[test]
fn a_resident_pair_disagrees_with_either_spelling_paired_otherwise() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let snapshot = issued(&entity(TYPE, None), Uuid::from_u128(7));
    put(&mut store, &snapshot);
    let other = id(&instance_id(1));

    assert!(
        store
            .conflict(&snapshot.gts_id, snapshot.gts_uuid)
            .is_none()
    );
    assert_eq!(
        store.conflict(&snapshot.gts_id, Uuid::from_u128(8)),
        Some(EntityKey::GtsUuid(snapshot.gts_uuid))
    );
    assert_eq!(
        store.conflict(&other, snapshot.gts_uuid),
        Some(EntityKey::GtsId(snapshot.gts_id.clone()))
    );
    assert!(
        store.conflict(&other, Uuid::from_u128(8)).is_none(),
        "unrelated"
    );
}

#[test]
fn invalidating_by_one_spelling_drops_the_entry_and_fences_reads_by_the_other() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let snapshot = issued(&entity(TYPE, None), Uuid::from_u128(7));
    put(&mut store, &snapshot);
    let by_uuid = store.ticket(&key(&snapshot));

    store.invalidate(&spelled(&snapshot), None);

    assert!(store.get(&key(&snapshot)).is_none());
    assert!(store.get(&spelled(&snapshot)).is_none());
    assert!(!store.quiet_since(&by_uuid));
    store.release(&by_uuid);
    assert!(!store.indexes(&spelled(&snapshot)));
    assert!(!store.indexes(&key(&snapshot)));
}

/// A ticket pins both spellings through eviction, so either still fences it.
#[test]
fn a_ticket_taken_with_both_spellings_is_fenced_by_either_after_eviction() {
    let snapshot = issued(&entity(TYPE, None), Uuid::from_u128(7));
    let filler = entity(&instance_id(1), None);
    let mut store = Store::new(
        resident_bytes(&snapshot).max(resident_bytes(&filler)),
        usize::MAX,
    );
    put(&mut store, &snapshot);
    let by_uuid = store.ticket(&key(&snapshot));
    put(&mut store, &filler);
    assert!(store.get(&key(&snapshot)).is_none(), "evicted");
    assert!(store.quiet_since(&by_uuid));

    store.invalidate(&spelled(&snapshot), None);
    assert!(!store.quiet_since(&by_uuid));
}

/// A paired reply fences reads under either key of that entity.
#[test]
fn a_record_naming_both_spellings_fences_only_that_entity() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let gts_uuid = Uuid::from_u128(7);
    let by_uuid = store.ticket(&EntityKey::GtsUuid(gts_uuid));
    let elsewhere = store.ticket(&EntityKey::GtsUuid(Uuid::from_u128(8)));

    store.record(&EntityKey::GtsId(id(TYPE)), Some((&id(TYPE), gts_uuid)));

    assert!(!store.quiet_since(&by_uuid));
    assert!(store.quiet_since(&elsewhere));
}

/// Inserting a snapshot whose identity contradicts a resident one never leaves two
/// entities claiming one spelling.
#[test]
fn an_insert_contradicting_a_resident_pair_drops_the_resident_entity() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let first = issued(&entity(TYPE, None), Uuid::from_u128(7));
    put(&mut store, &first);
    let moved = issued(&first, Uuid::from_u128(8));
    put(&mut store, &moved);

    assert!(store.get(&key(&first)).is_none());
    assert_eq!(
        store
            .get(&spelled(&moved))
            .expect("resident")
            .snapshot
            .gts_uuid,
        moved.gts_uuid
    );
    assert_eq!(store.used(), resident_bytes(&moved));
}

/// Distinct keys of the same form cannot alias. A one-sided event fences
/// only matching keys and possible counterparts.
#[test]
fn a_one_sided_event_fences_unresolved_reads_of_the_other_form_only() {
    let (a, b) = (
        EntityKey::GtsId(id(TYPE)),
        EntityKey::GtsId(id(&instance_id(1))),
    );
    let (u, v) = (
        EntityKey::GtsUuid(Uuid::from_u128(7)),
        EntityKey::GtsUuid(Uuid::from_u128(8)),
    );
    for (event, same_form, other_form) in [(&b, &a, &u), (&v, &u, &a)] {
        let mut store = Store::new(1024 * 1024, usize::MAX);
        let own = store.ticket(event);
        let same = store.ticket(same_form);
        let other = store.ticket(other_form);

        store.record(event, None);

        assert!(!store.quiet_since(&own), "{event}: its own key");
        assert!(store.quiet_since(&same), "{event}: another key of its form");
        assert!(
            !store.quiet_since(&other),
            "{event}: possibly the same entity"
        );
    }
}

/// A read that knew both spellings is fenced by neither one-sided counter: only its own
/// slots.
#[test]
fn a_one_sided_event_of_another_entity_leaves_a_resolved_read_alone() {
    let mut store = Store::new(1024 * 1024, usize::MAX);
    let resident = issued(&entity(TYPE, None), Uuid::from_u128(7));
    put(&mut store, &resident);
    let resolved = store.ticket(&spelled(&resident));

    store.record(&EntityKey::GtsId(id(&instance_id(1))), None);
    store.record(&EntityKey::GtsUuid(Uuid::from_u128(8)), None);

    assert!(store.quiet_since(&resolved));
}

#[test]
fn entry_limit_evicts_the_lru_and_zero_keeps_no_snapshots() {
    let [a, b, c] = [1, 2, 3].map(|n| entity(&instance_id(n), None));
    let mut store = Store::new(usize::MAX, 2);
    put(&mut store, &a);
    put(&mut store, &b);
    assert!(store.get(&key(&a)).is_some());
    put(&mut store, &c);
    assert!(store.peek(&key(&a)).is_some());
    assert!(store.peek(&key(&b)).is_none());
    assert!(!store.indexes(&spelled(&b)));
    assert!(store.peek(&key(&c)).is_some());
    assert_eq!(store.used(), resident_bytes(&a) + resident_bytes(&c));

    let mut disabled = Store::new(usize::MAX, 0);
    put(&mut disabled, &a);
    assert_eq!(disabled.used(), 0);
    assert!(disabled.peek(&key(&a)).is_none());
}

/// A resolution shows only the pair. It never erases, extends or replaces a read
/// proof, and a reply from an earlier request never moves a proof back.
#[test]
fn a_resolution_neither_erases_nor_extends_a_read_proof() {
    use std::time::Duration;

    use super::ReadContext;
    use crate::models::EntityIdentity;

    let window = Duration::from_secs(30);
    let mut store = Store::new(1024 * 1024, 16);
    store.remember_identities(16);
    store.track_visibility();
    // Tenant-owned: a global entity keeps no per-entity proofs.
    let mut owned = (*entity(TYPE, None)).clone();
    owned.ownership = crate::models::Ownership::ContextTenant;
    let snapshot = Arc::new(owned);
    let t0 = Instant::now();
    store.insert(
        Arc::clone(&snapshot),
        FieldSelection::light(),
        super::weight(&snapshot),
        etag("v1"),
        t0,
    );
    store.identities.learn(&snapshot.gts_id, snapshot.gts_uuid);
    let reader = ReadContext::new(Uuid::from_u128(10), None, Uuid::from_u128(1), &[]);
    let pair = EntityIdentity {
        gts_id: snapshot.gts_id.clone(),
        gts_uuid: snapshot.gts_uuid,
    };

    store.confirm_visible(&spelled(&snapshot), &reader, &etag("v1"), t0);
    let later = t0 + Duration::from_secs(10);
    store.confirm_resolved(snapshot.gts_uuid, &reader, later);
    store.confirm_resolved(snapshot.gts_uuid, &reader, t0 - Duration::from_secs(5));

    let edge = t0 + window - Duration::from_millis(1);
    assert!(store.visible(&key(&snapshot), &reader, edge, window));
    assert!(
        !store.visible(&key(&snapshot), &reader, t0 + window, window),
        "a resolution does not extend the read"
    );
    assert_eq!(
        store.resolvable(&spelled(&snapshot), &reader, t0 + window, window),
        Some(pair),
        "it extends resolution"
    );
    assert_eq!(
        store.resolvable(&key(&snapshot), &reader, later + window, window),
        None,
        "until its own window ends"
    );
}
