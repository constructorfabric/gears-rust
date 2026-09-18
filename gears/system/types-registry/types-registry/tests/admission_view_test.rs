//! Admission-view graph walks compared with the storage adapter.
//!
//! Empty overlays must match; replaced edges must change traversal. Diamond
//! fixtures catch double-counted entities; dense bipartite fixtures distinguish
//! the 512-entity closure bound from edge count.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::dry_run::view::AdmissionView;
use types_registry::domain::admission::worker::{Tuning, WorkerError, run_operation};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{
    DependencyStore, EntityEdge, ReverseImpact, Stores, snapshot_read,
};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-13 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-13 10:20:40 UTC);

const A: &str = gts_id!("cf.core.dvw.a.v1~");
const B: &str = gts_id!("cf.core.dvw.b.v1~");
const C: &str = gts_id!("cf.core.dvw.c.v1~");

type Provider = Arc<DBProvider<DbError>>;

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A Type Schema whose properties are `$ref`s at each of `targets`.
fn referencing(gts_id: &str, targets: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    for (index, target) in targets.iter().enumerate() {
        properties.insert(
            format!("ref{index}"),
            json!({ "$ref": format!("gts://{target}") }),
        );
    }
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": Value::Object(properties),
    })
}

async fn admit(db: &Provider, key: &str, gts_id: &str, content: Value) {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    let operation_id = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind: OperationKind::Registration,
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version: None,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .expect("accepted")
    .operation_id;

    let worker: DBProvider<WorkerError> = DBProvider::new(db.db());
    let outcome = run_operation(
        &stores(),
        &worker,
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: false,
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker itself must not fail");
    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Succeeded,
        "{outcome:?}",
    );
}

/// `b → a`, `c → a`, `c → b`.
async fn seed_diamond(db: &Provider) {
    admit(db, "a", A, referencing(A, &[])).await;
    admit(db, "b", B, referencing(B, &[A])).await;
    admit(db, "c", C, referencing(C, &[A, B])).await;
}

/// Names, sorted, for a comparison that does not depend on row ids.
fn names(rows: &[types_registry::domain::ports::EntityRow]) -> Vec<String> {
    let mut names: Vec<String> = rows.iter().map(|row| row.gts_id.clone()).collect();
    names.sort();
    names
}

/// Compare a reverse-impact answer without needing `PartialEq` on the rows.
fn reverse(result: &ReverseImpact) -> Result<Vec<String>, (usize, usize)> {
    match result {
        ReverseImpact::Within(rows) => Ok(names(rows)),
        ReverseImpact::OverBound { at_least, bound } => Err((*at_least, *bound)),
    }
}

/// Ask the adapter and the view the same question inside one snapshot, and hand
/// both answers back.
async fn compare<T, F>(db: &Provider, ask: F) -> (T, T)
where
    T: Send + 'static,
    F: for<'a> Fn(
            &'a dyn Stores,
            &'a DbTx<'a>,
        ) -> std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    provider
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let adapter = stores();
                let view = AdmissionView::new(stores());
                let from_adapter = ask(adapter.as_ref(), tx).await;
                let from_view = ask(&view, tx).await;
                Ok((from_adapter, from_view))
            })
        })
        .await
        .expect("the comparison reads must not fail")
}

/// Resolve the diamond's entity ids, in `[a, b, c]` order.
async fn diamond_ids(db: &Provider) -> [i64; 3] {
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let rows = provider
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let ids = [A.to_owned(), B.to_owned(), C.to_owned()];
                Ok(stores().find_by_gts_ids(tx, &allow_all(), &ids).await?)
            })
        })
        .await
        .expect("resolve the diamond");
    let id_of = |gts_id: &str| {
        rows.iter()
            .find(|row| row.gts_id == gts_id)
            .expect("every member of the diamond is admitted")
            .id
    };
    [id_of(A), id_of(B), id_of(C)]
}

#[tokio::test]
async fn a_view_closure_matches_the_adapter_on_an_untouched_registry() {
    let db = test_db().await;
    seed_diamond(&db).await;

    let (adapter, view) = compare(&db, |stores, tx| {
        Box::pin(async move {
            let closure = stores
                .closure(
                    tx,
                    &allow_all(),
                    &[C.to_owned(), "gts.cf.core.dvw.absent.v1~".to_owned()],
                )
                .await
                .expect("closure");
            (names(&closure.entities), closure.missing_roots)
        })
    })
    .await;

    assert_eq!(
        view, adapter,
        "the view's forward walk is the adapter's relation, including missing roots",
    );
    assert_eq!(
        view.0,
        vec![A.to_owned(), B.to_owned(), C.to_owned()],
        "the diamond's whole forward closure",
    );
    assert_eq!(view.1, vec!["gts.cf.core.dvw.absent.v1~".to_owned()]);
}

