#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The same conformance suite against the built-in `PostgreSQL` store, plus the
//! cases that only exist against a real server: the plans the read paths can
//! use, the cross-tenant trap, and the migrations.
//!
//! The lane needs `PostgreSQL` 16 or later **with pgvector**, which the
//! platform's `postgres_graph()` pin does not carry. Set
//! `GEARS_TEST_PG_GRAPH_IMAGE` to such an image — CI uses
//! `pgvector/pgvector:0.8.7-pg18` — to run this lane; otherwise it skips, unless
//! `GEARS_TEST_PG_GRAPH_REQUIRED` is set, which turns a missing server into a
//! failure so CI cannot go green by silently running nothing.
//!
//! **Every case gets its own server**, because some of them are operator
//! surgery on server-wide state (re-resolving the embedding space at boot,
//! creating the retired property graph for `m0010` to drop) and a shared instance would make them poison the
//! rest. The cost is one container per case, which on an ordinary machine is
//! more than Docker and `PostgreSQL` will take at once: at eight in parallel
//! the connection pools time out (`PoolTimedOut`) and a *different* case fails
//! on each run — which reads as flakiness in the gear and is contention on the
//! host. `STAND_PERMITS` bounds it for the in-process test runner; `nextest`
//! runs each case in its own process, so there the bound is
//! `--test-threads` (see the `test-graph-storage-pg` target).

mod conformance;
mod support;

use std::sync::Arc;

use graph_storage::config::GraphStorageConfig;
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

/// One attempt at a `PostgreSQL` 19 container: the operator's image when they
/// named one, the platform pin otherwise.
async fn start_server() -> Result<ContainerAsync<Postgres>, testcontainers::TestcontainersError> {
    let request = match graph_image() {
        Some((name, tag)) => test_containers::postgres_graph()
            .with_name(name)
            .with_tag(tag),
        None => test_containers::postgres_graph(),
    };
    request
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "graph")
        .start()
        .await
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

/// A live `PostgreSQL` with pgvector and the gear's schema applied.
struct Stand {
    store: Arc<PgGraphStore>,
    engine: PgGraphEngine,
    db: Arc<Db>,
    /// Kept so a test can reach the server outside the secure ORM — reading
    /// plans and the catalog is operator surgery, not something a gear can do.
    dsn: String,
    _container: ContainerAsync<Postgres>,
    /// Held for the case's lifetime: the stand is the scarce resource, not
    /// its startup, so the permit is released when the stand is dropped.
    _permit: tokio::sync::SemaphorePermit<'static>,
}

/// An image carrying `PostgreSQL` 19 **and** pgvector, when the operator names
/// one. Falls back to the platform pin, which starts but cannot serve this
/// gear's schema.
fn graph_image() -> Option<(String, String)> {
    let image = std::env::var("GEARS_TEST_PG_GRAPH_IMAGE").ok()?;
    let (name, tag) = image.rsplit_once(':').unwrap_or((image.as_str(), "latest"));
    Some((name.to_owned(), tag.to_owned()))
}

async fn stand() -> Option<Stand> {
    // Decide the skip before starting anything. The platform pin is a stock
    // image with no pgvector, so with no image configured this lane can only
    // end in a skip — and paying for a container per case first is how a
    // coverage run over the db-free lanes came to spend ten minutes starting
    // servers it was about to throw away. When the lane is *required* the
    // attempt still happens, so a future pin that does carry pgvector needs
    // no change here.
    if graph_image().is_none() && !test_containers::graph_lane_required() {
        eprintln!(
            "GEARS_TEST_PG_GRAPH_IMAGE is unset and the platform pin ({}) has no pgvector - \
             skipping the PostgreSQL lane",
            test_containers::postgres_graph_tag()
        );
        return None;
    }
    let permit = stand_permits()
        .acquire()
        .await
        .expect("the stand semaphore is never closed");
    // Starting a container is itself a resource request the host can refuse
    // under load, so a refusal is retried before it is believed: the
    // alternative is a case that fails for the daemon's reasons and reads as
    // the gear's.
    let mut started = start_server().await;
    for _ in 0..4 {
        if started.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        started = start_server().await;
    }

    let container = match started {
        Ok(container) => container,
        Err(error) => {
            assert!(
                !test_containers::graph_lane_required(),
                "GEARS_TEST_PG_GRAPH_REQUIRED is set but the graph image ({}) could not start: {error}",
                test_containers::postgres_graph_tag()
            );
            eprintln!("the graph image is unavailable - skipping the PostgreSQL lane: {error}");
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
                 to an image with PostgreSQL 16+ and pgvector"
            );
            return None;
        }
        panic!("migrations apply: {error}");
    }

    let config = GraphStorageConfig {
        // A deployment mirroring a domain hierarchy raises the chain ceiling;
        // the suite's deep-chain case needs the raised posture, and nothing
        // else in the suite is sensitive to it.
        ontology_max_chain_depth: 8,
        ..GraphStorageConfig::default()
    };
    let db = Arc::new(db);
    let store = Arc::new(PgGraphStore::new(
        Arc::clone(&db),
        config.validated().expect("the test configuration is valid"),
    ));
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

/// A second pool against the same server, with session parameters of its own.
///
/// The DSN is where a gear can reach `PostgreSQL`'s runtime parameters at all:
/// `DBRunner` exposes no statement surface, so `SET LOCAL` is unavailable to
/// gear code (gears-rust #4871), and `ConnectOpts` carries pool settings only.
/// A deployment sets the same things through `params:` in its database
/// configuration, which toolkit-db forwards to the connection verbatim.
async fn store_with(stand: &Stand, options: &str) -> Arc<PgGraphStore> {
    let encoded = options.replace(' ', "%20").replace('=', "%3D");
    let dsn = format!("{}?options={encoded}", stand.dsn);
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(2),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|error| panic!("a second pool with `{options}` connects: {error}"));
    Arc::new(PgGraphStore::new(
        Arc::new(db),
        GraphStorageConfig::default()
            .validated()
            .expect("the default configuration is valid"),
    ))
}

/// Filtered vector search under-returns without `hnsw.iterative_scan`.
///
/// HNSW is an approximate index and pgvector applies filters *after* the
/// approximate scan, so the tenant predicate this gear always adds removes
/// candidates that the scan has already spent its budget finding. A small
/// tenant sharing an index with a large one can therefore get an empty page
/// while its own matching vectors sit in the table -- and the answer looks
/// exactly like "there is nothing here", which is the part that makes it
/// dangerous rather than merely lossy.
///
/// `ef_search = 1` and a disabled sequential scan are what make the collapse
/// reachable in a test instead of at production scale: the same effect needs
/// tens of thousands of rows at the default of 40, and a fixture that large
/// would measure the machine rather than the behaviour.
#[tokio::test]
async fn a_filtered_vector_search_under_returns_without_iterative_scan() {
    let Some(stand) = stand().await else {
        return;
    };
    let crowd = tenant_on(&stand).await;
    let alone = tenant_on(&stand).await;

    for (tenant, keys) in [
        (
            crowd,
            (0..60).map(|i| format!("crowd-{i}")).collect::<Vec<_>>(),
        ),
        (alone, vec!["the-only-one".to_owned()]),
    ] {
        let scope = AccessScope::for_tenant(tenant);
        let ctx = conformance::ctx(tenant, &scope, None);
        stand
            .store
            .register_types(&ctx, conformance::ontology_batch())
            .await
            .expect("the ontology registers");
        let nodes: Vec<_> = keys
            .iter()
            .map(|key| conformance::summarized(key, key, key))
            .collect();
        conformance::ingest_batch(
            stand.store.as_ref(),
            &ctx,
            conformance::batch(nodes, Vec::new()),
        )
        .await
        .expect("the fixture commits");
    }

    let scope = AccessScope::for_tenant(alone);
    let ctx = conformance::ctx(alone, &scope, None);
    let epoch = conformance::EPOCH;
    let probe = "a probe that names nothing in particular";

    let strict = store_with(&stand, "-c enable_seqscan=off -c hnsw.ef_search=1").await;
    let missed = conformance::search_vector(strict.as_ref(), &ctx, probe, epoch).await;

    let iterative = store_with(
        &stand,
        "-c enable_seqscan=off -c hnsw.ef_search=1 -c hnsw.iterative_scan=relaxed_order",
    )
    .await;
    let found = conformance::search_vector(iterative.as_ref(), &ctx, probe, epoch).await;

    assert!(
        missed.is_empty(),
        "the small tenant's vector is not among the candidates the scan spent \
         its budget on: {missed:?}"
    );
    assert_eq!(
        found,
        vec!["the-only-one".to_owned()],
        "iterative scanning keeps going until the filter has something to \
         return, which is the difference between a recall setting and a \
         correctness one"
    );
}

