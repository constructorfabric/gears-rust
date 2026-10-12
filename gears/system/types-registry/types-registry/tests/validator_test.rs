//! Freshness validators and conditional reads through the database read path
//! (T22d, SPEC §8.5).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::{Candidate, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{
    BatchGetItem, EntityKey, EntityLookup, EntityRecord, RegistryService,
};
use types_registry::domain::selection::FieldSelection;
use types_registry::domain::validator::{IfNoneMatch, Validator};

const NOW: OffsetDateTime = datetime!(2026-09-27 12:00:00 UTC);
const BASE: &str = gts_id!("cf.core.validator.thing.v1~");
const DERIVED: &str = gts_id!("cf.core.validator.thing.v1~cf.core.validator.leaf.v1~");
const INSTANCE: &str = gts_id!("cf.core.validator.thing.v1~cf.core.validator.first.v1");
const MISSING: &str = gts_id!("cf.core.validator.missing.v1~");

struct Harness {
    service: RegistryService,
    _db: Arc<DBProvider<DbError>>,
}

async fn harness() -> Harness {
    let db = common::test_db().await;
    let service = RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        common::no_dispatch(),
        common::metrics(),
    );
    Harness { service, _db: db }
}

fn base(title: &str) -> Value {
    json!({
        "$id": format!("gts://{BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": title,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn derived() -> Value {
    json!({
        "$id": format!("gts://{DERIVED}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": format!("gts://{BASE}") },
            { "type": "object", "properties": { "tier": { "type": "string" } } },
        ],
    })
}

impl Harness {
    async fn run(&self, key: &str, kind: OperationKind, candidate: Candidate) {
        let request = SubmitRequest {
            idempotency_key: Some(key.to_owned()),
            dry_run: false,
            candidates: vec![candidate],
        };
        let accepted = match kind {
            OperationKind::Registration => self.service.submit(&request, NOW).await,
            OperationKind::Deletion => {
                self.service
                    .delete(&common::deletion_of(request), NOW)
                    .await
            }
        }
        .expect("accepted");
        self.service
            .admit(accepted.operation_id, NOW)
            .await
            .expect("admitted");
        let operation = self
            .service
            .operation(accepted.operation_id)
            .await
            .expect("read")
            .expect("operation");
        assert_eq!(
            operation.items[0].status,
            OperationItemStatus::Succeeded,
            "{key}: {:?}",
            operation.items[0].error,
        );
    }

    async fn register(&self, key: &str, gts_id: &str, content: Value, expected: Option<i64>) {
        let candidate = Candidate {
            gts_id: gts_id.to_owned(),
            content: Some(content),
            expected_resource_version: expected,
            force: false,
        };
        self.run(key, OperationKind::Registration, candidate).await;
    }

    async fn delete(&self, key: &str, gts_id: &str, expected: i64) {
        let candidate = Candidate {
            gts_id: gts_id.to_owned(),
            content: None,
            expected_resource_version: Some(expected),
            force: false,
        };
        self.run(key, OperationKind::Deletion, candidate).await;
    }

    async fn lookup(
        &self,
        gts_id: &str,
        selection: FieldSelection,
        if_none_match: Option<IfNoneMatch>,
    ) -> EntityLookup {
        self.service
            .lookup(&EntityKey::parse(gts_id), selection, if_none_match)
            .await
            .expect("read")
    }

    /// An unconditional read's record and validator.
    async fn found(&self, gts_id: &str, selection: FieldSelection) -> (EntityRecord, Validator) {
        match self.lookup(gts_id, selection, None).await {
            EntityLookup::Found { record, etag } => (record, etag),
            other => panic!("{gts_id} is found: {other:?}"),
        }
    }
}

fn holding(etag: Validator) -> IfNoneMatch {
    IfNoneMatch::Validators(vec![etag.encode()])
}

fn select(names: &[&str]) -> FieldSelection {
    FieldSelection::parse(names).expect("valid selection")
}

#[tokio::test]
async fn an_unchanged_entity_keeps_a_byte_identical_validator_and_a_revision_moves_it() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;

    let (_, first) = h.found(BASE, FieldSelection::default()).await;
    let (_, second) = h.found(BASE, FieldSelection::default()).await;
    assert_eq!(first.encode(), second.encode());

    h.register("base-2", BASE, base("two"), Some(1)).await;
    let (_, revised) = h.found(BASE, FieldSelection::default()).await;
    assert_ne!(revised, first);
}

#[tokio::test]
async fn a_refreshed_dependent_gets_a_new_validator_at_the_same_resource_version() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    h.register("derived", DERIVED, derived(), None).await;
    let (before, before_etag) = h.found(DERIVED, FieldSelection::default()).await;

    h.register("base-2", BASE, base("two"), Some(1)).await;
    let (after, after_etag) = h.found(DERIVED, FieldSelection::default()).await;

    assert_eq!(
        after.origin.expect("origin").resource_version,
        before.origin.expect("origin").resource_version,
        "the refresh moves no resource_version",
    );
    assert_ne!(after_etag, before_etag);
}

#[tokio::test]
async fn a_current_validator_answers_unchanged_and_a_stale_one_the_representation() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, stale) = h.found(BASE, FieldSelection::default()).await;
    h.register("base-2", BASE, base("two"), Some(1)).await;
    let (_, current) = h.found(BASE, FieldSelection::default()).await;

    match h
        .lookup(BASE, FieldSelection::default(), Some(holding(current)))
        .await
    {
        EntityLookup::Unchanged { etag } => assert_eq!(etag.encode(), current.encode()),
        other => panic!("unchanged: {other:?}"),
    }
    // The same selection as the validator was issued under, so only the revision
    // can make it stale.
    match h
        .lookup(BASE, FieldSelection::default(), Some(holding(stale)))
        .await
    {
        EntityLookup::Found { etag, .. } => assert_eq!(etag, current),
        other => panic!("found: {other:?}"),
    }
}

