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
use graph_storage_sdk::plugin_api::{ExpandRequest, GraphEngineV1, GraphStoreV1, HopBackend};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::Db;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

/// A live `PostgreSQL` 19 with the gear's schema and property graph applied.
struct Stand {
    store: Arc<PgGraphStore>,
    engine: PgGraphEngine,
    db: Arc<Db>,
    /// Kept so a test can reach the server outside the secure ORM — dropping
    /// the property graph is operator surgery, not something a gear can do.
    dsn: String,
    _container: ContainerAsync<Postgres>,
}

/// Remove the property graph, leaving the tables. This is what a gear sees on
/// a server whose major cannot create one.
async fn drop_property_graph(stand: &Stand) {
    use sea_orm::ConnectionTrait as _;
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for operator surgery");
    raw.execute_unprepared("DROP PROPERTY GRAPH kb")
        .await
        .expect("the property graph is dropped");
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
    let db = Arc::new(db);
    // The stand probes exactly as the gear's composition root does, so a
    // change to the probe is exercised by every case here.
    let pgq_available = graph_storage::infra::engine::probe_pgq(&db).await;
    let store = Arc::new(PgGraphStore::new(Arc::clone(&db), config, pgq_available));
    let engine = PgGraphEngine::new(Arc::clone(&store));
    Some(Stand {
        store,
        engine,
        db,
        dsn,
        _container: container,
    })
}