/// One conformance case against a live server: bring the stand up, mint a
/// tenant, run the shared case. The lane skips when `PostgreSQL` 19 is absent.
macro_rules! pg_case {
    ($name:ident, $case:path) => {
        #[tokio::test]
        async fn $name() {
            let Some(stand) = stand().await else {
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
    offboarding_removes_every_row_of_the_tenant_and_no_other,
    conformance::offboarding_removes_every_row_of_the_tenant_and_no_other
);
pg_case!(
    a_fenced_tenant_takes_no_write,
    conformance::a_fenced_tenant_takes_no_write
);
pg_case!(
    offboarding_is_idempotent_and_only_for_a_fenced_tenant,
    conformance::offboarding_is_idempotent_and_only_for_a_fenced_tenant
);
pg_case!(
    the_fenced_tenants_are_listed_for_the_read_path,
    conformance::the_fenced_tenants_are_listed_for_the_read_path
);

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
    a_keyless_retry_is_a_new_request,
    conformance::a_keyless_retry_is_a_new_request
);
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
    the_view_reads_report_whether_a_vector_is_stored,
    conformance::the_view_reads_report_whether_a_vector_is_stored
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
    a_type_pattern_narrows_search_and_a_hop,
    conformance::a_type_pattern_narrows_search_and_a_hop
);
pg_case!(
    hybrid_search_fuses_both_arms,
    conformance::hybrid_search_fuses_both_arms
);
pg_case!(
    deleting_an_already_tombstoned_row_is_a_no_op,
    conformance::deleting_an_already_tombstoned_row_is_a_no_op
);
pg_case!(
    the_type_catalogue_pages_through_its_own_cursor,
    conformance::the_type_catalogue_pages_through_its_own_cursor
);
pg_case!(
    a_deleted_conclusion_stops_pinning_its_endpoint,
    conformance::a_deleted_conclusion_stops_pinning_its_endpoint
);
pg_case!(
    a_batch_that_names_one_type_twice_is_refused,
    conformance::a_batch_that_names_one_type_twice_is_refused
);
pg_case!(
    an_unchanged_re_ingest_embeds_nothing,
    conformance::an_unchanged_re_ingest_embeds_nothing
);

pg_case!(
    a_key_carried_twice_is_written_twice_in_request_order,
    conformance::a_key_carried_twice_is_written_twice_in_request_order
);

pg_case!(
    a_purge_removes_a_node_and_every_edge_and_frees_its_key,
    conformance::a_purge_removes_a_node_and_every_edge_and_frees_its_key
);

pg_case!(
    a_purge_takes_a_tombstone_and_repeats_as_no_change,
    conformance::a_purge_takes_a_tombstone_and_repeats_as_no_change
);

pg_case!(
    a_scope_erasure_removes_its_tombstoned_members,
    conformance::a_scope_erasure_removes_its_tombstoned_members
);

pg_case!(
    a_tombstoned_edge_a_replacement_removes_is_counted,
    conformance::a_tombstoned_edge_a_replacement_removes_is_counted
);

pg_case!(
    a_purge_of_tombstones_only_refuses_a_live_node,
    conformance::a_purge_of_tombstones_only_refuses_a_live_node
);

pg_case!(
    a_purge_bound_to_a_creation_instant_leaves_a_later_node_alone,
    conformance::a_purge_bound_to_a_creation_instant_leaves_a_later_node_alone
);

pg_case!(
    a_hubs_edges_are_listed_a_page_at_a_time,
    conformance::a_hubs_edges_are_listed_a_page_at_a_time
);

pg_case!(
    edge_payloads_are_read_for_live_edges_only,
    conformance::edge_payloads_are_read_for_live_edges_only
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
/// migrations ran.
#[tokio::test]
async fn readiness_reports_every_capability_and_only_some_block_service() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::readiness_reports_every_capability_and_only_some_block_service(
        stand.store.as_ref(),
    )
    .await;
}

// --- scope replacement --------------------------------------------------------

/// Written out rather than `pg_case!`d: the two replacements are spawned as
/// separate tasks and need a runtime with threads to put them on, or the race
/// the obligation is about never happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_replacements_of_one_scope_serialize() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_replacements_of_one_scope_serialize(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Spawned tasks again, so the runtime needs threads to put them on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_update_of_a_node_advances_its_version() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::every_update_of_a_node_advances_its_version(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_writers_with_one_expected_version_do_not_both_win() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_writers_with_one_expected_version_do_not_both_win(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Spawned tasks again, so a multi-thread runtime rather than `pg_case!`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_type_updates_do_not_share_one_revision() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_type_updates_do_not_share_one_revision(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Spawned tasks again, so a multi-thread runtime rather than `pg_case!`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_racing_an_upsert_leaves_no_rewritten_tombstone() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::a_delete_racing_an_upsert_leaves_no_rewritten_tombstone(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Against the store the window opens in: the replacement's read and its
/// removal are separate statements, and the ingest can commit between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replacement_does_not_delete_what_an_ingest_moved_out_of_its_scope() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::a_replacement_does_not_delete_what_an_ingest_moved_out_of_its_scope(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Against the store where two inserts can genuinely collide on the unique
/// key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_batches_naming_one_new_endpoint_both_land() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_batches_naming_one_new_endpoint_both_land(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// Against the store the window opens in: both deletes read the row live
/// before either commits.
/// Against the store the window opens in: the node's delete and the edge's
/// both read the edge live before either commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_delete_racing_its_edge_delete_counts_every_edge_once() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::a_node_delete_racing_its_edge_delete_counts_every_edge_once(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_deletes_of_one_node_tombstone_it_once() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_deletes_of_one_node_tombstone_it_once(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// The edge half of that race, against the store the window can actually
/// open in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_edge_is_revived_by_the_next_upsert() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::a_deleted_edge_is_revived_by_the_next_upsert(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

/// One edge between `edge-a` and `edge-b` on a fresh tenant, and its key.
async fn one_edge(stand: &Stand) -> (Uuid, String) {
    let tenant = tenant_on(stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("edge-a", "a"),
                conformance::node("edge-b", "b"),
            ],
            vec![conformance::edge("edge-a", "edge-b")],
        ),
    )
    .await
    .expect("the edge is created");
    let key = stand
        .store
        .get_node(&ctx, &"edge-a".to_owned(), 10)
        .await
        .expect("the endpoint reads")
        .adjacency
        .first()
        .expect("the edge is adjacent")
        .edge_key
        .clone();
    (tenant, key)
}

/// A key no edge ever held is absent, not a no-op: only a tombstone under
/// the key proves the delete already happened.
#[tokio::test]
async fn deleting_an_edge_key_nobody_holds_is_not_found() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let refused = stand
        .store
        .soft_delete(
            &ctx,
            graph_storage_sdk::models::DeleteRequest::Edge("no-such-edge".to_owned()),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(graph_storage_sdk::plugin_api::GraphStoreError::NotFound)
        ),
        "an edge that never existed reads as absent, got {refused:?}"
    );
}

/// The edge half of the lost compare-and-set, made deterministic.
///
/// The race test above opens the window only now and then, so the arm that
/// settles a delete whose `deleted_at IS NULL` write found the row already
/// tombstoned went unrun. Here a plain transaction tombstones the row and
/// holds its lock: the delete reads the edge live, blocks on the write, and
/// sees zero rows affected once the holder commits. That delete happened
/// already, so the answer is the no-op, not a conflict and not absence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_delete_that_loses_its_write_to_another_delete_is_a_no_op() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let (tenant, key) = one_edge(&stand).await;

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hold the row");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "UPDATE edge SET deleted_at = now() WHERE edge_key = '{key}'"
        ))
        .await
        .expect("the holder tombstones the row and keeps its lock");

    let deleter = {
        let store = Arc::clone(&stand.store);
        let key = key.clone();
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            store
                .soft_delete(&ctx, graph_storage_sdk::models::DeleteRequest::Edge(key))
                .await
        })
    };

    // Commit only once the delete waits on the row, or it would read the
    // tombstone and take the arm that is already covered.
    let waiting = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n FROM pg_stat_activity \
         WHERE wait_event_type = 'Lock' AND datname = current_database()"
            .to_owned(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let row = raw
            .query_one_raw(waiting.clone())
            .await
            .expect("the activity view reads")
            .expect("a count is one row");
        if row.try_get::<i64>("", "n").expect("the count is a number") > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the delete never blocked on the held row"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    holder.commit().await.expect("the holder commits");

    let outcome = deleter
        .await
        .expect("the deleter task does not panic")
        .expect("a delete that lost to another delete is a no-op, not a refusal");
    assert_eq!(
        (outcome.tombstoned_nodes, outcome.tombstoned_edges),
        (0, 0),
        "the delete reports no write it did not make"
    );
}

/// An endpoint hidden while its edge stays live leaves the edge to the
/// edge read's answer: absent. Tombstoning the node by hand, without its
/// edges, is the shape a scope the caller does not hold also takes, and
/// the delete must neither tombstone the edge nor call it already gone.
#[tokio::test]
async fn an_edge_whose_endpoint_is_hidden_is_not_found_and_stays_live() {
    use sea_orm::ConnectionTrait as _;
    let Some(stand) = stand().await else {
        return;
    };
    let (tenant, key) = one_edge(&stand).await;
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hide the endpoint");
    raw.execute_unprepared("UPDATE node SET deleted_at = now() WHERE node_key = 'edge-b'")
        .await
        .expect("the endpoint is tombstoned alone");

    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let refused = stand
        .store
        .soft_delete(
            &ctx,
            graph_storage_sdk::models::DeleteRequest::Edge(key.clone()),
        )
        .await;
    assert!(
        matches!(
            refused,
            Err(graph_storage_sdk::plugin_api::GraphStoreError::NotFound)
        ),
        "a live edge behind a hidden endpoint reads as absent, got {refused:?}"
    );
    let live = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("SELECT (deleted_at IS NULL) AS live FROM edge WHERE edge_key = '{key}'"),
        ))
        .await
        .expect("the edge reads")
        .expect("the edge row is there");
    assert!(
        live.try_get::<bool>("", "live").expect("a flag"),
        "the refused delete left the edge as it was"
    );
}

/// Written out rather than `pg_case!`d, for the same reason as the scope
/// race: the two ingests are spawned as separate tasks and need a runtime
/// with threads to put them on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_committed_mutation_gets_its_own_revision() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::every_committed_mutation_gets_its_own_revision(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

pg_case!(
    scope_replacement_removes_an_edge_whose_endpoints_remain,
    conformance::scope_replacement_removes_an_edge_whose_endpoints_remain
);
/// A catalogue scan that has read nothing refuses rather than answering
/// "there is nothing".
///
/// The scan breaks out of its pass loop when the deadline is gone, so the
/// page already gathered survives with the cursor that resumes it. Before the
/// first pass there is no such page: breaking there would answer with an
/// empty list and no cursor, which every client reads as the end of the
/// catalogue -- the silent loss the break was introduced to avoid, in a
/// different shape.
///
/// The other half of the branch, breaking *after* progress, resumes through
/// the same `reached_overall` cursor the scan-cap exit uses, which
/// `the_type_catalogue_pages_through_its_own_cursor` covers. Forcing the
/// deadline to fall between two passes is a race, and a race asserts nothing
/// on the run where it does not happen.
#[tokio::test]
async fn a_catalogue_scan_with_no_progress_refuses_rather_than_answering_empty() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let live = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&live, conformance::ontology_batch())
        .await
        .expect("the ontology registers");

    // A budget that is already gone: the state every request reaches on its
    // way out, and the only one a case can be in without measuring the clock.
    let spent = graph_storage_sdk::plugin_api::StoreCtx {
        budget: graph_storage_sdk::models::RemainingBudget::starting_now(std::time::Duration::ZERO),
        ..conformance::ctx(tenant, &scope, None)
    };
    let refused = stand
        .store
        .list_types(&spent, graph_storage_sdk::models::TypeQuery::default())
        .await
        .expect_err("a scan that read nothing has nothing to hand back");
    assert!(
        matches!(
            refused,
            graph_storage_sdk::plugin_api::GraphStoreError::Deadline
        ),
        "expected a deadline refusal, got {refused:?}"
    );

    // And the ordinary path is untouched: a live budget still answers.
    let page = stand
        .store
        .list_types(&live, graph_storage_sdk::models::TypeQuery::default())
        .await
        .expect("a live budget lists the catalogue");
    assert!(!page.items.is_empty(), "the fixture is there to be listed");
}