#[tokio::test]
async fn a_batch_answers_each_key_by_its_own_validator() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    h.register("derived", DERIVED, derived(), None).await;
    let (_, base_etag) = h.found(BASE, FieldSelection::default()).await;
    let (_, derived_etag) = h.found(DERIVED, FieldSelection::default()).await;
    h.register("base-2", BASE, base("two"), Some(1)).await;

    let items = [
        BatchGetItem {
            key: EntityKey::parse(DERIVED),
            if_none_match: Some(holding(derived_etag)),
        },
        BatchGetItem {
            key: EntityKey::parse(BASE),
            if_none_match: Some(holding(base_etag)),
        },
        BatchGetItem {
            key: EntityKey::parse(MISSING),
            if_none_match: Some(holding(base_etag)),
        },
    ];
    let results = h
        .service
        .batch_get(&items, FieldSelection::default())
        .await
        .expect("read");

    // Both moved: the base by revision, the derived schema by its refresh.
    let answer = |key: &str| common::answer_for(&results, &EntityKey::parse(key));
    assert!(
        matches!(answer(DERIVED), EntityLookup::Found { .. }),
        "{results:?}"
    );
    assert!(
        matches!(answer(BASE), EntityLookup::Found { .. }),
        "{results:?}"
    );
    assert!(
        matches!(answer(MISSING), EntityLookup::NotFound),
        "{results:?}"
    );

    let (_, fresh) = h.found(BASE, FieldSelection::default()).await;
    let items = [
        BatchGetItem {
            key: EntityKey::parse(BASE),
            if_none_match: Some(holding(fresh)),
        },
        BatchGetItem {
            key: EntityKey::parse(DERIVED),
            if_none_match: None,
        },
    ];
    let results = h
        .service
        .batch_get(&items, FieldSelection::default())
        .await
        .expect("read");
    let answer = |key: &str| common::answer_for(&results, &EntityKey::parse(key));
    assert!(
        matches!(answer(BASE), EntityLookup::Unchanged { etag } if *etag == fresh),
        "{results:?}",
    );
    assert!(
        matches!(answer(DERIVED), EntityLookup::Found { .. }),
        "{results:?}"
    );
}

