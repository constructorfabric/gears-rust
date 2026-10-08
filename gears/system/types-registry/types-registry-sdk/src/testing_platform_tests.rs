//! The fake answers reads under the real client's contract: projection-scoped validators,
//! first-mention duplicates, lifecycle and depth filters, query-bound cursors.

use std::num::NonZeroU8;

use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_security::PlatformSecurityContext;

use super::FakePlatformRegistry;
use crate::contract::PlatformTypesRegistryApi;
use crate::field;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetItem, DeleteEntitiesRequest, DeleteItem, EntityField,
    EntityFilter, EntityKey, EntityKind, EntityLookup, FieldSelection, IdempotencyKey,
    LifecycleFilter, ListEntitiesRequest, PageRequest, Projection, PublisherContext,
};

const TYPE: &str = "gts.cf.test.pkg.a.v1~";
const OTHER: &str = "gts.cf.test.pkg.b.v1~";
const CHAINED: &str = "gts.cf.test.pkg.a.v1~cf.test.pkg.child.v1~";

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn key(id: &str) -> EntityKey {
    EntityKey::GtsId(gts::GtsId::try_new(id).expect("valid identifier"))
}

fn content_only() -> Projection {
    Projection::Select(FieldSelection::with(&[EntityField::Content]))
}

async fn read(
    fake: &FakePlatformRegistry,
    items: Vec<BatchGetItem>,
    projection: Projection,
) -> EntityLookup {
    let mut answers = fake
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest {
                items,
                projection,
                fresh: false,
            },
        )
        .await
        .expect("reads")
        .0;
    assert_eq!(answers.len(), 1, "one key, one answer");
    answers.remove(&key(TYPE)).expect("the key is answered")
}

async fn ids(fake: &FakePlatformRegistry, filter: EntityFilter) -> Vec<String> {
    fake.list_entities(
        &ctx(),
        ListEntitiesRequest {
            filter,
            ..ListEntitiesRequest::default()
        },
    )
    .await
    .expect("lists")
    .items
    .into_iter()
    .map(|e| e.gts_id.to_string())
    .collect()
}

#[tokio::test]
async fn a_validator_read_under_one_projection_does_not_condition_another() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    let EntityLookup::Found { etag, .. } =
        read(&fake, vec![key(TYPE).into()], Projection::Default).await
    else {
        panic!("seeded");
    };
    let conditional = || BatchGetItem {
        key: key(TYPE),
        if_none_match: Some(etag.clone()),
    };

    assert!(matches!(
        read(&fake, vec![conditional()], Projection::Default).await,
        EntityLookup::Unchanged { .. }
    ));
    assert!(matches!(
        read(&fake, vec![conditional()], content_only()).await,
        EntityLookup::Found { .. }
    ));
}

#[tokio::test]
async fn a_repeated_key_is_answered_under_its_first_mention() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    let EntityLookup::Found { etag, .. } =
        read(&fake, vec![key(TYPE).into()], Projection::Default).await
    else {
        panic!("seeded");
    };
    let conditional = BatchGetItem {
        key: key(TYPE),
        if_none_match: Some(etag),
    };

    let lookup = read(
        &fake,
        vec![key(TYPE).into(), conditional.clone()],
        Projection::Default,
    )
    .await;
    assert!(matches!(lookup, EntityLookup::Found { .. }), "{lookup:?}");

    let lookup = read(
        &fake,
        vec![conditional, key(TYPE).into()],
        Projection::Default,
    )
    .await;
    assert!(
        matches!(lookup, EntityLookup::Unchanged { .. }),
        "{lookup:?}"
    );
}

#[tokio::test]
async fn discovery_applies_the_lifecycle_filter() {
    let fake = FakePlatformRegistry::new().completing_after(0);
    fake.seed(TYPE, json!({}));
    fake.seed(OTHER, json!({}));
    fake.delete_entities(
        &ctx(),
        IdempotencyKey::new("delete-b").expect("key"),
        DeleteEntitiesRequest {
            items: vec![DeleteItem {
                key: key(OTHER),
                expected_resource_version: 1,
            }],
            dry_run: false,
            publisher: PublisherContext {
                name: "fake-test".to_owned(),
                version: "1.0.0".parse().expect("version"),
            },
        },
    )
    .await
    .expect("deletes");

    let with = |lifecycle| EntityFilter {
        lifecycle,
        ..EntityFilter::default()
    };
    assert_eq!(ids(&fake, with(LifecycleFilter::Active)).await, [TYPE]);
    assert_eq!(ids(&fake, with(LifecycleFilter::Deleted)).await, [OTHER]);
    assert_eq!(ids(&fake, with(LifecycleFilter::All)).await, [TYPE, OTHER]);
}

#[tokio::test]
async fn discovery_applies_the_chain_depth_filter() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    fake.seed(CHAINED, json!({}));

    let depth = |d| EntityFilter {
        max_chain_depth: NonZeroU8::new(d),
        ..EntityFilter::default()
    };
    assert_eq!(ids(&fake, depth(1)).await, [TYPE]);
    assert_eq!(ids(&fake, depth(2)).await, [TYPE, CHAINED]);
}

#[tokio::test]
async fn a_cursor_resumes_only_the_query_that_issued_it() {
    let fake = FakePlatformRegistry::new();
    fake.seed(TYPE, json!({}));
    fake.seed(OTHER, json!({}));
    let first = ListEntitiesRequest {
        page: PageRequest {
            limit: Some(1),
            cursor: None,
        },
        ..ListEntitiesRequest::default()
    };
    let cursor = fake
        .list_entities(&ctx(), first.clone())
        .await
        .expect("lists")
        .next
        .expect("a second page");
    let resumed = |mut request: ListEntitiesRequest| {
        request.page.cursor = Some(cursor.clone());
        request
    };

    let page = fake
        .list_entities(&ctx(), resumed(first.clone()))
        .await
        .expect("the same query resumes");
    assert_eq!(page.items[0].gts_id.to_string(), OTHER);

    let refiltered = |filter: EntityFilter| ListEntitiesRequest {
        filter,
        ..first.clone()
    };
    for changed in [
        ListEntitiesRequest {
            projection: content_only(),
            ..first.clone()
        },
        refiltered(EntityFilter {
            pattern: Some(gts::GtsIdPattern::try_new("gts.cf.test.*").expect("valid pattern")),
            ..EntityFilter::default()
        }),
        refiltered(EntityFilter {
            kind: Some(EntityKind::TypeSchema),
            ..EntityFilter::default()
        }),
        refiltered(EntityFilter {
            lifecycle: LifecycleFilter::All,
            ..EntityFilter::default()
        }),
        refiltered(EntityFilter {
            max_chain_depth: NonZeroU8::new(1),
            ..EntityFilter::default()
        }),
    ] {
        let error = fake
            .list_entities(&ctx(), resumed(changed.clone()))
            .await
            .expect_err("a changed query cannot resume");
        let CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } = &error
        else {
            panic!("not a field violation for {changed:?}: {error:?}");
        };
        assert_eq!(
            field_violations[0].field,
            field::CURSOR_FIELD,
            "{changed:?}"
        );
        assert_eq!(
            field_violations[0].reason,
            field::VALIDATION_FAILED,
            "{changed:?}"
        );
    }

    let wider = ListEntitiesRequest {
        page: PageRequest {
            limit: Some(10),
            cursor: None,
        },
        ..first
    };
    fake.list_entities(&ctx(), resumed(wider))
        .await
        .expect("the page size is not part of the binding");
}
