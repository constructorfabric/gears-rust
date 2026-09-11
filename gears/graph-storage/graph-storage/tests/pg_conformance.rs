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
//! the README) to run this lane; otherwise it skips — unless
//! `GEARS_TEST_PG_GRAPH_REQUIRED` is set, which turns a missing server into a
//! failure so CI cannot go green by silently running nothing.
//!
//! **Every case gets its own server**, because two of them are operator
//! surgery on server-wide state (dropping the property graph, re-resolving the
//! embedding space at boot) and a shared instance would make them poison the
//! rest. The cost is one container per case, which on an ordinary machine is
//! more than Docker and `PostgreSQL` will take at once: at eight in parallel
//! the connection pools time out (`PoolTimedOut`) and a *different* case fails
//! on each run — which reads as flakiness in the gear and is contention on the
//! host. `STAND_PERMITS` bounds it for the in-process test runner; `nextest`
//! runs each case in its own process, so there the bound is
//! `--test-threads` (see the `test-graph-storage-pg` target).

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

/// How many stands may exist at once under the in-process runner.
///
/// Two is deliberately conservative: measured on an 8-core, 23 GiB machine
/// with a development stand already running, eight concurrent stands fail a
/// different case on every run, four still fail about half the time, and two
/// have not failed. A host with memory to spare raises it with
/// `GEARS_TEST_PG_GRAPH_STANDS`. The cost of the conservative default is
/// wall-clock on one lane; the cost of the optimistic one is a suite that
/// cries wolf, which is worse.
static STAND_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);

/// Give the semaphore its permits once, from the environment or the default.
fn stand_permits() -> &'static tokio::sync::Semaphore {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let permits = std::env::var("GEARS_TEST_PG_GRAPH_STANDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|permits| *permits > 0)
            .unwrap_or(2);
        STAND_PERMITS.add_permits(permits);
    });
    &STAND_PERMITS
}