pg_case!(
    an_edge_is_not_taken_from_the_scope_that_declared_it,
    conformance::an_edge_is_not_taken_from_the_scope_that_declared_it
);
pg_case!(
    one_replacement_does_not_take_another_scopes_edges,
    conformance::one_replacement_does_not_take_another_scopes_edges
);
pg_case!(
    a_store_without_labels_refuses_every_label_call,
    conformance::a_store_without_labels_refuses_every_label_call
);
pg_case!(
    an_edge_cannot_name_a_tombstoned_endpoint,
    conformance::an_edge_cannot_name_a_tombstoned_endpoint
);
pg_case!(
    node_types_answers_the_live_nodes_it_is_asked_about,
    conformance::node_types_answers_the_live_nodes_it_is_asked_about
);
pg_case!(
    an_edge_only_scope_drops_the_edges_it_stops_declaring,
    conformance::an_edge_only_scope_drops_the_edges_it_stops_declaring
);
pg_case!(
    an_empty_replacement_removes_the_whole_scope,
    conformance::an_empty_replacement_removes_the_whole_scope
);
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
    a_type_referencing_a_non_ancestor_gains_an_optional_property,
    conformance::a_type_referencing_a_non_ancestor_gains_an_optional_property
);
pg_case!(
    a_type_referencing_a_non_ancestor_still_refuses_a_breaking_change,
    conformance::a_type_referencing_a_non_ancestor_still_refuses_a_breaking_change
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
    let Some(stand) = stand().await else {
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

/// The store holds a cursor to the listing that minted it without the
/// service in front of it: a request built in-process carries no filter hash
/// (`ODataQuery::with_filter` never sets one), and the store's check used to
/// compare `None` with `None` for such a caller. The identity is computed
/// from what the request holds, on both paths the store pages -- the
/// platform pager over the columns, the keyset walk over a payload ordering.
#[tokio::test]
async fn a_cursor_is_held_to_its_filter_and_type_set_at_the_store() {
    use toolkit_odata::SortDir;
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let store = stand.store.as_ref();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    conformance::projection_seeded(store, &ctx, &[]).await;

    let first_page = |mut request: graph_storage_sdk::models::ProjectionRequest| {
        request.query = request.query.with_limit(2);
        request
    };
    // One listing per path: the columns-only one is paged by the platform,
    // the payload-ordered one by this gear's keyset walk.
    for (path, minted) in [
        (
            "platform pager",
            first_page(conformance::projection(
                &[conformance::INDEXED],
                "payload/severity eq 'high'",
                &[],
            )),
        ),
        (
            "keyset over a payload ordering",
            first_page(conformance::projection(
                &[conformance::INDEXED],
                "payload/severity eq 'high'",
                &[("payload/score", SortDir::Desc)],
            )),
        ),
    ] {
        let page = store
            .project_table(&ctx, minted.clone())
            .await
            .expect("the first page is served");
        let token = page
            .page_info
            .next_cursor
            .expect("three matching tickets over a page of two leave a continuation");
        let cursor = toolkit_odata::CursorV1::decode(&token).expect("a CursorV1 token");
        assert!(
            cursor.f.is_some(),
            "{path}: the token names the listing although the request carried no hash"
        );

        let replay = |type_set: Option<&[&str]>, filter_text: &str| {
            let mut request = conformance::projection(
                type_set.unwrap_or(&[conformance::INDEXED]),
                filter_text,
                &[],
            );
            if type_set.is_none() {
                request.type_set = None;
            }
            request.query = request.query.with_limit(2).with_cursor(cursor.clone());
            request
        };
        for (case, request) in [
            (
                "replayed under another filter",
                replay(Some(&[conformance::INDEXED]), "payload/severity eq 'low'"),
            ),
            (
                "replayed without its filter",
                replay(Some(&[conformance::INDEXED]), ""),
            ),
            (
                "replayed without its type set",
                replay(None, "payload/severity eq 'high'"),
            ),
        ] {
            let refused = store
                .project_table(&ctx, request)
                .await
                .expect_err("the cursor does not continue a different listing");
            assert!(
                matches!(&refused, graph_storage_sdk::plugin_api::GraphStoreError::InvalidQuery { what } if what.contains("cursor")),
                "{path}, {case}: expected the cursor refused, got {refused:?}"
            );
        }

        // Its own listing continues.
        let same = replay(Some(&[conformance::INDEXED]), "payload/severity eq 'high'");
        let rest = store
            .project_table(&ctx, same)
            .await
            .expect("the cursor continues the listing that minted it");
        assert_eq!(
            page.items.len() + rest.items.len(),
            3,
            "{path}: the two pages together are the three high tickets"
        );
    }
}

/// The row ceiling, on a store configured below what the case seeds. The
/// conformance body is what the ceiling is held to; this is the store that
/// can be told to hold it at two rows.
#[tokio::test]
async fn a_pass_over_the_ceiling_is_refused_and_a_dry_run_reports_it() {
    let Some(stand) = stand().await else {
        return;
    };
    let bounded = Arc::new(PgGraphStore::new(
        Arc::clone(&stand.db),
        GraphStorageConfig {
            type_update_max_rows: 2,
            ontology_max_chain_depth: 8,
            ..GraphStorageConfig::default()
        }
        .validated()
        .expect("the test configuration is valid"),
    ));
    let tenant = tenant_on(&stand).await;
    conformance::a_pass_over_the_ceiling_is_refused_and_a_dry_run_reports_it(
        bounded.as_ref(),
        tenant,
    )
    .await;
}

#[tokio::test]
async fn colliding_node_keys_stay_inside_their_tenants() {
    let Some(stand) = stand().await else {
        return;
    };
    let one = tenant_on(&stand).await;
    let two = tenant_on(&stand).await;
    conformance::tenant_isolation(stand.store.as_ref(), one, two).await;
}

// --- what only a real PostgreSQL can show -----------------------------------

/// The plans of the two hot read paths can reach an index (m0009).
///
/// `enable_seqscan = off` makes the question "is there an index this
/// statement *can* use" rather than "is one cheaper here": the hop's read of
/// a frontier's incident edges has to be driven by an endpoint index, and a
/// listing of one type has none that yields that type in key order -- the shape a payload filter
/// needs at a middling selectivity, where the planner otherwise walks the
/// whole tenant in key order (measured on the Studio stand; the choice itself
/// needs volume to reproduce, the missing index does not). The assertion is
/// on the index condition, not an index name, so a planner's choice between
/// equivalent indexes does not decide it.
#[tokio::test]
async fn the_hop_and_the_filtered_listing_have_an_index_to_use() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let mut ontology = conformance::ontology_batch();
    ontology.push(conformance::other_thing_type());
    stand
        .store
        .register_types(&ctx, ontology)
        .await
        .expect("ontology registers");
    // The listed type is a fifth of the tenant and interleaved with another
    // in key order (`n1` < `n1-x0` < ... < `n10`), which is what a listing
    // of one type among several looks like. With one type alone, walking
    // the tenant's key index and filtering on the type *is* the type's key
    // order, the two plans cost the same, and which one the planner picks
    // turned on how the rows happened to be laid out on disk -- ingest
    // writing in key order was enough to flip it.
    let nodes: Vec<_> = (0..400)
        .flat_map(|i| {
            std::iter::once(conformance::node(&format!("n{i}"), "n")).chain((0..4).map(move |j| {
                graph_storage_sdk::models::NodeSpec {
                    type_id: conformance::OTHER_THING.to_owned(),
                    ..conformance::node(&format!("n{i}-x{j}"), "other")
                }
            }))
        })
        .collect();
    let edges = (0..400)
        .map(|i| conformance::edge(&format!("n{i}"), &format!("n{}", (i * 7 + 1) % 400)))
        .collect();
    conformance::ingest_batch(stand.store.as_ref(), &ctx, conformance::batch(nodes, edges))
        .await
        .expect("the graph is seeded");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to read plans");
    raw.execute_unprepared("ANALYZE node; ANALYZE edge")
        .await
        .expect("statistics are fresh");
    let tx = raw.begin().await.expect("one session for the setting");
    tx.execute_unprepared("SET LOCAL enable_seqscan = off")
        .await
        .expect("the planner setting applies");
    let statement =
        |sql: String| sea_orm::Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql);
    let plan = |sql: String| {
        let tx = &tx;
        async move {
            tx.query_all_raw(statement(format!("EXPLAIN {sql}")))
                .await
                .expect("the statement plans")
                .iter()
                .map(|row| row.try_get_by_index::<String>(0).expect("a plan line"))
                .collect::<Vec<_>>()
        }
    };
    let t = format!("'{tenant}'");
    let row = tx
        .query_one_raw(statement(format!(
            "SELECT id, gts_node_type_id FROM node WHERE tenant_id = {t} AND node_key = 'n1'"
        )))
        .await
        .expect("the seed resolves")
        .expect("n1 exists");
    let seed: i64 = row.try_get_by_index(0).expect("an id");
    let type_id: i32 = row.try_get_by_index(1).expect("a type id");

    let hop = plan(format!(
        "SELECT edge_key, gts_edge_type_id, src_node_id, dst_node_id FROM edge \
         WHERE tenant_id IN ({t}) AND (src_node_id IN ({seed}) OR dst_node_id IN ({seed})) \
         AND deleted_at IS NULL LIMIT 100001"
    ))
    .await;
    assert!(
        hop.iter()
            .any(|line| line.contains("Index Cond") && line.contains("src_node_id")),
        "the hop finds the frontier's edges through an index on their source:\n{}",
        hop.join("\n")
    );

    let listing = plan(format!(
        "SELECT node_key FROM node WHERE tenant_id IN ({t}) AND deleted_at IS NULL \
         AND gts_node_type_id IN ({type_id}) ORDER BY node_key ASC LIMIT 51"
    ))
    .await;
    let reads_the_type_by_index = listing
        .iter()
        .any(|line| line.contains("Index Cond") && line.contains("gts_node_type_id"));
    let sorts = listing.iter().any(|line| {
        let line = line.trim_start().trim_start_matches("->").trim_start();
        line.starts_with("Sort") || line.starts_with("Incremental Sort")
    });
    assert!(
        reads_the_type_by_index && !sorts,
        "a listing reads one type in key order, without a sort:\n{}",
        listing.join("\n")
    );
}

/// `m0011` brings an edge written with an empty discriminator to the form
/// ingest now writes, none. The edge is written through the store, below the
/// service's normalisation, which is how every such row was written before
/// it.
#[tokio::test]
async fn m0011_stores_an_empty_discriminator_as_none() {
    use sea_orm_migration::{MigrationTrait as _, SchemaManager};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let store = stand.store.as_ref();
    store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    conformance::ingest_batch(
        store,
        &ctx,
        conformance::batch(
            vec![
                conformance::node("empty-a", "a"),
                conformance::node("empty-b", "b"),
            ],
            vec![graph_storage_sdk::models::EdgeSpec {
                discriminator: Some(String::new()),
                ..conformance::edge("empty-a", "empty-b")
            }],
        ),
    )
    .await
    .expect("the edge commits");
    let key = "empty-a".to_owned();

    let before = store
        .get_node(&ctx, &key, 100)
        .await
        .expect("the node reads");
    assert_eq!(
        before.adjacency[0].discriminator.as_deref(),
        Some(""),
        "the store kept the empty string it was given"
    );

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to the stand");
    graph_storage::infra::storage::migrations::m0011_empty_discriminator::Migration
        .up(&SchemaManager::new(&raw))
        .await
        .expect("m0011 applies");

    let after = store
        .get_node(&ctx, &key, 100)
        .await
        .expect("the node reads");
    assert_eq!(
        after.adjacency[0].discriminator, None,
        "an empty discriminator is stored as none"
    );
}

/// `m0010` drops a property graph `m0002` left behind, and does nothing on a
/// server without SQL/PGQ, which is every server CI runs. Against a 19 beta
/// 1–3 image the case re-creates the graph and shows the drop.
#[tokio::test]
async fn m0010_drops_the_retired_property_graph_where_one_exists() {
    use sea_orm::ConnectionTrait as _;
    use sea_orm_migration::{MigrationTrait as _, SchemaManager};
    let Some(stand) = stand().await else {
        return;
    };
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for operator surgery");
    let present = |raw: &sea_orm::DatabaseConnection| {
        let raw = raw.clone();
        async move {
            raw.query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT 1 AS present FROM pg_class WHERE relname = 'kb' AND relkind = 'g'",
            ))
            .await
            .expect("the catalog reads")
            .is_some()
        }
    };
    let drop_graph =
        graph_storage::infra::storage::migrations::m0010_drop_property_graph::Migration;

    assert!(
        !present(&raw).await,
        "the migrations leave no property graph on a fresh database"
    );
    let created = raw
        .execute_unprepared(
            "CREATE PROPERTY GRAPH kb \
             VERTEX TABLES (node KEY (tenant_id, id)) \
             EDGE TABLES (edge KEY (tenant_id, id) \
               SOURCE KEY (tenant_id, src_node_id) REFERENCES node (tenant_id, id) \
               DESTINATION KEY (tenant_id, dst_node_id) REFERENCES node (tenant_id, id))",
        )
        .await;
    if let Err(error) = created {
        eprintln!(
            "this server has no SQL/PGQ ({error}); m0010 has nothing to drop, and runs as a no-op"
        );
        drop_graph
            .up(&SchemaManager::new(&raw))
            .await
            .expect("m0010 is a no-op on a server without SQL/PGQ");
        assert!(!present(&raw).await);
        return;
    }
    assert!(present(&raw).await, "the retired graph was re-created");
    drop_graph
        .up(&SchemaManager::new(&raw))
        .await
        .expect("m0010 drops the graph");
    assert!(!present(&raw).await, "m0010 dropped the retired graph");
}

