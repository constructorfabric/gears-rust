#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The same conformance suite against the built-in `PostgreSQL` store, plus the
//! cases that only exist against a real server: the SQL/PGQ hop, its parity
//! with the fallback hop, and the cross-tenant trap.
//!
//! The lane needs `PostgreSQL` 19 (`GRAPH_TABLE`) **with pgvector**, and those
//! two do not currently come in one platform-pinned image: the toolkit's
//! `postgres_graph()` pin is a stock `19beta3-alpine`, which has no pgvector,
//! while the gear's documented baseline requires it. Set
//! `GEARS_TEST_PG_GRAPH_IMAGE` to an image that carries both (see
//! `dev/DEVIATIONS.md` D-003) to run this lane; otherwise it skips — unless
//! `GEARS_TEST_PG_GRAPH_REQUIRED` is set, which turns a missing server into a
//! failure so CI cannot go green by silently running nothing.

mod conformance;

use std::sync::Arc;

use graph_storage::config::{GraphStorageConfig, HopStrategy};
use graph_storage::infra::engine::PgGraphEngine;
use graph_storage::infra::storage::migrations::Migrator;
use graph_storage::infra::store::PgGraphStore;
use graph_storage_sdk::models::{Direction, HopBudget, TruncationReason};
use graph_storage_sdk::plugin_api::{ExpandRequest, GraphEngineV1, GraphStoreV1};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

/// A live `PostgreSQL` 19 with the gear's schema and property graph applied.
struct Stand {
    store: Arc<PgGraphStore>,
    engine: PgGraphEngine,
    _container: ContainerAsync<Postgres>,
}

/// An image carrying `PostgreSQL` 19 **and** pgvector, when the operator names
/// one. Falls back to the platform pin, which starts but cannot serve this
/// gear's schema.
fn graph_image() -> Option<(String, String)> {
    let image = std::env::var("GEARS_TEST_PG_GRAPH_IMAGE").ok()?;
    let (name, tag) = image.rsplit_once(':').unwrap_or((image.as_str(), "latest"));
    Some((name.to_owned(), tag.to_owned()))
}

async fn stand(hop: HopStrategy) -> Option<Stand> {
    let started = match graph_image() {
        Some((name, tag)) => {
            cf_gears_test_containers::postgres_graph()
                .with_name(name)
                .with_tag(tag)
                .with_env_var("POSTGRES_PASSWORD", "pass")
                .with_env_var("POSTGRES_USER", "user")
                .with_env_var("POSTGRES_DB", "graph")
                .start()
                .await
        }
        None => {
            cf_gears_test_containers::postgres_graph()
                .with_env_var("POSTGRES_PASSWORD", "pass")
                .with_env_var("POSTGRES_USER", "user")
                .with_env_var("POSTGRES_DB", "graph")
                .start()
                .await
        }
    };

    let container = match started {
        Ok(container) => container,
        Err(error) => {
            assert!(
                !cf_gears_test_containers::graph_lane_required(),
                "GEARS_TEST_PG_GRAPH_REQUIRED is set but PostgreSQL 19 ({}) could not start: {error}",
                cf_gears_test_containers::postgres_graph_tag()
            );
            eprintln!("PostgreSQL 19 unavailable - skipping the SQL/PGQ lane: {error}");
            return None;
        }
    };

    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/graph");
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    if let Err(error) = run_migrations_for_testing(&db, Migrator::migrations()).await {
        // pgvector missing is the platform-pin gap, not a gear failure: say so
        // and skip, unless the lane was declared required.
        let message = error.to_string();
        assert!(
            !message.contains("extension \"vector\" is not available")
                || cf_gears_test_containers::graph_lane_required(),
            "migrations apply: {error}"
        );
        if message.contains("extension \"vector\" is not available") {
            eprintln!(
                "the graph image has no pgvector - skipping; set GEARS_TEST_PG_GRAPH_IMAGE \
                 to an image with PostgreSQL 19 and pgvector"
            );
            return None;
        }
        panic!("migrations apply: {error}");
    }

    let config = GraphStorageConfig {
        traversal_hop: hop,
        ..GraphStorageConfig::default()
    };
    let store = Arc::new(PgGraphStore::new(Arc::new(db), config, true));
    let engine = PgGraphEngine::new(Arc::clone(&store));
    Some(Stand {
        store,
        engine,
        _container: container,
    })
}

