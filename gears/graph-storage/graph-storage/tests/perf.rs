#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The § 6.1 retrieval scenarios, timed on a seeded reference graph.
//!
//! Opt-in: seeding the graph the criteria name takes minutes, so this lane
//! runs only when `GEARS_GRAPH_PERF` is set, alongside the same
//! `PostgreSQL` 19 + pgvector image the conformance lane needs. Without it
//! the cases skip -- loudly enough to be noticed in the output, quietly
//! enough not to make `cargo test` a coffee break.
//!
//! What is measured is the gear, not a deployment: the store port and the
//! engine port directly, under the same admission bounds the service applies.
//! Query embedding is excluded exactly as `nfr-search-latency` says, by
//! embedding the query text once outside the timed loop. The gateway, the
//! PDP round trip and JSON serialization are excluded too -- they belong to
//! the deployment's budget, and measuring them here would report someone
//! else's number as this gear's.
//!
//! ```text
//! GEARS_GRAPH_PERF=1 GEARS_TEST_PG_GRAPH_IMAGE=pg19-pgvector-test:latest \
//!   cargo test --release -p cf-gears-graph-storage --test perf -- --nocapture
//! ```
//!
//! Scale is `GEARS_GRAPH_PERF_SCALE` (default 1.0 = the criteria's 100k
//! nodes / 500k edges). A smaller scale reports honestly that it ran
//! smaller: a latency at 10k nodes is not evidence about 100k, and at any
//! scale but 1.0 no threshold is asserted at all.

/// The suite's fixtures: the same ontology and context helpers, so the graph
/// this lane times is the graph the conformance cases describe. Only part of
/// it is used from this binary, hence the allowance.
#[allow(dead_code)]
mod conformance;

use std::sync::Arc;
use std::time::{Duration, Instant};

use graph_storage::config::{GraphStorageConfig, HopStrategy};
use graph_storage::infra::engine::PgGraphEngine;
use graph_storage::infra::storage::migrations::Migrator;
use graph_storage::infra::store::PgGraphStore;
use graph_storage_sdk::models::{
    Direction, EdgeSpec, HopBudget, IngestOptions, IngestRequest, NodeSpec, ProjectionRequest,
    RemainingBudget, SearchMode, SearchRequest,
};
use graph_storage_sdk::plugin_api::{
    EmbeddingPlan, ExpandRequest, GraphEngineV1, GraphStoreV1, StoreCtx, VectorArm,
};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;
use tokio_util::sync::CancellationToken;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

struct Stand {
    store: Arc<PgGraphStore>,
    engine: PgGraphEngine,
    _container: ContainerAsync<Postgres>,
}

/// The graph the criteria name, scaled.
struct Shape {
    nodes: usize,
    edges: usize,
    scale: f64,
}

impl Shape {
    fn from_env() -> Self {
        let scale = std::env::var("GEARS_GRAPH_PERF_SCALE")
            .ok()
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.001, 10.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self {
            nodes: (100_000.0 * scale) as usize,
            edges: (500_000.0 * scale) as usize,
            scale,
        }
    }

    /// What the numbers below are evidence about.
    fn caveat(&self) -> String {
        if (self.scale - 1.0).abs() < f64::EPSILON {
            "the reference graph of the criteria (100k nodes / 500k edges)".to_owned()
        } else {
            format!(
                "a graph of {} nodes / {} edges -- scale {:.3} of the criteria's, so these \
                 numbers bound nothing about the reference graph",
                self.nodes, self.edges, self.scale
            )
        }
    }
}

fn enabled() -> bool {
    std::env::var("GEARS_GRAPH_PERF").is_ok_and(|value| !value.is_empty())
}

fn graph_image() -> Option<(String, String)> {
    let image = std::env::var("GEARS_TEST_PG_GRAPH_IMAGE").ok()?;
    let (name, tag) = image.rsplit_once(':').unwrap_or((image.as_str(), "latest"));
    Some((name.to_owned(), tag.to_owned()))
}