/// A type an earlier build stored, and this build's analysis refuses,
/// converges when it is offered again byte-identical.
///
/// The shape is the one Studio met after the upgrade (2026-09-28): its edge
/// types were registered with `x-gts-traits: {"full_text_search": []}`, which
/// the edge base no longer admits. Re-registering the unchanged schema -- what
/// a producer sends on every run -- was refused, and the changed schema that
/// this build accepts is a different schema, `409` without
/// `on_existing: update`. The stored row is written here by operator
/// surgery, because no current build writes it; a *changed* schema that is
/// still invalid is refused as before.
#[tokio::test]
async fn a_stored_type_this_build_would_refuse_converges_when_offered_unchanged() {
    use sea_orm::ConnectionTrait as _;
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let batch = conformance::ontology_batch();
    stand
        .store
        .register_types(&ctx, batch.clone())
        .await
        .expect("ontology registers");

    let mut legacy = batch
        .iter()
        .find(|r| r.type_id == conformance::LINK)
        .expect("the batch carries the link type")
        .schema
        .clone();
    legacy["x-gts-traits"] = serde_json::json!({ "full_text_search": [] });

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for operator surgery");
    raw.execute_raw(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "UPDATE gts_type SET type_schema = $1 WHERE tenant_id = $2 AND gts_type_id = $3",
        [
            legacy.clone().into(),
            tenant.into(),
            conformance::LINK.to_owned().into(),
        ],
    ))
    .await
    .expect("the stored schema is the one an earlier build wrote");

    let registered = stand
        .store
        .register_types_with(
            &ctx,
            vec![graph_storage_sdk::models::TypeRegistration {
                type_id: conformance::LINK.to_owned(),
                schema: legacy.clone(),
            }],
            graph_storage_sdk::models::TypeRegistrationOptions::default(),
        )
        .await
        .expect("the unchanged stored schema converges");
    assert_eq!(
        registered.first().map(|r| r.outcome),
        Some(graph_storage_sdk::models::TypeOutcome::Unchanged)
    );

    let legacy_schema = legacy.clone();
    let mut changed = legacy;
    changed["description"] = serde_json::json!("changed, and still refused");
    stand
        .store
        .register_types(
            &ctx,
            vec![graph_storage_sdk::models::TypeRegistration {
                type_id: conformance::LINK.to_owned(),
                schema: changed,
            }],
        )
        .await
        .expect_err("a changed schema is analyzed, and this one is refused");

    // The service analyzes before the store does, so it holds the same
    // exception: the path REST and the in-process client take.
    let harness = support::Harness::configured_over_store(
        Arc::clone(&stand.store) as Arc<dyn GraphStoreV1>,
        Arc::new(graph_storage::infra::fake_store::FakeGraphStore::new()),
        Arc::new(support::AllowInOwnTenant),
        GraphStorageConfig::default(),
    );
    let service_ctx = harness.ctx();
    harness
        .services
        .register_types(&service_ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers through the service");
    raw.execute_raw(sea_orm::Statement::from_sql_and_values(
        sea_orm::DatabaseBackend::Postgres,
        "UPDATE gts_type SET type_schema = $1 WHERE tenant_id = $2 AND gts_type_id = $3",
        [
            legacy_schema.clone().into(),
            harness.tenant.into(),
            conformance::LINK.to_owned().into(),
        ],
    ))
    .await
    .expect("the service tenant holds the earlier build's schema too");
    harness
        .services
        .register_types(
            &service_ctx,
            vec![graph_storage_sdk::models::TypeRegistration {
                type_id: conformance::LINK.to_owned(),
                schema: legacy_schema,
            }],
        )
        .await
        .expect("the service converges the unchanged stored schema too");
}

/// A write is the caller's own tenant's, whatever a read may see.
///
/// Under the platform's default `Subtree` mode a parent tenant's compiled
/// scope includes its children, and the store looks write-path rows up by key
/// under that scope. A parent's upsert of key K found the child's K and
/// rewrote it; a parent's replacement of `repository = R` hard-deleted the
/// child's nodes of R. The PDP here answers the way a subtree does, and the
/// child's node must come out of both exactly as it went in.
#[tokio::test]
async fn a_parent_tenants_writes_never_reach_its_child() {
    let Some(stand) = stand().await else {
        return;
    };
    let child = tenant_on(&stand).await;
    let harness = support::Harness::configured_over_store(
        Arc::clone(&stand.store) as Arc<dyn GraphStoreV1>,
        Arc::new(graph_storage::infra::fake_store::FakeGraphStore::new()),
        Arc::new(support::WithChildren(vec![child])),
        GraphStorageConfig::default(),
    );
    let parent = harness.tenant;
    graph_storage::infra::store::ingest::ensure_meta(
        stand.store.as_ref(),
        parent,
        &AccessScope::for_tenant(parent),
    )
    .await
    .expect("meta rows exist");
    let as_parent = harness.ctx();
    let as_child = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(child)
        .build()
        .expect("a valid security context");
    harness.seed_ontology(&as_child).await;
    harness.seed_ontology(&as_parent).await;

    let shared = |name: &str| graph_storage_sdk::models::NodeSpec {
        payload: Some(serde_json::json!({ "repository": "acme/infra" })),
        ..conformance::node("shared", name)
    };
    harness
        .services
        .ingest(
            &as_child,
            conformance::batch(vec![shared("the child's")], Vec::new()),
        )
        .await
        .expect("the child writes its node");

    harness
        .services
        .ingest(
            &as_parent,
            conformance::batch(vec![shared("the parent's")], Vec::new()),
        )
        .await
        .expect("the parent writes the same key");
    let child_scope = AccessScope::for_tenant(child);
    let child_ctx = conformance::ctx(child, &child_scope, None);
    let key = "shared".to_owned();
    let after_upsert = stand
        .store
        .get_node(&child_ctx, &key, 10)
        .await
        .expect("the child's node is there");
    assert_eq!(
        after_upsert.name.as_deref(),
        Some("the child's"),
        "the parent's upsert did not rewrite the child's node"
    );
    harness
        .services
        .ingest(
            &as_parent,
            graph_storage_sdk::models::IngestRequest {
                replace_scope: Some(graph_storage_sdk::models::ReplaceScope {
                    attribute: "repository".to_owned(),
                    value: "acme/infra".to_owned(),
                    generation: 1,
                }),
                ..conformance::batch(Vec::new(), Vec::new())
            },
        )
        .await
        .expect("the parent erases its own scope");

    let node = stand
        .store
        .get_node(&child_ctx, &key, 10)
        .await
        .expect("the parent's replacement did not remove the child's node");
    assert_eq!(node.name.as_deref(), Some("the child's"));
}

/// A replacement's membership predicate has an index to use.
///
/// The predicate is the store's own (`scope::membership`), rendered into the
/// statement a replacement issues for its stale set, and planned with
/// sequential scans off: the question is whether an index *can* serve it.
/// `payload #>> '{repository}' = ...` could not, so every replacement read the
/// tenant's managed nodes.
#[tokio::test]
async fn a_replacements_membership_is_served_by_the_payload_index() {
    use graph_storage::infra::storage::entity::node;
    use sea_orm::sea_query::{Expr, ExprTrait as _, PostgresQueryBuilder, Query};
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
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
    // A tenant with volume in it, of which the scope is a sliver: on an empty
    // table the tenant's own index is as good as any, and the plan would say
    // nothing about the predicate.
    let nodes = (0..1000)
        .map(|i| graph_storage_sdk::models::NodeSpec {
            payload: Some(serde_json::json!({
                "repository": if i % 100 == 0 { "acme/infra".to_owned() } else { format!("acme/r{i}") }
            })),
            ..conformance::node(&format!("n{i}"), "n")
        })
        .collect();
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(nodes, Vec::new()),
    )
    .await
    .expect("the tenant is seeded");
    let (sql, values) = Query::select()
        .column(node::Column::Id)
        .from(node::Entity)
        .and_where(Expr::col(node::Column::TenantId).eq(tenant))
        .and_where(Expr::col(node::Column::DeletedAt).is_null())
        .and_where(graph_storage::infra::store::scope::membership(
            "repository",
            "acme/infra",
        ))
        .build(PostgresQueryBuilder);

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to read plans");
    raw.execute_unprepared("ANALYZE node")
        .await
        .expect("statistics are fresh");
    let tx = raw.begin().await.expect("one session for the setting");
    tx.execute_unprepared("SET LOCAL enable_seqscan = off")
        .await
        .expect("the planner setting applies");
    let plan: Vec<String> = tx
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            format!("EXPLAIN {sql}"),
            values,
        ))
        .await
        .expect("the statement plans")
        .iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("a plan line"))
        .collect();
    assert!(
        plan.iter()
            .any(|line| line.contains("Index Cond") && line.contains("payload @>")),
        "the membership is an index condition on the payload:\n{}",
        plan.join("\n")
    );
}

/// Admission refuses a NUL before any statement; this is the net under it. A
/// NUL that reaches the server anyway -- here through the store directly,
/// which is where a path admission does not cover would put it -- is refused
/// as `22021` for `text`, and that is the caller's input, not a store fault:
/// it used to classify as `Internal` and answer `500 unknown`.
#[tokio::test]
async fn a_nul_that_reaches_the_server_is_invalid_input_not_an_internal_error() {
    let Some(stand) = stand().await else {
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

    let error = conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(vec![conformance::node("nul", "na\u{0}me")], Vec::new()),
    )
    .await
    .expect_err("the server cannot store a NUL");
    assert!(
        matches!(
            error,
            graph_storage_sdk::plugin_api::GraphStoreError::InvalidQuery { .. }
        ),
        "a NUL the server refused is invalid input, got {error:?}"
    );
}