/// A key named twice is answered by its first mention's condition; the second is
/// the same question and is not asked again.
#[tokio::test]
async fn a_duplicate_key_is_answered_by_its_first_condition() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, current) = h.found(BASE, FieldSelection::default()).await;
    let conditional = BatchGetItem {
        key: EntityKey::parse(BASE),
        if_none_match: Some(holding(current)),
    };
    let unconditional = BatchGetItem {
        key: EntityKey::parse(BASE),
        if_none_match: None,
    };

    let results = h
        .service
        .batch_get(
            &[conditional.clone(), unconditional.clone()],
            FieldSelection::default(),
        )
        .await
        .expect("read");
    assert!(
        matches!(results.as_slice(), [(_, EntityLookup::Unchanged { .. })]),
        "{results:?}"
    );
    let results = h
        .service
        .batch_get(&[unconditional, conditional], FieldSelection::default())
        .await
        .expect("read");
    assert!(
        matches!(results.as_slice(), [(_, EntityLookup::Found { .. })]),
        "{results:?}"
    );
}

/// An identifier and its Registry Reference are two keys for one row, so one can
/// be `unchanged` while the other is `found` with its document.
#[tokio::test]
async fn both_keys_of_one_entity_are_answered_by_their_own_conditions() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (record, current) = h.found(BASE, select(&["content"])).await;
    let reference = EntityKey::Uuid(record.gts_uuid);
    let items = [
        BatchGetItem {
            key: reference.clone(),
            if_none_match: Some(holding(current)),
        },
        BatchGetItem {
            key: EntityKey::parse(BASE),
            if_none_match: None,
        },
    ];
    let results = h
        .service
        .batch_get(&items, select(&["content"]))
        .await
        .expect("read");
    let answer = |key: &EntityKey| common::answer_for(&results, key);
    assert!(
        matches!(answer(&reference), EntityLookup::Unchanged { etag } if *etag == current),
        "{results:?}"
    );
    match answer(&EntityKey::parse(BASE)) {
        EntityLookup::Found { record, etag } => {
            assert_eq!(*etag, current);
            assert!(
                record.content.is_some(),
                "the found answer carries its document"
            );
        }
        other => panic!("found: {other:?}"),
    }
}

#[tokio::test]
async fn an_instance_validator_changes_on_revision() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    h.register("instance", INSTANCE, json!({ "name": "first" }), None)
        .await;
    let (_, before) = h.found(INSTANCE, FieldSelection::default()).await;

    h.register("instance-2", INSTANCE, json!({ "name": "second" }), Some(1))
        .await;
    let (_, after) = h.found(INSTANCE, FieldSelection::default()).await;
    assert_ne!(after, before);
}

#[tokio::test]
async fn deletion_moves_the_validator_and_the_tombstone_keeps_one() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, live) = h.found(BASE, FieldSelection::default()).await;

    h.delete("delete", BASE, 1).await;
    let (_, tombstone) = h.found(BASE, FieldSelection::default()).await;
    assert_ne!(tombstone, live);
    assert!(matches!(
        h.lookup(BASE, FieldSelection::default(), Some(holding(live)))
            .await,
        EntityLookup::Found { .. },
    ));
}

#[tokio::test]
async fn a_narrow_validator_never_answers_for_a_wider_selection() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, narrow) = h.found(BASE, FieldSelection::default()).await;

    match h
        .lookup(BASE, select(&["content"]), Some(holding(narrow)))
        .await
    {
        EntityLookup::Found { etag, .. } => assert_ne!(etag, narrow),
        other => panic!("found: {other:?}"),
    }
}

#[tokio::test]
async fn any_matches_an_existing_entity_and_nothing_else() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, etag) = h.found(BASE, FieldSelection::default()).await;

    assert!(matches!(
        h.lookup(BASE, FieldSelection::default(), Some(IfNoneMatch::Any)).await,
        EntityLookup::Unchanged { etag: e } if e == etag,
    ));
    assert!(matches!(
        h.lookup(MISSING, FieldSelection::default(), Some(IfNoneMatch::Any))
            .await,
        EntityLookup::NotFound,
    ));
}

#[tokio::test]
async fn one_matching_validator_in_a_list_is_enough() {
    let h = harness().await;
    h.register("base", BASE, base("one"), None).await;
    let (_, etag) = h.found(BASE, FieldSelection::default()).await;
    let condition = IfNoneMatch::Validators(vec!["garbage".to_owned(), etag.encode()]);

    assert!(matches!(
        h.lookup(BASE, FieldSelection::default(), Some(condition))
            .await,
        EntityLookup::Unchanged { .. },
    ));
}