/// The store's tenants need their meta rows; `ensure_meta` is what boot does.
async fn tenant_on(stand: &Stand) -> Uuid {
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    graph_storage::infra::store::ingest::ensure_meta(&stand.store, tenant, &scope)
        .await
        .expect("meta rows exist");
    tenant
}

/// One conformance case against a live server: bring the stand up, mint a
/// tenant, run the shared case. The lane skips when `PostgreSQL` 19 is absent.
macro_rules! pg_case {
    ($name:ident, $case:path) => {
        #[tokio::test]
        async fn $name() {
            let Some(stand) = stand(HopStrategy::Pgq).await else {
                return;
            };
            let tenant = tenant_on(&stand).await;
            $case(stand.store.as_ref(), tenant).await;
        }
    };
}

// --- the shared obligations, against the real store -------------------------

pg_case!(a_failed_batch_commits_nothing, conformance::batch_atomicity);

pg_case!(
    source_generations_are_fenced_monotonically,
    conformance::generation_fencing
);

pg_case!(
    a_node_never_outlives_its_incident_edges,
    conformance::no_orphan_edges
);

pg_case!(a_recorded_idempotency_key_replays, conformance::idempotency);

pg_case!(an_identical_batch_converges, conformance::convergent_replay);

pg_case!(
    tombstoned_rows_are_invisible,
    conformance::tombstones_are_invisible
);

pg_case!(
    a_denied_row_reads_like_an_absent_one,
    conformance::denied_is_indistinguishable_from_absent
);

pg_case!(
    search_applies_the_scope_inside_the_statement,
    conformance::search_is_scoped
);

#[tokio::test]
async fn colliding_node_keys_stay_inside_their_tenants() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let one = tenant_on(&stand).await;
    let two = tenant_on(&stand).await;
    conformance::tenant_isolation(stand.store.as_ref(), one, two).await;
}

// --- what only a real PostgreSQL 19 can show --------------------------------

/// The property-graph DDL the migration executed is the DDL the declaration
/// generates: if `MATCH` and `CREATE PROPERTY GRAPH` could disagree, this hop
/// would not parse.
#[tokio::test]
async fn the_pattern_hop_walks_the_graph() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");
    stand
        .store
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("a", "a"),
                    conformance::node("b", "b"),
                    conformance::node("c", "c"),
                ],
                vec![conformance::edge("a", "b"), conformance::edge("b", "c")],
            ),
        )
        .await
        .expect("the batch commits");

    let ids = stand
        .store
        .resolve_node_ids(&ctx, &["a".to_owned()])
        .await
        .expect("resolution succeeds");
    let seed = ids.first().expect("`a` resolves").1;

    let response = stand
        .engine
        .expand(
            &ctx,
            ExpandRequest {
                frontier: vec![seed],
                direction: Direction::Outgoing,
                edge_types: None,
                labels: None,
                budget: HopBudget {
                    max_frontier: 100,
                    max_edges_scanned: 1_000,
                },
            },
        )
        .await
        .expect("the pattern hop runs");

    let reached: Vec<String> = response.edges.iter().map(|e| e.dst.clone()).collect();
    assert_eq!(reached, vec!["b".to_owned()], "one hop reaches exactly `b`");
    assert!(response.truncated.is_none());
}