/// The cross-tenant trap. Two tenants own a node under the same key, and each
/// has its own edge; a hop that leaked would return the other tenant's key.
///
/// The precondition is asserted first: on the prototype this fixture went
/// missing and the guard test passed vacuously for days.
#[tokio::test]
async fn a_hop_never_leaves_its_tenant() {
    let Some(stand) = stand().await else {
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
                discriminators: None,
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
    let Some(stand) = stand().await else {
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
                discriminators: None,
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

/// Asking for degrees does not buy a second edge budget.
///
/// `max_edges_scanned` is documented as the bound on one hop. The incidence
/// scan took it, and the degree scan -- a separate read, taken only when a
/// neighbourhood asks for degree-ordered retention -- took a fresh one of its
/// own, so the same configured ceiling meant one number for a traversal and
/// twice that for a neighbourhood. A limit whose value depends on which
/// caller is asking is not a limit anyone can size a deployment against.
#[tokio::test]
async fn asking_for_degrees_does_not_double_the_hop_edge_budget() {
    let Some(stand) = stand().await else {
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

    // A hub with eight edges and a budget of four: the incidence scan spends
    // the whole allowance, so a degree scan that respects the same hop budget
    // has nothing left and must say so rather than read four more.
    let mut nodes = vec![conformance::node("hub", "hub")];
    let mut edges = Vec::new();
    for index in 0..8 {
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

    let request = |with_degrees| ExpandRequest {
        frontier: frontier.clone(),
        direction: Direction::Either,
        edge_types: None,
        labels: None,
        budget: HopBudget {
            max_frontier: 1_000,
            max_edges_scanned: 4,
        },
        with_degrees,
        discriminators: None,
    };

    let plain = stand
        .engine
        .expand(&ctx, request(false))
        .await
        .expect("the hop runs");
    let with_degrees = stand
        .engine
        .expand(&ctx, request(true))
        .await
        .expect("the hop runs with degrees");

    assert_eq!(
        plain.edges.len(),
        with_degrees.edges.len(),
        "the same budget reads the same number of edges whoever is asking"
    );
    assert_eq!(
        with_degrees.truncated,
        Some(TruncationReason::EdgeScanCap),
        "and the hop still reports that its scan was cut short"
    );
    assert_eq!(
        with_degrees.degrees.len(),
        with_degrees.reached.len(),
        "degrees stay index-aligned with the reached set even when unknown"
    );
    // The observable difference. Edge counts and the truncation flag come
    // from the first scan and say nothing about the second, so what proves
    // the budget is shared is that the degree scan had nothing left to spend:
    // the degrees come back unknown rather than computed from four more rows
    // nobody accounted for.
    assert!(
        with_degrees.degrees.iter().all(|degree| *degree == 0),
        "the first scan spent the hop's allowance, so the degrees are unknown \
         rather than bought with a second one: {:?}",
        with_degrees.degrees
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
#[tokio::test]
async fn a_hop_that_hits_its_edge_budget_reports_it() {
    let Some(stand) = stand().await else {
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
        discriminators: None,
    };

    let cut = stand
        .engine
        .expand(&ctx, request(3))
        .await
        .expect("the hop runs");
    assert_eq!(
        cut.truncated,
        Some(TruncationReason::EdgeScanCap),
        "a scan stopped by the edge budget must say so"
    );
    assert_eq!(cut.edges.len(), 3, "the budget still bounds the work");

    // The same hop inside its budget is not truncated, otherwise the
    // assertion above would pass on a hop that reports the cap always.
    let whole = stand
        .engine
        .expand(&ctx, request(100))
        .await
        .expect("the hop runs");
    assert_eq!(whole.truncated, None, "an unbounded hop is whole");
    assert_eq!(whole.edges.len(), 6, "every edge of the hub");
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
    let Some(stand) = stand().await else {
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
        discriminators: None,
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
    let Some(stand) = stand().await else {
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
    let Some(stand) = stand().await else {
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
    let Some(stand) = stand().await else {
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
    let Some(stand) = stand().await else {
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

/// What the ORM reports when `ON CONFLICT DO NOTHING` elides an insert whose
/// row was asked back, on the server this store runs on.
///
/// Phantom creation relies on it: two batches naming one new endpoint race,
/// the loser's insert is elided, and the loser must read the winner's row
/// rather than fail; so must a node or an edge inserted twice. On a backend
/// with `RETURNING` the elision surfaces as `RecordNotFound` -- the returning
/// select found no row -- and every insert that settles a race asks
/// `lost_insert_race` whether this is that. The answer is checked here, on the
/// server: were an ORM upgrade to report it differently, this is where it
/// shows, not as a batch that fails only when two producers happen to collide.
#[tokio::test]
async fn an_elided_insert_that_asks_for_its_row_reports_record_not_found() {
    use graph_storage::infra::storage::entity::graph_meta;
    use sea_orm::{ActiveValue, EntityTrait as _, sea_query::OnConflict};

    let Some(stand) = stand().await else {
        return;
    };
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to the stand");
    let tenant = Uuid::now_v7();
    let row = || graph_meta::ActiveModel {
        tenant_id: ActiveValue::Set(tenant),
        key: ActiveValue::Set("elision-probe".to_owned()),
        value: ActiveValue::Set(serde_json::json!(0)),
    };
    let do_nothing = || {
        OnConflict::columns([graph_meta::Column::TenantId, graph_meta::Column::Key])
            .do_nothing()
            .to_owned()
    };

    graph_meta::Entity::insert(row())
        .on_conflict(do_nothing())
        .exec_with_returning(&raw)
        .await
        .expect("the first insert lands and returns its row");
    let elided = graph_meta::Entity::insert(row())
        .on_conflict(do_nothing())
        .exec_with_returning(&raw)
        .await;
    assert!(
        matches!(elided, Err(sea_orm::DbErr::RecordNotFound(_))),
        "an elided insert with RETURNING must report RecordNotFound, got {elided:?}"
    );
    let error = toolkit_db::secure::ScopeError::Db(elided.expect_err("elided"));
    assert!(
        graph_storage::infra::store::ingest::lost_insert_race(&error),
        "every insert that settles a race reads this error as one: {error:?}"
    );
}

/// The window this needs is open here: the two claims run in two
/// transactions, and both read the edge unowned before either commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_scopes_racing_to_claim_an_unowned_edge_leave_it_with_one() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_scopes_racing_to_claim_an_unowned_edge_leave_it_with_one(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

pg_case!(
    an_edge_does_not_follow_a_node_ingested_under_a_new_key,
    conformance::an_edge_does_not_follow_a_node_ingested_under_a_new_key
);

pg_case!(
    an_expected_version_on_an_absent_key_is_a_conflict_unless_it_is_zero,
    conformance::an_expected_version_on_an_absent_key_is_a_conflict_unless_it_is_zero
);
pg_case!(
    a_batch_with_one_conflicting_type_registers_none_and_names_it,
    conformance::a_batch_with_one_conflicting_type_registers_none_and_names_it
);
pg_case!(
    a_replacement_leaves_alone_what_does_not_carry_its_attribute,
    conformance::a_replacement_leaves_alone_what_does_not_carry_its_attribute
);

/// The window is open here: two transactions both find no row before either
/// commits, and the unique key decides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_creators_with_expected_version_zero_do_not_both_win() {
    let Some(stand) = stand().await else {
        return;
    };
    conformance::two_creators_with_expected_version_zero_do_not_both_win(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        Uuid::now_v7(),
    )
    .await;
}

pg_case!(
    a_read_returns_the_version_a_write_compares_against,
    conformance::a_read_returns_the_version_a_write_compares_against
);

#[tokio::test]
async fn a_hop_follows_only_its_direction() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    conformance::a_hop_follows_only_its_direction(stand.store.as_ref(), &stand.engine, tenant)
        .await;
}

#[tokio::test]
async fn a_hop_reports_and_filters_discriminators() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    conformance::a_hop_reports_and_filters_discriminators(
        stand.store.as_ref(),
        &stand.engine,
        tenant,
    )
    .await;
}

/// Real row locks, so the runtime needs threads for the writers to overlap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_batches_all_land() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    conformance::overlapping_batches_all_land(
        std::sync::Arc::clone(&stand.store) as std::sync::Arc<dyn GraphStoreV1>,
        tenant,
    )
    .await;
}

pg_case!(
    a_count_agrees_with_the_listing,
    conformance::a_count_agrees_with_the_listing
);

/// A column filter goes through the platform's binding on this store (the
/// fake answers column filters unfiltered), so its count is asserted here.
#[tokio::test]
async fn a_count_applies_a_column_filter_as_the_listing_does() {
    let Some(stand) = stand().await else {
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
                conformance::node("col-a", "alpha"),
                conformance::node("col-b", "beta"),
                conformance::node("col-c", "alpha"),
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the batch commits");
    let counted = stand
        .store
        .count_table(
            &ctx,
            conformance::projection(&[], "name eq 'alpha'", &[]),
            100,
        )
        .await
        .expect("the count answers");
    assert_eq!((counted.count, counted.capped), (2, false));
}

/// A listing whose statement the database fails is the server's failure, not
/// the caller's (gears-rust #5789). The pager reports it as an `OData` `Db`
/// error, which the store used to answer as `InvalidQuery` -- a 400 that tells
/// a client to fix a request that was fine and never to retry it.
///
/// The failure is made deterministic rather than hoped for: a plain
/// connection holds `node` under `ACCESS EXCLUSIVE`, and the listing runs on a
/// pool whose `lock_timeout` makes the server refuse to wait for it.
#[tokio::test]
async fn a_listing_the_database_fails_is_unavailable_not_an_invalid_query() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hold the table");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared("LOCK TABLE node IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("the holder locks the node table");

    let impatient = store_with(&stand, "-c lock_timeout=200").await;
    let error = impatient
        .project_table(
            &ctx,
            graph_storage_sdk::models::ProjectionRequest::default(),
        )
        .await
        .expect_err("the listing cannot read a table it may not lock");
    holder.rollback().await.expect("the holder lets go");

    // Which server-side category a lock timeout gets is not pinned here: the
    // pager hands the store the driver's text without its SQLSTATE, so the
    // listing cannot classify it as the rest of the store does (55P03 is a
    // serialization failure there). What holds is that it is not the caller's
    // mistake.
    assert!(
        !matches!(
            error,
            graph_storage_sdk::plugin_api::GraphStoreError::InvalidQuery { .. }
                | graph_storage_sdk::plugin_api::GraphStoreError::LimitExceeded { .. }
        ),
        "a database failure is the server's, not an invalid query: {error:?}"
    );
}

/// A key another batch inserts between this batch's up-front read and its
/// bulk insert is written on the item path, as an update of that row
/// (gears-rust #5040). The other batch is a plain transaction holding an
/// uncommitted row under the key: the bulk insert waits on it, `DO NOTHING`
/// skips it once it commits, and `RETURNING` leaves it out -- which is the
/// only thing that tells the store the key was lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_lost_to_another_batch_during_the_bulk_insert_lands_as_an_update() {
    use graph_storage_sdk::models::ItemOutcome;
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};

    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other batch");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "INSERT INTO node (tenant_id, node_key, gts_node_type_id, name) \
             SELECT '{tenant}', 'race-k', id, 'held' FROM gts_type \
             WHERE tenant_id = '{tenant}' AND gts_type_id = '{}'",
            conformance::OWNED
        ))
        .await
        .expect("the holder inserts the key and keeps it uncommitted");

    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            let mut request = conformance::batch(
                vec![
                    conformance::node("race-a", "a"),
                    conformance::node("race-k", "from the batch"),
                    conformance::node("race-z", "z"),
                ],
                vec![conformance::edge("race-a", "race-k")],
            );
            request.options.report_per_item = true;
            conformance::ingest_batch(store.as_ref(), &ctx, request).await
        })
    };

    // Commit only once the bulk insert waits on the held key.
    let waiting = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n FROM pg_stat_activity \
         WHERE wait_event_type = 'Lock' AND datname = current_database()"
            .to_owned(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let row = raw
            .query_one_raw(waiting.clone())
            .await
            .expect("the activity view reads")
            .expect("a count is one row");
        if row.try_get::<i64>("", "n").expect("the count is a number") > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the bulk insert never waited on the held key"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    holder.commit().await.expect("the holder commits");

    let outcome = writer
        .await
        .expect("the writer task does not panic")
        .expect("the batch lands over the other batch's row");
    assert_eq!(
        outcome.per_item_nodes,
        Some(vec![
            ItemOutcome::Inserted,
            ItemOutcome::Updated,
            ItemOutcome::Inserted
        ]),
        "the lost key is an update, the others are inserts"
    );
    let view = stand
        .store
        .get_node(&ctx, &"race-k".to_owned(), 10)
        .await
        .expect("the key reads");
    assert_eq!(view.name.as_deref(), Some("from the batch"));
    assert_eq!(
        view.envelope.version,
        Some(2),
        "the held row, then the batch"
    );
    assert_eq!(
        view.adjacency.len(),
        1,
        "the edge is attached to the row that won, not to a row that was never inserted"
    );
}

/// Two batches, each carrying one node and an edge to the other's node,
/// which it does not carry and so materializes as a phantom. Each batch
/// writes both keys, one as a node and one as a phantom; if either took its
/// locks out of key order -- its new nodes in bulk ahead of a phantom with a
/// smaller key -- the two would wait on each other and the server would
/// break the deadlock by failing one. Every batch must land (gears-rust
/// #5040, the ordering of #5042).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batches_whose_nodes_are_each_others_phantoms_all_land() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");

    for round in 0..30 {
        // `a` sorts before `z`: the first batch writes a node then a phantom,
        // the second a phantom then a node, both in key order.
        let (a, z) = (format!("cross-{round}-a"), format!("cross-{round}-z"));
        let writers: Vec<_> = [(a.clone(), z.clone()), (z.clone(), a.clone())]
            .into_iter()
            .map(|(carried, named)| {
                let store = Arc::clone(&stand.store);
                tokio::spawn(async move {
                    let scope = AccessScope::for_tenant(tenant);
                    let ctx = conformance::ctx(tenant, &scope, None);
                    conformance::ingest_batch(
                        store.as_ref(),
                        &ctx,
                        conformance::batch(
                            vec![conformance::node(&carried, "carried")],
                            vec![conformance::edge(&carried, &named)],
                        ),
                    )
                    .await
                })
            })
            .collect();
        for writer in writers {
            writer
                .await
                .expect("the writer task does not panic")
                .unwrap_or_else(|error| panic!("round {round}: a batch failed: {error:?}"));
        }
        for key in [&a, &z] {
            let view = stand
                .store
                .get_node(&ctx, key, 10)
                .await
                .expect("both keys exist");
            assert_eq!(
                view.adjacency.len(),
                2,
                "round {round}: {key} has both edges"
            );
        }
    }
}

/// The edge half of the lost race: an edge key another batch inserts between
/// this batch's up-front read and its bulk insert is written on the item path,
/// as an update of that row, and stays one row (gears-rust #5040). The other
/// batch is a plain transaction holding an uncommitted edge under the key the
/// store derives, so the bulk insert waits on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_lost_to_another_batch_during_the_bulk_insert_lands_as_an_update() {
    use graph_storage_sdk::models::ItemOutcome;
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};

    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("edge-race-a", "a"),
                conformance::node("edge-race-b", "b"),
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("the endpoints exist");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other batch");
    let type_uuid: Uuid = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT gts_type_uuid AS u FROM gts_type WHERE tenant_id = '{tenant}' \
                 AND gts_type_id = '{}'",
                conformance::LINK
            ),
        ))
        .await
        .expect("the type reads")
        .expect("the link type is registered")
        .try_get("", "u")
        .expect("a uuid");
    let mut spec = conformance::edge("edge-race-a", "edge-race-b");
    let key = graph_storage::domain::identity::derive_edge_key(type_uuid, &spec);

    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "INSERT INTO edge (tenant_id, edge_key, gts_edge_type_id, src_node_id, dst_node_id) \
             SELECT '{tenant}', '{key}', t.id, a.id, b.id FROM gts_type t, node a, node b \
             WHERE t.tenant_id = '{tenant}' AND t.gts_type_id = '{link}' \
             AND a.tenant_id = '{tenant}' AND a.node_key = 'edge-race-a' \
             AND b.tenant_id = '{tenant}' AND b.node_key = 'edge-race-b'",
            link = conformance::LINK
        ))
        .await
        .expect("the holder inserts the edge and keeps it uncommitted");

    spec.payload = Some(serde_json::json!({ "from": "the batch" }));
    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            let mut request = conformance::batch(Vec::new(), vec![spec]);
            request.options.report_per_item = true;
            conformance::ingest_batch(store.as_ref(), &ctx, request).await
        })
    };

    let waiting = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n FROM pg_stat_activity \
         WHERE wait_event_type = 'Lock' AND datname = current_database()"
            .to_owned(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let row = raw
            .query_one_raw(waiting.clone())
            .await
            .expect("the activity view reads")
            .expect("a count is one row");
        if row.try_get::<i64>("", "n").expect("the count is a number") > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the bulk insert never waited on the held edge"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    holder.commit().await.expect("the holder commits");

    let outcome = writer
        .await
        .expect("the writer task does not panic")
        .expect("the batch lands over the other batch's edge");
    assert_eq!(
        outcome.per_item_edges,
        Some(vec![ItemOutcome::Updated]),
        "the lost edge is an update"
    );
    let rows = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT count(*)::bigint AS n, max(payload ->> 'from') AS f FROM edge \
                 WHERE tenant_id = '{tenant}' AND edge_key = '{key}'"
            ),
        ))
        .await
        .expect("the edge reads")
        .expect("one row");
    assert_eq!(rows.try_get::<i64>("", "n").expect("a count"), 1, "one row");
    assert_eq!(
        rows.try_get::<String>("", "f").expect("a payload"),
        "the batch",
        "the batch's write is the one that stands"
    );
}