async fn stand() -> Option<Stand> {
    if !enabled() {
        eprintln!(
            "perf lane skipped: set GEARS_GRAPH_PERF=1 (and GEARS_TEST_PG_GRAPH_IMAGE) to run it"
        );
        return None;
    }
    let (name, tag) = graph_image()?;
    let container = test_containers::postgres_graph()
        .with_name(name)
        .with_tag(tag)
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "graph")
        .start()
        .await
        .expect("the graph image starts");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    let dsn = format!("postgres://user:pass@127.0.0.1:{port}/graph");
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(8),
            min_conns: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("connect");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations apply");
    let db = Arc::new(db);
    let pgq = graph_storage::infra::engine::probe_pgq(&db).await;
    let store = Arc::new(PgGraphStore::new(
        Arc::clone(&db),
        GraphStorageConfig {
            traversal_hop: HopStrategy::Pgq,
            ..GraphStorageConfig::default()
        },
        pgq,
    ));
    let engine = PgGraphEngine::new(Arc::clone(&store));
    Some(Stand {
        store,
        engine,
        _container: container,
    })
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Percentile of a sample, nearest-rank. Small samples are what an
/// interactive latency budget is judged on in practice, and a nearest-rank
/// p95 over 40 runs is honest about being one of the two slowest.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort_unstable();
    let rank = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len());
    samples[rank - 1]
}

struct Measured {
    p50: Duration,
    p95: Duration,
    worst: Duration,
    runs: usize,
}

impl std::fmt::Display for Measured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50 {:>7.1} ms   p95 {:>7.1} ms   max {:>7.1} ms   ({} runs)",
            self.p50.as_secs_f64() * 1000.0,
            self.p95.as_secs_f64() * 1000.0,
            self.worst.as_secs_f64() * 1000.0,
            self.runs
        )
    }
}

async fn measure<F, Fut>(runs: usize, mut once: F) -> Measured
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Warm first: the criteria say "warm indexes", and the first call of a
    // shape pays for plan and cache population that no interactive request
    // after it pays again.
    once(0).await;
    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let started = Instant::now();
        once(run).await;
        samples.push(started.elapsed());
    }
    Measured {
        p50: percentile(&mut samples, 0.50),
        p95: percentile(&mut samples, 0.95),
        worst: *samples.last().expect("at least one run"),
        runs,
    }
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// The vocabulary the seeded documents are drawn from. Fixed, so a query has
/// a predictable selectivity: `severity` filters a quarter of the graph,
/// `"authentication"` matches roughly a fifth of the text.
const TOPICS: [&str; 5] = [
    "authentication service",
    "payment ledger",
    "search index",
    "message broker",
    "deployment pipeline",
];
const SEVERITIES: [&str; 4] = ["low", "medium", "high", "critical"];

fn seeded_type() -> graph_storage_sdk::models::TypeRegistration {
    graph_storage_sdk::models::TypeRegistration {
        type_id: conformance::OWNED.to_owned(),
        schema: serde_json::json!({
            "$id": format!("gts://{}", conformance::OWNED),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "x-gts-traits": {
                "full_text_search": ["/name", "/payload/summary"],
                "vector_search": ["/payload/summary"],
                "index": ["/payload/severity"]
            },
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~" },
                { "type": "object", "properties": {
                    "payload": { "type": "object", "properties": {
                        "summary": { "type": "string" },
                        "severity": { "type": "string" }
                    }}
                }}
            ]
        }),
    }
}

/// Deterministic pseudo-randomness: the same graph every run, so two runs of
/// this lane are comparable and a regression is a regression rather than a
/// different graph.
fn mix(seed: usize) -> usize {
    let mut x = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    x
}

fn seeded_node(index: usize) -> NodeSpec {
    let topic = TOPICS[mix(index) % TOPICS.len()];
    let severity = SEVERITIES[mix(index + 7) % SEVERITIES.len()];
    NodeSpec {
        node_key: format!("perf-{index}"),
        type_id: conformance::OWNED.to_owned(),
        name: Some(format!("finding {index} in the {topic}")),
        payload: Some(serde_json::json!({
            "summary": format!("a {severity} finding about the {topic}, number {index}"),
            "severity": severity,
        })),
        ..NodeSpec::default()
    }
}