/// Seed one stand and expand one hop, returning the reached ids and the
/// producer keys of the traversed edges.
async fn seed_and_expand(stand: &Stand, direction: Direction) -> (Vec<i64>, Vec<String>) {
    let tenant = tenant_on(stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");
    stand
        .store
        .ingest(
            &ctx,
            conformance::batch(
                vec![
                    conformance::node("hub", "hub"),
                    conformance::node("spoke-1", "one"),
                    conformance::node("spoke-2", "two"),
                ],
                vec![
                    conformance::edge("hub", "spoke-1"),
                    conformance::edge("spoke-2", "hub"),
                ],
            ),
        )
        .await
        .expect("the batch commits");

    let seed = stand
        .store
        .resolve_node_ids(&ctx, &["hub".to_owned()])
        .await
        .expect("resolution succeeds")
        .first()
        .expect("`hub` resolves")
        .1;

    let response = stand
        .engine
        .expand(
            &ctx,
            ExpandRequest {
                frontier: vec![seed],
                direction,
                edge_types: None,
                labels: None,
                budget: HopBudget {
                    max_frontier: 100,
                    max_edges_scanned: 1_000,
                },
            },
        )
        .await
        .expect("the hop runs");

    // Ids are per-stand surrogates, so compare the producer keys — the
    // only representation both stands share.
    let mut keys: Vec<String> = response
        .edges
        .iter()
        .flat_map(|e| [e.src.clone(), e.dst.clone()])
        .collect();
    keys.sort();
    (response.reached, keys)
}

/// The two backends must answer identically. Compared **at the seam**, not
/// through the API: an end-to-end comparison hid a backend returning its own
/// frontier for days on the prototype.
#[tokio::test]
async fn both_hop_backends_return_the_same_answer() {
    let Some(pgq) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let Some(two_query) = stand(HopStrategy::TwoQuery).await else {
        return;
    };

    for direction in [Direction::Outgoing, Direction::Incoming, Direction::Either] {
        let (pattern_reached, pattern_keys) = seed_and_expand(&pgq, direction).await;
        let (fallback_reached, fallback_keys) = seed_and_expand(&two_query, direction).await;
        assert_eq!(
            pattern_reached.len(),
            fallback_reached.len(),
            "the backends disagree on how many nodes {direction:?} reaches"
        );
        assert_eq!(
            pattern_keys, fallback_keys,
            "the backends disagree on the {direction:?} edges"
        );
    }
}

/// The cross-tenant trap. Two tenants own a node under the same key, and each
/// has its own edge; a hop that leaked would return the other tenant's key.
///
/// The precondition is asserted first: on the prototype this fixture went
/// missing and the guard test passed vacuously for days.
#[tokio::test]
async fn a_hop_never_leaves_its_tenant() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let ours = tenant_on(&stand).await;
    let theirs = tenant_on(&stand).await;

    for (tenant, far) in [(ours, "ours-far"), (theirs, "theirs-far")] {
        let scope = AccessScope::for_tenant(tenant);
        let ctx = conformance::ctx(tenant, &scope, None);
        stand
            .store
            .register_types(&ctx, conformance::ontology_batch())
            .await
            .expect("ontology registers");
        stand
            .store
            .ingest(
                &ctx,
                conformance::batch(
                    vec![
                        conformance::node("shared-key", "start"),
                        conformance::node(far, far),
                    ],
                    vec![conformance::edge("shared-key", far)],
                ),
            )
            .await
            .expect("the batch commits");
    }

    // Precondition: the trap exists on the other side.
    let their_scope = AccessScope::for_tenant(theirs);
    let their_ctx = conformance::ctx(theirs, &their_scope, None);
    let theirs_view = stand
        .store
        .get_node(&their_ctx, &"shared-key".to_owned(), 10)
        .await
        .expect("the other tenant owns the same key");
    assert_eq!(
        theirs_view.adjacency.len(),
        1,
        "the trap fixture must have an edge for the leak to expose"
    );

    let our_scope = AccessScope::for_tenant(ours);
    let our_ctx = conformance::ctx(ours, &our_scope, None);
    let seed = stand
        .store
        .resolve_node_ids(&our_ctx, &["shared-key".to_owned()])
        .await
        .expect("resolution succeeds")
        .first()
        .expect("our key resolves")
        .1;

    let response = stand
        .engine
        .expand(
            &our_ctx,
            ExpandRequest {
                frontier: vec![seed],
                direction: Direction::Either,
                edge_types: None,
                labels: None,
                budget: HopBudget {
                    max_frontier: 100,
                    max_edges_scanned: 1_000,
                },
            },
        )
        .await
        .expect("the hop runs");

    // Raw, non-deduplicated: a leak shows up as an extra edge, and dedup
    // would destroy exactly the signal this test exists to observe.
    let keys: Vec<String> = response.edges.iter().map(|e| e.dst.clone()).collect();
    assert_eq!(
        keys,
        vec!["ours-far".to_owned()],
        "the hop must reach only our own far node"
    );
}

/// A budget that stops a walk says so; truncation is never silent.
#[tokio::test]
async fn a_stopped_hop_reports_why() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");

    let response = stand
        .engine
        .expand(
            &ctx,
            ExpandRequest {
                frontier: vec![1, 2, 3],
                direction: Direction::Either,
                edge_types: None,
                labels: None,
                budget: HopBudget {
                    max_frontier: 1,
                    max_edges_scanned: 10,
                },
            },
        )
        .await
        .expect("the hop runs");
    assert_eq!(
        response.truncated,
        Some(TruncationReason::FrontierCap),
        "a frontier over the cap must be reported, not silently trimmed"
    );
}