/// The ontology plus an edge type admitting only owned endpoints and a
/// reference node type -- a concrete type that edge type refuses.
async fn register_constrained_types(
    stand: &Stand,
    ctx: &graph_storage_sdk::plugin_api::StoreCtx<'_>,
) {
    let mut batch = conformance::ontology_batch();
    batch.push(graph_storage_sdk::models::TypeRegistration {
        type_id: conformance::OWNED_ONLY.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{}", conformance::OWNED_ONLY),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "src_types": [conformance::OWNED_FAMILY],
                "dst_types": [conformance::OWNED_FAMILY]
            },
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.edge.v1~cf.core.graph.static_edge.v1~" }]
        }),
    });
    batch.push(graph_storage_sdk::models::TypeRegistration {
        type_id: conformance::REFERENCE.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{}", conformance::REFERENCE),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.reference_node.v1~" }]
        }),
    });
    stand
        .store
        .register_types(ctx, batch)
        .await
        .expect("the ontology registers");
}

/// Return once some session of the stand's database waits on a row lock.
async fn until_a_session_waits_on_a_lock(raw: &sea_orm::DatabaseConnection, what: &str) {
    use sea_orm::ConnectionTrait as _;
    let waiting = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n FROM pg_stat_activity \
         WHERE wait_event_type = 'Lock' AND datname = current_database()"
            .to_owned(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let row = raw
            .query_one_raw(waiting.clone())
            .await
            .expect("the activity view reads")
            .expect("a count is one row");
        if row.try_get::<i64>("", "n").expect("the count is a number") > 0 {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A phantom the batch materializes, which another batch materializes as
/// a different type while this one waits on an earlier key, is refused as
/// the type change it now is -- not overwritten from the batch's up-front
/// read, which still saw the phantom (gears-rust #5864, a review finding).
///
/// The other batch is a plain transaction that holds the earlier key and
/// changes the phantom's type before it commits, which is what a
/// materializing batch holding that key does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phantom_materialized_meanwhile_is_not_overwritten_from_the_prefetch() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    register_constrained_types(&stand, &ctx).await;
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![conformance::node("mat-a", "a")],
            vec![conformance::edge("mat-a", "mat-z")],
        ),
    )
    .await
    .expect("the node and a phantom endpoint commit");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other batch");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "UPDATE node SET name = 'held' WHERE tenant_id = '{tenant}' AND node_key = 'mat-a'"
        ))
        .await
        .expect("the holder takes the earlier key");

    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            conformance::ingest_batch(
                store.as_ref(),
                &ctx,
                conformance::batch(
                    vec![
                        conformance::node("mat-a", "from the batch"),
                        conformance::node("mat-z", "materialized by the batch"),
                    ],
                    Vec::new(),
                ),
            )
            .await
        })
    };
    until_a_session_waits_on_a_lock(&raw, "the batch never waited on the earlier key").await;
    holder
        .execute_unprepared(&format!(
            "UPDATE node SET gts_node_type_id = (SELECT id FROM gts_type \
             WHERE tenant_id = '{tenant}' AND gts_type_id = '{}'), version = version + 1 \
             WHERE tenant_id = '{tenant}' AND node_key = 'mat-z'",
            conformance::REFERENCE
        ))
        .await
        .expect("the holder materializes the phantom as another type");
    holder.commit().await.expect("the holder commits");

    let outcome = writer.await.expect("the writer task does not panic");
    assert!(
        matches!(
            outcome,
            Err(graph_storage_sdk::plugin_api::GraphStoreError::Conflict { .. })
        ),
        "the batch meets a concrete node of another type: {outcome:?}"
    );
    let row = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT t.gts_type_id FROM node n JOIN gts_type t ON t.id = n.gts_node_type_id \
                 WHERE n.tenant_id = '{tenant}' AND n.node_key = 'mat-z'"
            ),
        ))
        .await
        .expect("the type reads")
        .expect("the node is there");
    assert_eq!(
        row.try_get::<String>("", "gts_type_id").expect("a type id"),
        conformance::REFERENCE,
        "the other batch's type stands"
    );
}

/// An edge to a phantom endpoint that another batch materializes, while
/// this one waits on an earlier key, into a type the edge does not admit is
/// refused -- not admitted on the up-front read's phantom (gears-rust #5864,
/// a review finding). The endpoint is then held under `FOR SHARE` until commit, so
/// the materialization cannot slip in after the check either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_endpoint_materialized_meanwhile_is_checked_as_its_new_type() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    register_constrained_types(&stand, &ctx).await;
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![conformance::node("lnk-a", "a")],
            vec![conformance::edge("lnk-a", "lnk-z")],
        ),
    )
    .await
    .expect("the node and a phantom endpoint commit");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other batch");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "UPDATE node SET name = 'held' WHERE tenant_id = '{tenant}' AND node_key = 'lnk-a'"
        ))
        .await
        .expect("the holder takes the earlier key");

    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            conformance::ingest_batch(
                store.as_ref(),
                &ctx,
                conformance::batch(
                    vec![conformance::node("lnk-a", "from the batch")],
                    vec![graph_storage_sdk::models::EdgeSpec {
                        type_id: conformance::OWNED_ONLY.to_owned(),
                        ..conformance::edge("lnk-a", "lnk-z")
                    }],
                ),
            )
            .await
        })
    };
    until_a_session_waits_on_a_lock(&raw, "the batch never waited on the earlier key").await;
    holder
        .execute_unprepared(&format!(
            "UPDATE node SET gts_node_type_id = (SELECT id FROM gts_type \
             WHERE tenant_id = '{tenant}' AND gts_type_id = '{}'), version = version + 1 \
             WHERE tenant_id = '{tenant}' AND node_key = 'lnk-z'",
            conformance::REFERENCE
        ))
        .await
        .expect("the holder materializes the endpoint as a type the edge refuses");
    holder.commit().await.expect("the holder commits");

    let outcome = writer.await.expect("the writer task does not panic");
    assert!(
        outcome.is_err(),
        "an owned-only edge to a reference node is refused: {outcome:?}"
    );
    let row = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT count(*)::bigint AS n FROM edge e JOIN gts_type t \
                 ON t.id = e.gts_edge_type_id WHERE e.tenant_id = '{tenant}' \
                 AND t.gts_type_id = '{}'",
                conformance::OWNED_ONLY
            ),
        ))
        .await
        .expect("the count reads")
        .expect("a count is one row");
    assert_eq!(
        row.try_get::<i64>("", "n").expect("a count"),
        0,
        "no edge the type refuses was written"
    );
}

/// A batch claims a source namespace only after the earlier keys pending in
/// its bulk run are inserted (gears-rust #5864, a review finding). Claiming first
/// held the namespace row while the batch then waited on an earlier key,
/// and a batch holding that key and needing the namespace deadlocked with
/// it. Here the batch waits on an earlier key held by another transaction,
/// and a third session must be able to claim the namespace meanwhile: the
/// batch has not taken it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_namespace_is_claimed_after_the_earlier_keys_of_the_run() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    register_constrained_types(&stand, &ctx).await;

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other batch");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "INSERT INTO node (tenant_id, node_key, gts_node_type_id, name) \
             SELECT '{tenant}', 'ns-a', id, 'held' FROM gts_type \
             WHERE tenant_id = '{tenant}' AND gts_type_id = '{}'",
            conformance::OWNED
        ))
        .await
        .expect("the holder takes the earlier key");

    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            let mirror = graph_storage_sdk::models::NodeSpec {
                node_key: "nsz:repo:1".to_owned(),
                type_id: conformance::REFERENCE.to_owned(),
                payload: Some(serde_json::json!({
                    "source": { "system": "nsz", "kind": "repo", "native_id": "1" }
                })),
                ..graph_storage_sdk::models::NodeSpec::default()
            };
            conformance::ingest_batch(
                store.as_ref(),
                &ctx,
                conformance::batch(vec![conformance::node("ns-a", "a"), mirror], Vec::new()),
            )
            .await
        })
    };
    until_a_session_waits_on_a_lock(&raw, "the batch never waited on the earlier key").await;

    let probe = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a third session");
    let claim = probe.begin().await.expect("the probe begins");
    claim
        .execute_unprepared("SET LOCAL lock_timeout = '2s'")
        .await
        .expect("the probe bounds its wait");
    let claimed = claim
        .execute_unprepared(&format!(
            "INSERT INTO source_namespace_owner (tenant_id, namespace, owner_principal) \
             VALUES ('{tenant}', 'nsz', 'probe') ON CONFLICT DO NOTHING"
        ))
        .await;
    claim.rollback().await.expect("the probe rolls back");
    holder.rollback().await.expect("the holder lets the key go");
    writer
        .await
        .expect("the writer task does not panic")
        .expect("the batch lands once the earlier key is let go");
    assert!(
        claimed.is_ok(),
        "the waiting batch must not hold the namespace yet: {claimed:?}"
    );
}