/// Edges with a degree distribution rather than a uniform one: a tenth of the
/// nodes are hubs and take most of the edges, because a traversal budget is
/// only interesting where the graph is dense.
fn seeded_edge(index: usize, nodes: usize) -> EdgeSpec {
    let src = mix(index * 3).max(1) % nodes;
    // A tenth of the nodes are hubs: a traversal budget is only interesting
    // where the graph is dense.
    let hub = (mix(index * 5 + 1) % nodes.max(1)).div_euclid(10);
    let dst = if index.is_multiple_of(3) {
        hub
    } else {
        mix(index * 7 + 2) % nodes
    };
    EdgeSpec {
        type_id: conformance::LINK.to_owned(),
        src_node_key: format!("perf-{src}"),
        dst_node_key: format!("perf-{}", if dst == src { (dst + 1) % nodes } else { dst }),
        discriminator: Some(format!("e{index}")),
        ..EdgeSpec::default()
    }
}

/// Seed the graph and report what the write path cost while doing it.
///
/// Two numbers come out of this: throughput with embedding **off**, which is
/// what `nfr-ingest-throughput` measures, and the wall clock of the whole
/// seeding, which is not a criterion and is reported so the lane's own cost
/// is visible.
async fn seed(stand: &Stand, ctx: &StoreCtx<'_>, shape: &Shape, embed: bool) -> (Duration, usize) {
    let store = stand.store.as_ref();
    let mut types = conformance::ontology_batch();
    types.retain(|registration| registration.type_id != conformance::OWNED);
    types.push(seeded_type());
    store
        .register_types(ctx, types)
        .await
        .expect("the ontology registers");

    let coordinator = conformance::coordinator();
    let mut written = 0usize;
    let started = Instant::now();

    for chunk in (0..shape.nodes).collect::<Vec<_>>().chunks(2_000) {
        let nodes: Vec<NodeSpec> = chunk.iter().map(|index| seeded_node(*index)).collect();
        let request = IngestRequest {
            nodes,
            edges: Vec::new(),
            options: IngestOptions {
                embed: Some(embed),
                ..IngestOptions::default()
            },
            replace_scope: None,
            idempotency_key: None,
        };
        let plan = plan_for(store, ctx, &request, &coordinator).await;
        written += request.nodes.len();
        store
            .ingest(ctx, request, plan)
            .await
            .expect("nodes commit");
    }

    for chunk in (0..shape.edges).collect::<Vec<_>>().chunks(5_000) {
        let edges: Vec<EdgeSpec> = chunk
            .iter()
            .map(|index| seeded_edge(*index, shape.nodes))
            .collect();
        let request = IngestRequest {
            nodes: Vec::new(),
            edges,
            options: IngestOptions {
                embed: Some(false),
                create_phantoms: Some(false),
                ..IngestOptions::default()
            },
            replace_scope: None,
            idempotency_key: None,
        };
        written += request.edges.len();
        store
            .ingest(
                ctx,
                request,
                EmbeddingPlan {
                    epoch: Some(conformance::EPOCH),
                    nodes: Vec::new(),
                },
            )
            .await
            .expect("edges commit");
    }
    (started.elapsed(), written)
}

async fn plan_for(
    store: &impl GraphStoreV1,
    ctx: &StoreCtx<'_>,
    request: &IngestRequest,
    coordinator: &graph_storage::domain::embedding::EmbeddingCoordinator,
) -> EmbeddingPlan {
    let mut records = std::collections::BTreeMap::new();
    if let Ok(record) = store.get_type(ctx, &conformance::OWNED.to_owned()).await {
        records.insert(conformance::OWNED.to_owned(), record);
    }
    let keys: Vec<String> = request.nodes.iter().map(|n| n.node_key.clone()).collect();
    let current = store
        .embedding_state(ctx, &keys)
        .await
        .expect("embedding state is readable");
    let nodes = coordinator
        .plan(
            &request.nodes,
            request.options.embed.unwrap_or(true),
            |node| graph_storage::domain::embedding::declared_paths(&records, node),
            &current,
            RemainingBudget::starting_now(Duration::from_mins(10)),
            CancellationToken::new(),
        )
        .await
        .expect("the deterministic provider always embeds");
    EmbeddingPlan {
        epoch: Some(conformance::EPOCH),
        nodes,
    }
}