/// The store's tenants need their meta rows; `ensure_meta` is what boot does.
async fn tenant_on(stand: &Stand) -> Uuid {
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    graph_storage::infra::store::ingest::ensure_meta(stand.store.as_ref(), tenant, &scope)
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

pg_case!(
    a_fresh_tenant_reports_a_usable_revision,
    conformance::a_fresh_tenant_reports_a_usable_revision
);

pg_case!(
    materializing_a_phantom_revalidates_its_edges,
    conformance::materializing_a_phantom_revalidates_its_edges
);

pg_case!(
    an_edge_type_refuses_an_endpoint_it_does_not_admit,
    conformance::endpoint_constraints_are_enforced
);

pg_case!(a_recorded_idempotency_key_replays, conformance::idempotency);
pg_case!(
    a_document_is_retrieved_by_its_own_text,
    conformance::a_document_is_retrieved_by_its_own_text
);
pg_case!(
    a_declared_path_reaches_the_vector,
    conformance::a_declared_path_reaches_the_vector
);
pg_case!(
    a_skipped_re_ingest_preserves_the_vector,
    conformance::a_skipped_re_ingest_preserves_the_vector
);
pg_case!(
    a_stale_vector_stops_ranking_but_the_node_stays,
    conformance::a_stale_vector_stops_ranking_but_the_node_stays
);
pg_case!(
    only_the_active_epoch_ranks,
    conformance::only_the_active_epoch_ranks
);

pg_case!(an_identical_batch_converges, conformance::convergent_replay);
pg_case!(
    an_unchanged_re_ingest_embeds_nothing,
    conformance::an_unchanged_re_ingest_embeds_nothing
);

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

pg_case!(
    the_envelope_records_the_subject_of_each_verb,
    conformance::the_envelope_records_the_subject_of_each_verb
);

pg_case!(
    a_projection_row_carries_the_envelope,
    conformance::a_projection_row_carries_the_envelope
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
    conformance::ingest_batch(
        stand.store.as_ref(),
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

    // Asserted first, and deliberately: the pattern backend declines by
    // falling back, so a hop that never executed returns the *right answer*
    // from the two-query backend. Without this line this test passes while
    // testing nothing it claims to test -- which is how a pattern that lost
    // its anchor and failed on every request went unnoticed.
    assert_eq!(
        response.served_by,
        HopBackend::Pattern,
        "the single-statement pattern must be what answered, not the fallback"
    );
    let reached: Vec<String> = response.edges.iter().map(|e| e.dst.clone()).collect();
    assert_eq!(reached, vec!["b".to_owned()], "one hop reaches exactly `b`");
    assert!(response.truncated.is_none());
}

/// Seed one stand and expand one hop, returning the reached ids, the producer
/// keys of the traversed edges, and which backend actually answered.
///
/// The last of the three is not decoration. The pattern backend declines by
/// falling back, so a comparison of "the two backends" run against a stand
/// whose pattern silently failed compares the fallback with itself and agrees
/// perfectly.
async fn seed_and_expand(
    stand: &Stand,
    direction: Direction,
) -> (Vec<i64>, Vec<String>, HopBackend) {
    let tenant = tenant_on(stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");
    conformance::ingest_batch(
        stand.store.as_ref(),
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
    (response.reached, keys, response.served_by)
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
        let (pattern_reached, pattern_keys, pattern_backend) =
            seed_and_expand(&pgq, direction).await;
        let (fallback_reached, fallback_keys, fallback_backend) =
            seed_and_expand(&two_query, direction).await;
        assert_eq!(
            (pattern_backend, fallback_backend),
            (HopBackend::Pattern, HopBackend::TwoQuery),
            "the {direction:?} comparison must be between two different backends"
        );
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
        conformance::ingest_batch(
            stand.store.as_ref(),
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

/// The configuration ADR-0001 calls the baseline: a server with no SQL/PGQ.
///
/// `PostgreSQL` 16 has no `GRAPH_TABLE`, so the conditional migration emits no
/// property graph and the gear must serve every hop on the fallback backend
/// "with no functional difference to the caller". Dropping the property graph
/// on a PG19 stand reproduces exactly that condition — the capability is
/// absent — without needing a second image, and it is the condition the gear
/// got wrong: it attempted a pattern per request and answered `500` on a
/// configuration the specification supports.
#[tokio::test]
async fn traversal_answers_on_a_server_without_the_property_graph() {
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
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![conformance::node("p-a", "a"), conformance::node("p-b", "b")],
            vec![conformance::edge("p-a", "p-b")],
        ),
    )
    .await
    .expect("the batch commits");

    let seed = stand
        .store
        .resolve_node_ids(&ctx, &["p-a".to_owned()])
        .await
        .expect("resolution succeeds")
        .first()
        .map_or_else(|| panic!("`p-a` resolves"), |(_, id)| *id);

    // Precondition: with the property graph present, the probe says so and the
    // pattern hop is what answers. Without this the test could pass on a stand
    // that never had SQL/PGQ at all, proving nothing.
    assert!(
        graph_storage::infra::engine::probe_pgq(stand.store.db()).await,
        "the stand must start with a working property graph for this test to mean anything"
    );

    drop_property_graph(&stand).await;

    assert!(
        !graph_storage::infra::engine::probe_pgq(stand.store.db()).await,
        "the probe must report the capability as absent once the graph is gone"
    );

    // The engine was built while the capability was present, so this exercises
    // the per-request path: the pattern fails, and the hop falls back rather
    // than failing the request.
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
        .expect("a server without SQL/PGQ must still answer, on the fallback backend");

    let reached: Vec<String> = response.edges.iter().map(|e| e.dst.clone()).collect();
    assert_eq!(
        reached,
        vec!["p-b".to_owned()],
        "the fallback backend answers the same question the pattern would have"
    );

    // And an engine constructed *after* the capability vanished never attempts
    // the pattern at all — the probe is what init uses.
    let unprobed = PgGraphEngine::new(Arc::new(PgGraphStore::new(
        Arc::clone(&stand.db),
        GraphStorageConfig {
            traversal_hop: HopStrategy::Pgq,
            ..GraphStorageConfig::default()
        },
        graph_storage::infra::engine::probe_pgq(stand.store.db()).await,
    )));
    let again = unprobed
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
        .expect("the fallback backend answers");
    assert_eq!(again.edges.len(), 1);
}

/// What `StoreCapabilities::snapshots = false` actually means here.
///
/// DESIGN § 3.3 obligation 5 asks that every arm of one compound read observe
/// one graph state. The built-in store declares the capability absent, and
/// this is the observable consequence: a row committed after `begin_read`
/// **is** visible to a call carrying that snapshot. The obligation is declined
/// rather than approximated, which is what the capability mechanism is for —
/// but "declined" is a claim worth holding to an assertion instead of a
/// comment, so that a future change which quietly starts honouring it, or
/// quietly makes it worse, shows up here.
#[tokio::test]
async fn the_built_in_store_declines_the_snapshot_obligation() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    assert!(
        !stand.store.capabilities().snapshots,
        "the store must declare the capability absent rather than claim it"
    );

    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("ontology registers");
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(vec![conformance::node("snap-before", "before")], Vec::new()),
    )
    .await
    .expect("the first batch commits");

    let snapshot = stand.store.begin_read(&ctx).await.expect("snapshot opens");

    // A concurrent commit, landing between the arms of the compound read.
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(vec![conformance::node("snap-after", "after")], Vec::new()),
    )
    .await
    .expect("the concurrent batch commits");

    let under = conformance::ctx(tenant, &scope, Some(&snapshot));
    let seen = stand
        .store
        .resolve_node_ids(&under, &["snap-after".to_owned()])
        .await
        .expect("resolution succeeds");

    assert!(
        !seen.is_empty(),
        "the built-in store does not isolate a compound read: this asserts the \
         *absence* of isolation, so if it ever starts isolating, revisit \
         StoreCapabilities::snapshots and DESIGN section 3.3 together"
    );
    assert_eq!(
        snapshot.revision.revision + 1,
        stand
            .store
            .revision(&ctx)
            .await
            .expect("revision reads")
            .revision,
        "the snapshot recorded the revision it opened at, even though it does \
         not hold it"
    );

    stand
        .store
        .end_read(snapshot)
        .await
        .expect("snapshot closes");
}