/// A stored edge a batch replays, which another transaction tombstones while
/// the batch waits on an earlier edge key, is revived from the row as it is
/// at the edge's turn -- not reported `Unchanged` from the batch's up-front
/// read, which still saw it live (gears-rust #5864, a review finding).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edge_tombstoned_meanwhile_is_revived_not_reported_unchanged() {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    let edges = || {
        vec![
            conformance::edge("rev-a", "rev-b"),
            conformance::edge("rev-a", "rev-c"),
        ]
    };
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("rev-a", "a"),
                conformance::node("rev-b", "b"),
                conformance::node("rev-c", "c"),
            ],
            edges(),
        ),
    )
    .await
    .expect("the edges commit");
    let mut keys: Vec<String> = stand
        .store
        .get_node(&ctx, &"rev-a".to_owned(), 10)
        .await
        .expect("the node reads")
        .adjacency
        .into_iter()
        .map(|entry| entry.edge_key)
        .collect();
    keys.sort();
    let (earlier, later) = (keys[0].clone(), keys[1].clone());

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection for the other transaction");
    let holder = raw.begin().await.expect("the holder begins");
    holder
        .execute_unprepared(&format!(
            "UPDATE edge SET payload = '{{\"held\": 1}}' \
             WHERE tenant_id = '{tenant}' AND edge_key = '{earlier}'"
        ))
        .await
        .expect("the holder takes the earlier edge key");

    let writer = {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            conformance::ingest_batch(
                store.as_ref(),
                &ctx,
                conformance::batch(Vec::new(), edges()),
            )
            .await
        })
    };
    until_a_session_waits_on_a_lock(&raw, "the batch never waited on the earlier edge key").await;
    holder
        .execute_unprepared(&format!(
            "UPDATE edge SET deleted_at = now() \
             WHERE tenant_id = '{tenant}' AND edge_key = '{later}'"
        ))
        .await
        .expect("the holder tombstones the later edge");
    holder.commit().await.expect("the holder commits");

    writer
        .await
        .expect("the writer task does not panic")
        .expect("the replay lands");
    let row = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!(
                "SELECT (deleted_at IS NULL) AS live FROM edge \
                 WHERE tenant_id = '{tenant}' AND edge_key = '{later}'"
            ),
        ))
        .await
        .expect("the edge reads")
        .expect("the edge is there");
    assert!(
        row.try_get::<bool>("", "live").expect("a flag"),
        "the replay revives the edge it names, as a replay after the delete does"
    );
}

/// A purge under a scope that names the node by id removes its edges too.
/// `id` is each entity's own column, so that scope applied to the edge delete
/// would match edges by `edge.id`, keep the node's incident edges, and the
/// foreign key would refuse the node. The edges go under the tenant part of
/// the scope, after the node was found under the whole of it.
#[tokio::test]
async fn a_purge_under_a_node_id_scope_removes_the_nodes_edges() {
    use toolkit_security::{ScopeConstraint, ScopeFilter, pep_properties};

    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    // Edges first, so the edge ids run ahead of the node ids: with equal ids,
    // a node-id filter applied to `edge.id` would match by coincidence.
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("filler-a", "a"),
                conformance::node("filler-b", "b"),
            ],
            (0..3)
                .map(|n| {
                    let mut filler = conformance::edge("filler-a", "filler-b");
                    filler.discriminator = Some(format!("f{n}"));
                    filler
                })
                .collect(),
        ),
    )
    .await
    .expect("the filler commits");
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("by-id", "by id"),
                conformance::node("by-id-peer", "peer"),
            ],
            vec![conformance::edge("by-id", "by-id-peer")],
        ),
    )
    .await
    .expect("the batch commits");
    let id = stand
        .store
        .resolve_node_ids(&ctx, &["by-id".to_owned()])
        .await
        .expect("the key resolves")[0]
        .1;
    {
        use sea_orm::ConnectionTrait as _;
        let raw = sea_orm::Database::connect(&stand.dsn)
            .await
            .expect("a plain connection");
        let clash = raw
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!(
                    "SELECT count(*)::bigint AS n FROM edge e JOIN node s ON s.id = e.src_node_id \
                     WHERE e.tenant_id = '{tenant}' AND s.node_key = 'by-id' AND e.id = {id}"
                ),
            ))
            .await
            .expect("reads")
            .expect("one row")
            .try_get::<i64>("", "n")
            .expect("a count");
        assert_eq!(
            clash, 0,
            "precondition: the node's edge has an id of its own"
        );
    }

    let by_id = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, tenant),
        ScopeFilter::eq(pep_properties::RESOURCE_ID, id),
    ])]);
    let narrow = conformance::ctx(tenant, &by_id, None);
    let outcome = stand
        .store
        .purge_node(
            &narrow,
            "by-id".to_owned(),
            graph_storage_sdk::models::PurgeOptions::default(),
        )
        .await
        .expect("the purge commits");
    assert_eq!((outcome.purged_nodes, outcome.purged_edges), (1, 1));
    let peer = stand
        .store
        .get_node(&ctx, &"by-id-peer".to_owned(), 10)
        .await
        .expect("the peer stays");
    assert!(peer.adjacency.is_empty(), "the edge went with the node");
}

/// A purge under a scope with no tenant constraint -- a direct store call;
/// the service always pins a write to one tenant -- removes the node's edges
/// in the node's own tenant rather than in an empty scope, so the foreign key
/// does not refuse the node (gears-rust #5906, a review finding).
#[tokio::test]
async fn a_purge_under_an_unconstrained_scope_removes_the_nodes_edges() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(
            vec![
                conformance::node("open-a", "a"),
                conformance::node("open-b", "b"),
            ],
            vec![conformance::edge("open-a", "open-b")],
        ),
    )
    .await
    .expect("the batch commits");

    let everything = AccessScope::allow_all();
    let open = conformance::ctx(tenant, &everything, None);
    let outcome = stand
        .store
        .purge_node(
            &open,
            "open-a".to_owned(),
            graph_storage_sdk::models::PurgeOptions::default(),
        )
        .await
        .expect("the purge commits");
    assert_eq!((outcome.purged_nodes, outcome.purged_edges), (1, 1));
    let peer = stand
        .store
        .get_node(&ctx, &"open-b".to_owned(), 10)
        .await
        .expect("the peer stays");
    assert!(peer.adjacency.is_empty(), "the edge went with the node");
}