/// The container's mapped port, waited for rather than demanded.
///
/// A container that has just been started may not have published its port
/// yet, and under load the gap is wide enough to see (`PortNotExposed`).
async fn mapped_port(container: &ContainerAsync<Postgres>) -> u16 {
    let mut last = None;
    for _ in 0..20 {
        match container.get_host_port_ipv4(5432).await {
            Ok(port) => return port,
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    panic!("the container never published its port: {last:?}");
}

/// Connect, allowing the server a moment to finish coming up.
///
/// A server whose process is running is not yet a server that answers: the
/// first connections to a fresh instance can time out or meet a half-open
/// socket (`unexpected response from SSLRequest`). Retrying a connection to a
/// server that is still starting is what any client does; it is not papering
/// over a gear failure, and the assertion still fails if the server never
/// arrives.
async fn connect_with_retry(dsn: &str) -> Db {
    let opts = || ConnectOpts {
        max_conns: Some(4),
        min_conns: Some(1),
        ..Default::default()
    };
    let mut last = None;
    for _ in 0..15 {
        match connect_db(dsn, opts()).await {
            Ok(db) => return db,
            Err(error) => {
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
    }
    panic!("the server never accepted a connection: {last:?}");
}

/// A live `PostgreSQL` 19 with the gear's schema and property graph applied.
struct Stand {
    store: Arc<PgGraphStore>,
    engine: PgGraphEngine,
    db: Arc<Db>,
    /// Kept so a test can reach the server outside the secure ORM — dropping
    /// the property graph is operator surgery, not something a gear can do.
    dsn: String,
    _container: ContainerAsync<Postgres>,
    /// Held for the case's lifetime: the stand is the scarce resource, not
    /// its startup, so the permit is released when the stand is dropped.
    _permit: tokio::sync::SemaphorePermit<'static>,
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
    let permit = stand_permits()
        .acquire()
        .await
        .expect("the stand semaphore is never closed");
    let started = match graph_image() {
        Some((name, tag)) => {
            test_containers::postgres_graph()
                .with_name(name)
                .with_tag(tag)
                .with_env_var("POSTGRES_PASSWORD", "pass")
                .with_env_var("POSTGRES_USER", "user")
                .with_env_var("POSTGRES_DB", "graph")
                .start()
                .await
        }
        None => {
            test_containers::postgres_graph()
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
                !test_containers::graph_lane_required(),
                "GEARS_TEST_PG_GRAPH_REQUIRED is set but PostgreSQL 19 ({}) could not start: {error}",
                test_containers::postgres_graph_tag()
            );
            eprintln!("PostgreSQL 19 unavailable - skipping the SQL/PGQ lane: {error}");
            return None;
        }
    };

    let port = mapped_port(&container).await;
    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/graph");
    let db = connect_with_retry(&dsn).await;

    if let Err(error) = run_migrations_for_testing(&db, Migrator::migrations()).await {
        // pgvector missing is the platform-pin gap, not a gear failure: say so
        // and skip, unless the lane was declared required — then a stock image
        // is a failure, because a lane that skips itself proves nothing.
        let no_pgvector = error
            .to_string()
            .contains("extension \"vector\" is not available");
        assert!(
            !(no_pgvector && test_containers::graph_lane_required()),
            "GEARS_TEST_PG_GRAPH_REQUIRED is set but the graph image has no pgvector: {error}"
        );
        if no_pgvector {
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
        // A deployment mirroring a domain hierarchy raises the chain ceiling;
        // the suite's deep-chain case needs the raised posture, and nothing
        // else in the suite is sensitive to it.
        ontology_max_chain_depth: 8,
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
        _permit: permit,
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
    a_same_key_ingest_may_not_change_the_type,
    conformance::a_same_key_ingest_may_not_change_the_type
);
pg_case!(
    per_item_outcomes_follow_the_batch_order,
    conformance::per_item_outcomes_follow_the_batch_order
);
pg_case!(
    a_scope_and_an_idempotency_key_belong_to_their_producer,
    conformance::a_scope_and_an_idempotency_key_belong_to_their_producer
);
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

// --- payload projection -----------------------------------------------------

pg_case!(
    a_declared_payload_path_filters_and_orders_the_projection,
    conformance::a_declared_payload_path_filters_and_orders_the_projection
);
pg_case!(
    an_undeclared_payload_path_is_refused_naming_the_alternatives,
    conformance::an_undeclared_payload_path_is_refused_naming_the_alternatives
);
pg_case!(
    an_index_path_onto_a_non_scalar_is_refused_at_registration,
    conformance::an_index_path_onto_a_non_scalar_is_refused_at_registration
);
pg_case!(
    a_deeper_chain_registers_and_its_ancestor_admits_the_leaf,
    conformance::a_deeper_chain_registers_and_its_ancestor_admits_the_leaf
);

/// Readiness against a real server: the database row is healthy because the
/// migrations ran, and the SQL/PGQ row reports what this server could provide.
#[tokio::test]
async fn readiness_reports_every_capability_and_only_some_block_service() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    conformance::readiness_reports_every_capability_and_only_some_block_service(
        stand.store.as_ref(),
    )
    .await;
}

// --- scope replacement --------------------------------------------------------

/// Written out rather than `pg_case!`d: the two replacements have to run on
/// real threads, or the race the obligation is about never happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_replacements_of_one_scope_serialize() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    conformance::two_replacements_of_one_scope_serialize(stand.store.as_ref(), Uuid::now_v7())
        .await;
}

pg_case!(
    scope_replacement_removes_what_the_batch_no_longer_names,
    conformance::scope_replacement_removes_what_the_batch_no_longer_names
);
pg_case!(
    scope_replacement_preserves_analysis_edges_and_their_endpoints,
    conformance::scope_replacement_preserves_analysis_edges_and_their_endpoints
);

// --- source-namespace ownership ----------------------------------------------

pg_case!(
    a_source_namespace_is_claimed_by_its_first_writer,
    conformance::a_source_namespace_is_claimed_by_its_first_writer
);
pg_case!(
    writing_under_another_producers_namespace_is_forbidden,
    conformance::writing_under_another_producers_namespace_is_forbidden
);
pg_case!(
    a_transfer_moves_the_namespace_and_records_who_moved_it,
    conformance::a_transfer_moves_the_namespace_and_records_who_moved_it
);
pg_case!(
    an_owned_nodes_source_field_claims_no_namespace,
    conformance::an_owned_nodes_source_field_claims_no_namespace
);

// --- type evolution (registering a changed schema in place) -----------------

pg_case!(
    a_backward_compatible_change_updates_the_type_in_place,
    conformance::a_backward_compatible_change_updates_the_type_in_place
);
pg_case!(
    an_incompatible_change_is_refused_with_its_location,
    conformance::an_incompatible_change_is_refused_with_its_location
);
pg_case!(
    a_changed_schema_is_still_a_conflict_by_default,
    conformance::a_changed_schema_is_still_a_conflict_by_default
);
pg_case!(
    a_dry_run_reports_every_verdict_and_writes_nothing,
    conformance::a_dry_run_reports_every_verdict_and_writes_nothing
);
pg_case!(
    a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit,
    conformance::a_change_the_schemas_cannot_prove_is_admitted_when_the_rows_fit
);
pg_case!(
    a_change_the_stored_rows_contradict_is_refused_naming_them,
    conformance::a_change_the_stored_rows_contradict_is_refused_naming_them
);
pg_case!(
    a_migration_moves_the_data_with_the_type,
    conformance::a_migration_moves_the_data_with_the_type
);
pg_case!(
    a_migration_that_leaves_rows_invalid_is_refused_naming_them,
    conformance::a_migration_that_leaves_rows_invalid_is_refused_naming_them
);
pg_case!(
    a_migration_without_a_schema_change_is_refused,
    conformance::a_migration_without_a_schema_change_is_refused
);
pg_case!(
    a_migration_stamps_its_writer_and_moves_the_version,
    conformance::a_migration_stamps_its_writer_and_moves_the_version
);
pg_case!(
    an_accepted_type_update_advances_the_graph_revision,
    conformance::an_accepted_type_update_advances_the_graph_revision
);
pg_case!(
    a_new_index_path_becomes_filterable_without_recreating_the_type,
    conformance::a_new_index_path_becomes_filterable_without_recreating_the_type
);

/// Keyset paging over a payload ordering, which only the built-in store
/// serves: every page continues where the last one ended, the ordered walk
/// is the same as the one-page answer, and the rows missing the attribute
/// come last.
#[tokio::test]
async fn a_payload_ordered_projection_pages_by_keyset() {
    use toolkit_odata::SortDir;
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let store = stand.store.as_ref();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    let whole = store
        .project_table(
            &ctx,
            conformance::projection_seeded(store, &ctx, &[("payload/score", SortDir::Desc)]).await,
        )
        .await
        .expect("projection succeeds");
    let expected: Vec<String> = whole.items.iter().map(|r| r.node_key.clone()).collect();
    assert_eq!(expected, vec!["t1", "t5", "t2", "t3", "t4"]);

    let mut walked = Vec::new();
    let mut request = conformance::projection(
        &[conformance::INDEXED],
        "",
        &[("payload/score", SortDir::Desc)],
    );
    request.query = request.query.with_limit(2);
    loop {
        let page = store
            .project_table(&ctx, request.clone())
            .await
            .expect("a page is served");
        assert!(page.items.len() <= 2, "the page honours its limit");
        walked.extend(page.items.iter().map(|r| r.node_key.clone()));
        let Some(token) = page.page_info.next_cursor else {
            break;
        };
        let cursor = toolkit_odata::CursorV1::decode(&token).expect("a CursorV1 token");
        assert_eq!(cursor.s, "-payload/score,+node_key");
        request.query = toolkit_odata::ODataQuery::new()
            .with_limit(2)
            .with_cursor(cursor);
        assert!(walked.len() <= 5, "the walk terminates");
    }
    assert_eq!(walked, expected, "pages concatenate to the one-page answer");
}

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
                with_degrees: false,
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
                with_degrees: false,
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
                with_degrees: false,
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
                with_degrees: false,
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

/// The edge-scan budget is a bound *and* a report.
///
/// It was neither: `live_edges` passed the budget to `LIMIT` and nothing
/// compared what came back against it, so `TruncationReason::EdgeScanCap`
/// existed in the vocabulary, was rendered by the DTO, and was produced by no
/// code path. A hop over a dense region returned a partial subgraph that a
/// caller could not tell from a complete one -- the failure mode the type's own
/// "never silent" comment forbids, and the same shape as the traversal
/// backend that fell back in silence.
///
/// Run on both backends, because they build their answer differently and each
/// has to reach the same conclusion about its own scan.
#[tokio::test]
async fn a_hop_that_hits_its_edge_budget_reports_it_on_both_backends() {
    for hop in [HopStrategy::Pgq, HopStrategy::TwoQuery] {
        let Some(stand) = stand(hop).await else {
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

        // A hub with six edges, so a budget of three is inside it.
        let mut nodes = vec![conformance::node("hub", "hub")];
        let mut edges = Vec::new();
        for index in 0..6 {
            let key = format!("spoke-{index}");
            nodes.push(conformance::node(&key, &key));
            edges.push(conformance::edge("hub", &key));
        }
        conformance::ingest_batch(stand.store.as_ref(), &ctx, conformance::batch(nodes, edges))
            .await
            .expect("the hub commits");

        let ids = stand
            .store
            .resolve_node_ids(&ctx, &["hub".to_owned()])
            .await
            .expect("the hub resolves");
        let frontier: Vec<_> = ids.into_iter().map(|(_, id)| id).collect();

        let request = |max_edges_scanned| ExpandRequest {
            frontier: frontier.clone(),
            direction: Direction::Either,
            edge_types: None,
            labels: None,
            budget: HopBudget {
                max_frontier: 1_000,
                max_edges_scanned,
            },
            with_degrees: false,
        };

        let cut = stand
            .engine
            .expand(&ctx, request(3))
            .await
            .expect("the hop runs");
        assert_eq!(
            cut.truncated,
            Some(TruncationReason::EdgeScanCap),
            "{hop:?}: a scan stopped by the edge budget must say so"
        );
        assert_eq!(
            cut.edges.len(),
            3,
            "{hop:?}: the budget still bounds the work"
        );

        // The same hop inside its budget is not truncated, otherwise the
        // assertion above would pass on a hop that reports the cap always.
        let whole = stand
            .engine
            .expand(&ctx, request(100))
            .await
            .expect("the hop runs");
        assert_eq!(whole.truncated, None, "{hop:?}: an unbounded hop is whole");
        assert_eq!(whole.edges.len(), 6, "{hop:?}: every edge of the hub");
    }
}

/// The degree a neighborhood ranks by is the neighbour's own connectivity in
/// the authorized subgraph, and it is opt-in.
///
/// Both halves matter. The count has to be the node's *own* degree — at depth
/// one every neighbour is tied to the frontier by exactly one edge, so a
/// within-hop count would rank a hub's neighbours arbitrarily, which is the
/// failure `fr-neighborhood-projection` exists to prevent. And it has to be
/// opt-in, because it is a second scoped read that a traversal has no use
/// for.
#[tokio::test]
async fn a_hop_reports_the_reached_nodes_degree_only_when_asked() {
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

    // `core` carries two edges of its own; `leaf` carries only the one that
    // ties it to the root.
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("root", "root"),
                conformance::node("core", "core"),
                conformance::node("leaf", "leaf"),
                conformance::node("far-1", "far-1"),
                conformance::node("far-2", "far-2"),
            ],
            vec![
                conformance::edge("root", "core"),
                conformance::edge("root", "leaf"),
                conformance::edge("core", "far-1"),
                conformance::edge("core", "far-2"),
            ],
        ),
    )
    .await
    .expect("the fixture commits");

    let ids = stand
        .store
        .resolve_node_ids(&ctx, &["root".to_owned(), "core".to_owned()])
        .await
        .expect("the root resolves");
    let by_key: std::collections::BTreeMap<String, i64> = ids.into_iter().collect();
    let frontier = vec![by_key["root"]];

    let request = |with_degrees| ExpandRequest {
        frontier: frontier.clone(),
        direction: Direction::Either,
        edge_types: None,
        labels: None,
        budget: HopBudget {
            max_frontier: 1_000,
            max_edges_scanned: 1_000,
        },
        with_degrees,
    };

    let silent = stand
        .engine
        .expand(&ctx, request(false))
        .await
        .expect("the hop runs");
    assert!(
        silent.degrees.is_empty(),
        "a hop that was not asked for degrees does not pay for them"
    );

    let ranked = stand
        .engine
        .expand(&ctx, request(true))
        .await
        .expect("the hop runs");
    assert_eq!(ranked.degrees.len(), ranked.reached.len(), "index-aligned");
    let degree_of_core = ranked
        .reached
        .iter()
        .zip(&ranked.degrees)
        .find(|(id, _)| **id == by_key["core"])
        .map(|(_, degree)| *degree);
    assert_eq!(
        degree_of_core,
        Some(3),
        "`core` has three live edges: one to the root and two of its own"
    );
    let leaf_degree = ranked
        .reached
        .iter()
        .zip(&ranked.degrees)
        .filter(|(id, _)| **id != by_key["core"])
        .map(|(_, degree)| *degree)
        .collect::<Vec<_>>();
    assert_eq!(
        leaf_degree,
        vec![1],
        "the leaf has only the edge that reached it"
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
                with_degrees: false,
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
                with_degrees: false,
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

// --- both families, and the edge read -----------------------------------------

pg_case!(
    both_node_families_and_both_edge_families_round_trip,
    conformance::both_node_families_and_both_edge_families_round_trip
);
pg_case!(
    an_edge_read_carries_the_envelope,
    conformance::an_edge_read_carries_the_envelope
);

#[tokio::test]
async fn an_edge_whose_endpoint_is_hidden_is_not_readable() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let one = tenant_on(&stand).await;
    let two = tenant_on(&stand).await;
    conformance::an_edge_whose_endpoint_is_hidden_is_not_readable(stand.store.as_ref(), one, two)
        .await;
}

/// The adversarial sweep: one trap fixture, every read surface the store port
/// exposes, against a real server.
#[tokio::test]
async fn no_read_surface_answers_with_another_tenants_rows() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let one = tenant_on(&stand).await;
    let two = tenant_on(&stand).await;
    conformance::no_read_surface_answers_with_another_tenants_rows(stand.store.as_ref(), one, two)
        .await;
}

// --- boot-time embedding-space resolution -----------------------------------

use graph_storage::infra::store::spaces::{SpaceResolution, resolve};

fn space(model: &str) -> graph_storage_sdk::models::EmbeddingSpaceId {
    graph_storage_sdk::models::EmbeddingSpaceId::new(
        model,
        "tokenizer-v1",
        serde_json::json!({"lowercase": true}),
        serde_json::json!({"mode": "mean"}),
        serde_json::json!({"l2": true}),
        8,
    )
}

/// What boot decides about the deployment's vectors (ADR-0005).
///
/// Only the gear's composition root calls this, which is why it had no test:
/// the decision that matters most on an upgrade -- a provider that is not the
/// one the stored vectors came from -- was made by code nothing exercised.
#[tokio::test]
async fn a_second_boot_with_another_provider_reports_a_mismatch_rather_than_opening_an_epoch() {
    let Some(stand) = stand(HopStrategy::Pgq).await else {
        return;
    };
    let first = resolve(&stand.db, &space("model-a"))
        .await
        .expect("the first boot resolves");
    let SpaceResolution::Active { epoch } = first else {
        panic!("a first boot adopts the provider's own space, got {first:?}");
    };

    assert_eq!(
        resolve(&stand.db, &space("model-a"))
            .await
            .expect("a same-provider boot resolves"),
        SpaceResolution::Active { epoch },
        "the same provider adopts the recorded epoch rather than opening another"
    );

    // The upgrade case. Opening a new epoch here would strand every stored
    // vector in a space nothing searches -- invisible corruption, which is
    // what the mismatch exists to refuse.
    match resolve(&stand.db, &space("model-b"))
        .await
        .expect("a different-provider boot still resolves")
    {
        SpaceResolution::Mismatched {
            recorded_identity,
            recorded_epoch,
        } => {
            assert_eq!(recorded_epoch, epoch);
            assert_eq!(recorded_identity, space("model-a").identity_hash);
        }
        other @ SpaceResolution::Active { .. } => {
            panic!("a different provider must not be adopted silently, got {other:?}")
        }
    }
}

pg_case!(
    an_edge_type_evolves_over_its_own_rows,
    conformance::an_edge_type_evolves_over_its_own_rows
);