// ---------------------------------------------------------------------------
// The scenarios
// ---------------------------------------------------------------------------

/// The four retrieval scenarios of § 6.1, on one seeded graph.
///
/// One test rather than four because the graph is the expensive part: seeding
/// it once and asking it four questions is the difference between minutes and
/// half an hour, and the scenarios are not independent of each other anyway
/// -- they share the indexes they are warm against.
#[tokio::test]
async fn the_retrieval_scenarios_answer_within_their_budgets() {
    let Some(stand) = stand().await else {
        return;
    };
    let shape = Shape::from_env();
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let store = stand.store.as_ref();

    let (seeding, rows) = seed(&stand, &ctx, &shape, true).await;
    println!(
        "\nseeded {rows} rows ({} nodes, {} edges) in {:.1} s -- {}",
        shape.nodes,
        shape.edges,
        seeding.as_secs_f64(),
        shape.caveat()
    );

    // --- scenario 1: hybrid narrowing ------------------------------------
    //
    // The query text is embedded once, outside the loop: `nfr-search-latency`
    // excludes query embedding, and including it would report the fake
    // provider's speed as this gear's.
    let query = "a critical finding about the authentication service";
    let vector = conformance::coordinator()
        .embed_query(
            query,
            RemainingBudget::starting_now(Duration::from_secs(30)),
            CancellationToken::new(),
        )
        .await
        .expect("the query embeds");
    let hybrid = measure(40, |_| {
        let vector = vector.clone();
        async {
            let hits = store
                .search(
                    &ctx,
                    SearchRequest {
                        mode: SearchMode::Hybrid,
                        query: Some(query.to_owned()),
                        arm_limit: 50,
                        limit: 50,
                        type_patterns: Vec::new(),
                    },
                    Some(VectorArm {
                        query_vector: vector,
                        epoch: conformance::EPOCH,
                    }),
                )
                .await
                .expect("hybrid search answers");
            assert!(!hits.hits.is_empty(), "the query must retrieve something");
        }
    })
    .await;
    println!("hybrid search (arm limit 50)      {hybrid}");

    // --- scenario 2: the criteria table ----------------------------------
    // Filtered on a declared payload path and ordered, which is the shape
    // the scenario describes -- an unfiltered first page would measure the
    // table's ceiling, not the query behind it.
    let table_request = conformance::projection(
        &[conformance::OWNED],
        "payload/severity eq 'critical'",
        &[("node_key", toolkit_odata::SortDir::Asc)],
    );
    let table = measure(40, |_| {
        let request = ProjectionRequest {
            type_set: table_request.type_set.clone(),
            query: table_request.query.clone(),
        };
        async {
            let page = store
                .project_table(&ctx, request)
                .await
                .expect("the projection answers");
            assert!(!page.items.is_empty(), "the filter must match something");
        }
    })
    .await;
    println!("criteria table (filter + page 50) {table}");

    // --- scenario 3: bounded traversal with edge-type filtering ----------
    let seeds: Vec<String> = (0..8).map(|i| format!("perf-{}", i * 977)).collect();
    let seed_ids: Vec<graph_storage_sdk::models::NodeId> = store
        .resolve_node_ids(&ctx, &seeds)
        .await
        .expect("the seeds resolve")
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    assert!(!seed_ids.is_empty(), "the seeds must exist in the graph");
    let type_set = store
        .resolve_type_set(&ctx, &[conformance::LINK.to_owned()])
        .await
        .expect("the edge type resolves");
    let engine = &stand.engine;
    let reader = &ctx;
    let traversal = measure(30, |_| {
        let frontier = seed_ids.clone();
        let edge_types = Some(type_set.clone());
        async move {
            let mut reached = frontier;
            for _ in 0..3 {
                let response = engine
                    .expand(
                        reader,
                        ExpandRequest {
                            frontier: reached.clone(),
                            direction: Direction::Either,
                            edge_types: edge_types.clone(),
                            labels: None,
                            budget: HopBudget {
                                max_frontier: 1_000,
                                max_edges_scanned: 50_000,
                            },
                        },
                    )
                    .await
                    .expect("the hop runs");
                reached = response.reached;
                reached.sort_unstable();
                reached.dedup();
                reached.truncate(1_000);
                if reached.is_empty() {
                    break;
                }
            }
        }
    })
    .await;
    println!("depth-3 typed traversal           {traversal}");

    // --- scenario 4: the depth-3 UI neighborhood -------------------------
    //
    // The same three hops plus the hydration the UI needs, which is what
    // `nfr-traversal-latency` actually budgets: a neighborhood nobody can
    // render is not the scenario.
    let root = store
        .resolve_node_ids(&ctx, &["perf-1".to_owned()])
        .await
        .expect("the root resolves")
        .first()
        .expect("the root exists")
        .1;
    let neighborhood = measure(30, |_| async {
        let mut frontier = vec![root];
        let mut visited = vec![root];
        for _ in 0..3 {
            let response = engine
                .expand(
                    reader,
                    ExpandRequest {
                        frontier: frontier.clone(),
                        direction: Direction::Either,
                        edge_types: None,
                        labels: None,
                        budget: HopBudget {
                            max_frontier: 1_000,
                            max_edges_scanned: 50_000,
                        },
                    },
                )
                .await
                .expect("the hop runs");
            frontier = response.reached;
            frontier.sort_unstable();
            frontier.dedup();
            frontier.retain(|id| !visited.contains(id));
            visited.extend(frontier.iter().copied());
            visited.truncate(1_000);
            if frontier.is_empty() || visited.len() >= 1_000 {
                break;
            }
        }
        let hydrated = store
            .hydrate_nodes(reader, &visited)
            .await
            .expect("the neighborhood hydrates");
        assert!(!hydrated.is_empty(), "a neighborhood must have nodes in it");
    })
    .await;
    println!("depth-3 neighborhood (budget 1k)  {neighborhood}\n");

    // The thresholds, asserted only at full scale: a latency measured on a
    // tenth of the graph is not evidence about the graph the criteria name,
    // and asserting it there would be a green light nobody earned.
    if (shape.scale - 1.0).abs() < f64::EPSILON {
        assert!(
            hybrid.p95 <= Duration::from_millis(500),
            "nfr-search-latency: hybrid p95 {hybrid}"
        );
        assert!(
            neighborhood.p95 <= Duration::from_secs(1),
            "nfr-traversal-latency: neighborhood p95 {neighborhood}"
        );
        assert!(
            traversal.p95 <= Duration::from_secs(1),
            "typed traversal p95 {traversal}"
        );
    }
}

/// `nfr-ingest-throughput`: 10,000 nodes and 20,000 edges, embedding
/// excluded, in 60 seconds or less.
///
/// Separate from the retrieval lane because the criterion is explicit that
/// embedding is excluded, and the retrieval graph needs vectors -- timing
/// both on one seeding would report a number the criterion does not ask for.
#[tokio::test]
async fn a_producer_sized_batch_lands_inside_its_budget() {
    let Some(stand) = stand().await else {
        return;
    };
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = conformance::ctx(tenant, &scope, None);
    let shape = Shape {
        nodes: 10_000,
        edges: 20_000,
        scale: 1.0,
    };

    let (elapsed, rows) = seed(&stand, &ctx, &shape, false).await;
    println!(
        "\ningest, embedding excluded: {rows} rows (10k nodes + 20k edges) in {:.1} s\n",
        elapsed.as_secs_f64()
    );
    assert!(
        elapsed <= Duration::from_mins(1),
        "nfr-ingest-throughput: 10k nodes + 20k edges took {:.1} s",
        elapsed.as_secs_f64()
    );
}