/// An edge whose far endpoint the caller cannot see is left out of the
/// listing, and the listing still moves past it: the page that held it comes
/// back short and carries a cursor, and the next page goes on from there.
///
/// The endpoint is hidden the way `an_edge_whose_endpoint_is_hidden_is_not_found_and_stays_live`
/// hides one -- tombstoned by hand while its edge stays live -- which is the
/// shape a scope the caller does not hold also takes.
#[tokio::test]
async fn a_listed_edge_whose_far_endpoint_is_hidden_is_skipped() {
    use sea_orm::ConnectionTrait as _;
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = tenant_on(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    stand
        .store
        .register_types(&ctx, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    // One batch per edge, so storage ids follow the order written here.
    conformance::ingest_batch(
        stand.store.as_ref(),
        &ctx,
        conformance::batch(vec![conformance::node("hid-hub", "hub")], Vec::new()),
    )
    .await
    .expect("the hub commits");
    for far in ["hid-a", "hid-b", "hid-c"] {
        conformance::ingest_batch(
            stand.store.as_ref(),
            &ctx,
            conformance::batch(
                vec![conformance::node(far, far)],
                vec![conformance::edge("hid-hub", far)],
            ),
        )
        .await
        .expect("the edge commits");
    }
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hide the endpoint");
    raw.execute_unprepared(&format!(
        "UPDATE node SET deleted_at = now() \
         WHERE tenant_id = '{tenant}' AND node_key = 'hid-b'"
    ))
    .await
    .expect("the endpoint is tombstoned alone");

    let query = |after: Option<i64>| graph_storage_sdk::plugin_api::NodeEdgesQuery {
        node_key: "hid-hub".to_owned(),
        direction: graph_storage_sdk::models::Direction::Outgoing,
        edge_types: None,
        discriminators: Vec::new(),
        after,
        limit: 2,
    };
    let first = stand
        .store
        .list_node_edges(&ctx, &query(None))
        .await
        .expect("the first page answers");
    assert_eq!(
        first
            .items
            .iter()
            .map(|e| e.dst.as_str())
            .collect::<Vec<_>>(),
        vec!["hid-a"],
        "the edge to the hidden endpoint is skipped"
    );
    let after = first
        .next_after
        .expect("a short page still carries the position past the skipped edge");
    let second = stand
        .store
        .list_node_edges(&ctx, &query(Some(after)))
        .await
        .expect("the second page answers");
    assert_eq!(
        second
            .items
            .iter()
            .map(|e| e.dst.as_str())
            .collect::<Vec<_>>(),
        vec!["hid-c"]
    );
    assert_eq!(second.next_after, None, "and the listing ends");
}

// --- the offboarding fence against a write still running ---------------------

/// Wait until `n` sessions wait on a lock.
async fn waiting_on_locks(raw: &sea_orm::DatabaseConnection, n: i64) {
    use sea_orm::ConnectionTrait as _;
    let waiting = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n FROM pg_stat_activity \
         WHERE wait_event_type = 'Lock' AND datname = current_database()"
            .to_owned(),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let row = raw
            .query_one_raw(waiting.clone())
            .await
            .expect("the activity view reads")
            .expect("a count is one row");
        if row.try_get::<i64>("", "n").expect("the count is a number") >= n {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{n} sessions never waited on the held row"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Rows of `tenant` in `table` that also meet `and`, read outside the secure
/// ORM.
async fn rows_where(
    raw: &sea_orm::DatabaseConnection,
    table: &str,
    tenant: Uuid,
    and: &str,
) -> i64 {
    use sea_orm::ConnectionTrait as _;
    raw.query_one_raw(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        format!("SELECT count(*)::bigint AS n FROM {table} WHERE tenant_id = '{tenant}' {and}"),
    ))
    .await
    .expect("the count reads")
    .expect("a count is one row")
    .try_get::<i64>("", "n")
    .expect("the count is a number")
}

/// Rows of `tenant` left in `table`.
async fn rows_of(raw: &sea_orm::DatabaseConnection, table: &str, tenant: Uuid) -> i64 {
    rows_where(raw, table, tenant, "").await
}

/// A tenant with a graph, and a plain transaction holding `row` of it, so
/// the statements that take that row queue on it in the order the test
/// starts them.
async fn held(
    stand: &Stand,
    row: &str,
) -> (
    Uuid,
    sea_orm::DatabaseConnection,
    sea_orm::DatabaseTransaction,
) {
    use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
    let (tenant, _) = one_edge(stand).await;
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hold the row");
    let holder = raw.begin().await.expect("the holder begins");
    // Locked, not updated: a waiter on an updated row restarts its search
    // for the new version and may be overtaken, so only a row that stays the
    // same version keeps the waiters in the order they came.
    holder
        .execute_unprepared(&format!(
            "SELECT 1 FROM {row} AND tenant_id = '{tenant}' FOR UPDATE"
        ))
        .await
        .expect("the holder takes the row");
    (tenant, raw, holder)
}

/// The tenant's revision row, which every write and the fence take.
const REVISION_ROW: &str = "graph_meta WHERE key = 'graph_revision'";

/// One write of each kind the fence has to stop.
#[derive(Clone, Copy, Debug)]
enum LateWrite {
    Ingest,
    Delete,
    Transfer,
    Types,
}

impl LateWrite {
    const ALL: [Self; 4] = [Self::Ingest, Self::Delete, Self::Transfer, Self::Types];

    fn spawn(
        self,
        stand: &Stand,
        tenant: Uuid,
    ) -> tokio::task::JoinHandle<Result<(), graph_storage_sdk::plugin_api::GraphStoreError>> {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move {
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            match self {
                Self::Ingest => conformance::ingest_batch(
                    store.as_ref(),
                    &ctx,
                    conformance::batch(vec![conformance::node("late", "late")], Vec::new()),
                )
                .await
                .map(drop),
                Self::Delete => store
                    .soft_delete(
                        &ctx,
                        graph_storage_sdk::models::DeleteRequest::Node("edge-a".to_owned()),
                    )
                    .await
                    .map(drop),
                Self::Transfer => store
                    .transfer_source_namespace(&ctx, "late-ns", "someone")
                    .await
                    .map(drop),
                Self::Types => store
                    .register_types(&ctx, vec![conformance::requirement_v1()])
                    .await
                    .map(drop),
            }
        })
    }

    /// What the write leaves when it commits: zero before it ran.
    async fn trace(self, raw: &sea_orm::DatabaseConnection, tenant: Uuid) -> i64 {
        match self {
            Self::Ingest => rows_where(raw, "node", tenant, "AND node_key = 'late'").await,
            Self::Delete => rows_where(raw, "node", tenant, "AND deleted_at IS NOT NULL").await,
            Self::Transfer => rows_of(raw, "source_namespace_owner", tenant).await,
            Self::Types => {
                rows_where(
                    raw,
                    "gts_type",
                    tenant,
                    &format!("AND gts_type_id = '{}'", conformance::EVOLVING),
                )
                .await
            }
        }
    }
}

fn fence(
    stand: &Stand,
    tenant: Uuid,
) -> tokio::task::JoinHandle<
    Result<
        graph_storage_sdk::models::TenantOffboarding,
        graph_storage_sdk::plugin_api::GraphStoreError,
    >,
> {
    let store = Arc::clone(&stand.store);
    tokio::spawn(async move { conformance::fence(store.as_ref(), tenant, None).await })
}

/// The fence takes the revision row first; a write of each kind already
/// running, waiting on the same row, then sees the committed fence and rolls
/// back. Nothing of it is left, though it had done its work before it waited.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_still_running_when_the_fence_lands_is_refused() {
    let Some(stand) = stand().await else {
        return;
    };
    for write in LateWrite::ALL {
        let (tenant, raw, holder) = held(&stand, REVISION_ROW).await;
        assert_eq!(write.trace(&raw, tenant).await, 0, "{write:?}");

        let fenced = fence(&stand, tenant);
        waiting_on_locks(&raw, 1).await;
        let late = write.spawn(&stand, tenant);
        waiting_on_locks(&raw, 2).await;
        holder.commit().await.expect("the holder lets go");

        fenced
            .await
            .expect("the fence task does not panic")
            .expect("the fence commits");
        let refused = late
            .await
            .expect("the write task does not panic")
            .expect_err("a write that waited behind the fence is refused");
        assert!(
            matches!(
                refused,
                graph_storage_sdk::plugin_api::GraphStoreError::TenantFenced { tenant: t } if t == tenant
            ),
            "{write:?}: {refused:?}"
        );
        assert_eq!(
            write.trace(&raw, tenant).await,
            0,
            "{write:?}: the refused write left nothing"
        );
    }
}

/// The write takes the revision row first: the fence waits for it, the
/// write sees no fence and commits, and the deletion that starts after the
/// fence removes its rows with the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_that_commits_before_the_fence_is_removed_with_the_rest() {
    let Some(stand) = stand().await else {
        return;
    };
    let (tenant, raw, holder) = held(&stand, REVISION_ROW).await;

    let late = LateWrite::Ingest.spawn(&stand, tenant);
    waiting_on_locks(&raw, 1).await;
    let fenced = fence(&stand, tenant);
    waiting_on_locks(&raw, 2).await;
    holder.commit().await.expect("the holder lets go");

    late.await
        .expect("the write task does not panic")
        .expect("a write ahead of the fence commits");
    fenced
        .await
        .expect("the fence task does not panic")
        .expect("the fence commits");
    let finished = graph_storage::domain::offboarding::run_pending(
        stand.store.as_ref(),
        graph_storage::domain::offboarding::WorkerSettings::new(
            std::num::NonZeroU64::new(2).expect("a batch"),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(30),
        )
        .expect("valid settings"),
        &mut graph_storage::domain::offboarding::WorkerState::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    assert_eq!(finished, 1, "the worker's pass finishes the deletion");

    for table in [
        "edge",
        "node",
        "gts_type",
        "graph_meta",
        "ingest_idempotency",
        "scope_registry",
        "source_namespace_owner",
    ] {
        assert_eq!(rows_of(&raw, table, tenant).await, 0, "{table} is empty");
    }
    assert_eq!(
        rows_of(&raw, "tenant_offboarding", tenant).await,
        1,
        "the record stays"
    );
}

/// Two steps for one tenant at once take turns on its record: held while
/// both queue, then released, they each remove their own rows, and the
/// counts on the record are every row once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_steps_for_one_tenant_take_turns_and_count_each_row_once() {
    let Some(stand) = stand().await else {
        return;
    };
    let (tenant, _) = one_edge(&stand).await;
    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection");
    let types = rows_of(&raw, "gts_type", tenant).await;
    let meta = rows_of(&raw, "graph_meta", tenant).await;
    conformance::fence(stand.store.as_ref(), tenant, None)
        .await
        .expect("the fence");

    let holder = {
        use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
        let holder = raw.begin().await.expect("the holder begins");
        holder
            .execute_unprepared(&format!(
                "SELECT 1 FROM tenant_offboarding WHERE tenant_id = '{tenant}' FOR UPDATE"
            ))
            .await
            .expect("the holder takes the record");
        holder
    };
    let step = || {
        let store = Arc::clone(&stand.store);
        tokio::spawn(async move { store.offboard_step(tenant, std::num::NonZeroU64::MIN).await })
    };
    let (one, two) = (step(), step());
    waiting_on_locks(&raw, 2).await;
    holder.commit().await.expect("the holder lets go");
    for task in [one, two] {
        task.await
            .expect("the step task does not panic")
            .expect("a step runs");
    }

    let (record, _) = conformance::offboard_fully(stand.store.as_ref(), tenant, 1).await;
    let removed = record.removed;
    assert_eq!(removed.edges, 1, "{removed:?}");
    assert_eq!(removed.nodes, 2, "{removed:?}");
    assert_eq!(i64::try_from(removed.types).unwrap(), types, "{removed:?}");
    assert_eq!(i64::try_from(removed.meta).unwrap(), meta, "{removed:?}");
}

/// A parent tenant reading its subtree stops seeing a child the moment the
/// child is fenced, though the child's rows are still there: the read's
/// scope loses the tenants whose deletion is running.
#[tokio::test]
async fn a_parent_reading_its_subtree_does_not_see_a_fenced_child() {
    use graph_storage_sdk::GraphStorageClientV1 as _;
    let Some(stand) = stand().await else {
        return;
    };
    let (parent, child) = (Uuid::now_v7(), Uuid::now_v7());
    // The fence below is made in the store, as on another replica; this
    // service reads the fenced tenants again on every request, as it would
    // once their refresh is over.
    let harness = support::Harness::over_store_seeing_every_fence(
        Arc::clone(&stand.store) as Arc<dyn GraphStoreV1>,
        Arc::new(graph_storage::infra::fake_store::FakeGraphStore::new()),
        Arc::new(support::WithChildren(vec![child])),
        GraphStorageConfig::default(),
    );
    let client = graph_storage::domain::local_client::GraphStorageLocalClient::new(Arc::clone(
        &harness.services,
    ));
    let member = |tenant: Uuid| {
        toolkit_security::SecurityContext::builder()
            .subject_id(Uuid::now_v7())
            .subject_tenant_id(tenant)
            .build()
            .expect("a valid security context")
    };
    let (in_parent, in_child) = (member(parent), member(child));
    client
        .register_types(&in_child, conformance::ontology_batch())
        .await
        .expect("the ontology registers");
    client
        .ingest(
            &in_child,
            conformance::batch(vec![conformance::node("childs", "childs")], Vec::new()),
        )
        .await
        .expect("the child's graph lands");
    client
        .get_node(&in_parent, &"childs".to_owned(), None)
        .await
        .expect("the parent reads its child before the fence");

    conformance::fence(stand.store.as_ref(), child, None)
        .await
        .expect("the fence");
    let hidden = client
        .get_node(&in_parent, &"childs".to_owned(), None)
        .await
        .expect_err("the fenced child is out of the parent's reads");
    assert_eq!(hidden.status_code(), 404, "{hidden:?}");
}

/// A replayed ingest wrote nothing, so it does not take the tenant's
/// revision row: it answers from its receipt while another transaction
/// holds that row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replayed_ingest_does_not_wait_on_the_revision_row() {
    let Some(stand) = stand().await else {
        return;
    };
    let (tenant, _) = one_edge(&stand).await;
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let keyed = || {
        let mut request = conformance::batch(vec![conformance::node("keyed", "keyed")], Vec::new());
        request.idempotency_key = Some("replayed".to_owned());
        request
    };
    conformance::ingest_batch(stand.store.as_ref(), &ctx, keyed())
        .await
        .expect("the first ingest records its receipt");

    let raw = sea_orm::Database::connect(&stand.dsn)
        .await
        .expect("a plain connection to hold the row");
    let holder = {
        use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
        let holder = raw.begin().await.expect("the holder begins");
        holder
            .execute_unprepared(&format!(
                "SELECT 1 FROM {REVISION_ROW} AND tenant_id = '{tenant}' FOR UPDATE"
            ))
            .await
            .expect("the holder takes the revision row");
        holder
    };
    let replayed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        conformance::ingest_batch(stand.store.as_ref(), &ctx, keyed()),
    )
    .await
    .expect("a replay does not wait on the held row")
    .expect("the replay answers");
    assert!(replayed.replayed);
    holder.rollback().await.expect("the holder lets go");
}

// --- the gear itself: init, then its lifecycle entry ---------------------------

/// The gear's configuration, as a deployment hands it.
struct GearConfig(serde_json::Value);

impl toolkit::config::ConfigProvider for GearConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        (gear_name == "graph-storage").then_some(&self.0)
    }
}

/// The gear as the platform starts it -- `init`, then its lifecycle entry --
/// finishes a deletion fenced before it started: the worker is wired, not
/// only the function it runs.
#[tokio::test]
async fn the_started_gear_removes_a_fenced_tenants_graph() {
    use toolkit::Gear as _;
    use toolkit::lifecycle::Runnable as _;

    let Some(stand) = stand().await else { return };
    let tenant = tenant_on(&stand).await;
    conformance::seed_everything(stand.store.as_ref(), tenant).await;
    conformance::fence(stand.store.as_ref(), tenant, None)
        .await
        .expect("the fence");

    let hub = Arc::new(toolkit::client_hub::ClientHub::new());
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(support::AllowInOwnTenant));
    let ctx = toolkit::GearCtx::new(
        "graph-storage",
        Uuid::new_v4(),
        Arc::new(GearConfig(serde_json::json!({
            "config": {
                "embedding_provider": "fake",
                "offboarding_interval_secs": 1,
            }
        }))),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new((*stand.db).clone()));
    let gear = Arc::new(graph_storage::gear::GraphStorage::default());
    gear.init(&ctx).await.expect("the gear starts");

    let cancel = tokio_util::sync::CancellationToken::new();
    let running = tokio::spawn(Arc::clone(&gear).run(cancel.clone()));
    let finished = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let record = conformance::record_of(stand.store.as_ref(), tenant)
                .await
                .expect("the record stays");
            if record.is_complete() {
                return record;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the gear's worker finishes the deletion");
    cancel.cancel();
    running
        .await
        .expect("the worker task ends")
        .expect("the worker ends cleanly when cancelled");

    assert!(finished.removed.total() > 0, "{finished:?}");
}