/// Exactly at the bound. `c` is reached from `a` directly and again through `b`,
/// so a walk that charges its allowance before discarding what it has already
/// counted sees three where there are two, and refuses a set that fits.
#[tokio::test]
async fn a_view_reverse_impact_matches_the_adapter_on_a_diamond_at_the_bound() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let [a, ..] = diamond_ids(&db).await;

    let (adapter, view) = compare(&db, move |stores, tx| {
        Box::pin(async move {
            reverse(
                &stores
                    .reverse_impact(tx, &allow_all(), &[a], 2)
                    .await
                    .expect("reverse impact"),
            )
        })
    })
    .await;

    assert_eq!(view, adapter, "the view's reverse walk is the adapter's");
    assert_eq!(
        view,
        Ok(vec![B.to_owned(), C.to_owned()]),
        "two dependents fit a bound of two",
    );
}

/// One below the bound, so both must refuse — and the view must not refuse the
/// case above by the same arithmetic.
#[tokio::test]
async fn a_view_reverse_impact_matches_the_adapter_over_the_bound() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let [a, ..] = diamond_ids(&db).await;

    let (adapter, view) = compare(&db, move |stores, tx| {
        Box::pin(async move {
            reverse(
                &stores
                    .reverse_impact(tx, &allow_all(), &[a], 1)
                    .await
                    .expect("reverse impact"),
            )
            .map_err(|(_, bound)| bound)
        })
    })
    .await;

    assert!(view.is_err(), "two dependents do not fit a bound of one");
    assert_eq!(view, adapter, "and the two agree on refusing");
}

/// Overlapping roots: `c` depends on both, and is excluded from neither answer
/// by being a root of the other.
#[tokio::test]
async fn a_view_reverse_impact_matches_the_adapter_with_overlapping_roots() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let [a, b, _] = diamond_ids(&db).await;

    let (adapter, view) = compare(&db, move |stores, tx| {
        Box::pin(async move {
            reverse(
                &stores
                    .reverse_impact(tx, &allow_all(), &[a, b], 8)
                    .await
                    .expect("reverse impact"),
            )
        })
    })
    .await;

    assert_eq!(view, adapter, "the view's reverse walk is the adapter's");
    assert_eq!(
        view,
        Ok(vec![C.to_owned()]),
        "roots are excluded from their own impact, and `c` is counted once",
    );
}

/// Every internal edge of the diamond — the deletion order's input, on the
/// shape the other cases use.
#[tokio::test]
async fn a_view_edges_within_matches_the_adapter_on_the_diamond() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let ids = diamond_ids(&db).await;
    let [a, b, c] = ids;

    let (adapter, view) = compare(&db, move |stores, tx| {
        Box::pin(async move {
            stores
                .edges_within(tx, &allow_all(), &ids)
                .await
                .expect("edges within")
        })
    })
    .await;

    assert_eq!(view, adapter, "the view's edge read is the adapter's");
    let mut expected: Vec<EntityEdge> = [(b, a), (c, a), (c, b)]
        .into_iter()
        .map(|(from_entity_id, to_entity_id)| EntityEdge {
            from_entity_id,
            to_entity_id,
        })
        .collect();
    expected.sort_unstable();
    assert_eq!(view, expected, "every internal edge of the diamond");
}

// ---------------------------------------------------------------------------
// A superseded outgoing set: where the two answers must differ
// ---------------------------------------------------------------------------

/// [`compare`], but the view's overlay has already superseded `from`'s outgoing
/// set with the empty one — an in-batch revision whose new document dropped every
/// `$ref`. The adapter is untouched, so the pair shows what the overlay changed.
async fn compare_with_replaced_edges<T, F>(db: &Provider, from: i64, ask: F) -> (T, T)
where
    T: Send + 'static,
    F: for<'a> Fn(
            &'a dyn Stores,
            &'a DbTx<'a>,
        ) -> std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    provider
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let adapter = stores();
                let view = AdmissionView::new(stores());
                view.replace_outgoing(tx, &allow_all(), from, &[])
                    .await
                    .expect("the overlay takes a replaced outgoing set");
                let from_adapter = ask(adapter.as_ref(), tx).await;
                let from_view = ask(&view, tx).await;
                Ok((from_adapter, from_view))
            })
        })
        .await
        .expect("the comparison reads must not fail")
}

/// Forward. `c`'s stored `$ref`s are the ones the batch's revision of `c`
/// removed, so the walk must stop at `c` rather than follow them. A view that
/// handed the roots to the adapter's recursive SQL would answer the adapter's
/// closure and predict a batch that no longer exists.
#[tokio::test]
async fn a_view_closure_does_not_follow_edges_the_batch_replaced() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let [.., c] = diamond_ids(&db).await;

    let (adapter, view) = compare_with_replaced_edges(&db, c, |stores, tx| {
        Box::pin(async move {
            names(
                &stores
                    .closure(tx, &allow_all(), &[C.to_owned()])
                    .await
                    .expect("closure")
                    .entities,
            )
        })
    })
    .await;

    assert_eq!(
        view,
        vec![C.to_owned()],
        "the revision dropped both `$ref`s, so `c` consumes nothing",
    );
    assert_eq!(
        adapter,
        vec![A.to_owned(), B.to_owned(), C.to_owned()],
        "the adapter still reads the stored edges, which is what makes the view's answer the overlay's",
    );
}

/// Reverse. `c -> a` is one of the removed edges, so `a`'s impact set loses `c`
/// and keeps `b`, whose own outgoing set nothing replaced.
#[tokio::test]
async fn a_view_reverse_impact_does_not_follow_edges_the_batch_replaced() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let [a, _, c] = diamond_ids(&db).await;

    let (adapter, view) = compare_with_replaced_edges(&db, c, move |stores, tx| {
        Box::pin(async move {
            reverse(
                &stores
                    .reverse_impact(tx, &allow_all(), &[a], 8)
                    .await
                    .expect("reverse impact"),
            )
        })
    })
    .await;

    assert_eq!(
        view,
        Ok(vec![B.to_owned()]),
        "`c` no longer depends on `a`, because this batch is the thing that changed that",
    );
    assert_eq!(
        adapter,
        Ok(vec![B.to_owned(), C.to_owned()]),
        "the stored relation still holds both",
    );
}

/// The deletion order's input. Both of `c`'s edges are gone, leaving the one
/// edge the batch did not touch.
#[tokio::test]
async fn a_view_edges_within_does_not_report_edges_the_batch_replaced() {
    let db = test_db().await;
    seed_diamond(&db).await;
    let ids = diamond_ids(&db).await;
    let [a, b, c] = ids;

    let (adapter, view) = compare_with_replaced_edges(&db, c, move |stores, tx| {
        Box::pin(async move {
            stores
                .edges_within(tx, &allow_all(), &ids)
                .await
                .expect("edges within")
        })
    })
    .await;

    assert_eq!(
        view,
        vec![EntityEdge {
            from_entity_id: b,
            to_entity_id: a,
        }],
        "only `b -> a` survives the batch's revision of `c`",
    );
    assert_eq!(
        adapter.len(),
        3,
        "the stored relation still holds all three: {adapter:?}",
    );
}

/// `HOLDERS * LEAVES` edges in a bipartite graph. One flat layer avoids the
/// exponential resolved-document growth of a fully connected DAG.
const LEAVES: usize = 32;
const HOLDERS: usize = 17;

fn leaf_id(index: usize) -> String {
    format!("gts.cf.core.dvw.leaf{index:02}.v1~")
}

fn holder_id(index: usize) -> String {
    format!("gts.cf.core.dvw.holder{index:02}.v1~")
}

/// Admit the bipartite set and return its entity ids.
async fn seed_dense(db: &Provider) -> Vec<i64> {
    for index in 0..LEAVES {
        let gts_id = leaf_id(index);
        admit(
            db,
            &format!("leaf-{index}"),
            &gts_id,
            referencing(&gts_id, &[]),
        )
        .await;
    }
    let targets: Vec<String> = (0..LEAVES).map(leaf_id).collect();
    let refs: Vec<&str> = targets.iter().map(String::as_str).collect();
    for index in 0..HOLDERS {
        let gts_id = holder_id(index);
        admit(
            db,
            &format!("holder-{index}"),
            &gts_id,
            referencing(&gts_id, &refs),
        )
        .await;
    }

    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let rows = provider
        .transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let ids: Vec<String> = (0..LEAVES)
                    .map(leaf_id)
                    .chain((0..HOLDERS).map(holder_id))
                    .collect();
                Ok(stores().find_by_gts_ids(tx, &allow_all(), &ids).await?)
            })
        })
        .await
        .expect("resolve the dense set");
    assert_eq!(rows.len(), LEAVES + HOLDERS, "every member is admitted");
    let mut ids: Vec<i64> = rows.into_iter().map(|row| row.id).collect();
    ids.sort_unstable();
    ids
}

/// 544 edges across 49 entities: deletion bounds endpoints, not edge count.
#[tokio::test]
async fn a_view_edges_within_matches_the_adapter_past_the_closure_bound() {
    let db = test_db().await;
    let ids = seed_dense(&db).await;
    let expected_pairs = HOLDERS * LEAVES;
    assert!(
        expected_pairs > 512,
        "the fixture must exceed the closure bound to test anything: {expected_pairs}",
    );

    let queried = ids.clone();
    let (adapter, view) = compare(&db, move |stores, tx| {
        let queried = queried.clone();
        Box::pin(async move {
            stores
                .edges_within(tx, &allow_all(), &queried)
                .await
                .map(|pairs| pairs.len())
                .map_err(|error| error.to_string())
        })
    })
    .await;

    assert_eq!(
        view,
        Ok(expected_pairs),
        "the dense set's internal edges, none of them refused",
    );
    assert_eq!(view, adapter, "the view's edge read is the adapter's");
}
