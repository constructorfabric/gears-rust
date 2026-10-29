#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The § 6.1 retrieval scenarios, timed on a seeded reference graph.
//!
//! Opt-in: seeding the graph the criteria name takes minutes, so this lane
//! runs only when `GEARS_GRAPH_PERF` is set, alongside the same
//! `PostgreSQL` + pgvector image the conformance lane needs. Without it
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
//! GEARS_GRAPH_PERF=1 GEARS_TEST_PG_GRAPH_IMAGE=pgvector/pgvector:0.8.7-pg18 \
//!   cargo test --release -p cf-gears-graph-storage --test perf -- --nocapture
//! ```
//!
//! Scale is `GEARS_GRAPH_PERF_SCALE` (default 1.0 = the criteria's 100k
//! nodes / 500k edges). A smaller scale reports honestly that it ran
//! smaller: a latency at 10k nodes is not evidence about 100k, and at any
//! scale but 1.0 no threshold is asserted at all.
//!
//! Two more knobs turn one run into a benchmark worth comparing with the
//! next (method and reporting: `docs/PERF.md`):
//!
//! - `GEARS_GRAPH_PERF_REPEAT` (default 1) times every scenario that many
//!   times on the one seeded graph, so a run reports its own spread.
//! - `GEARS_GRAPH_PERF_RTT_MS` (default `0`), a comma list of milliseconds
//!   added to every round trip to the database with `netem`, applied from a
//!   `nicolaka/netshoot` container in the database container's network
//!   namespace. How p50 grows with it says how many sequential round trips a
//!   request makes, which a localhost run cannot show.
//!
//! Every timing is printed as one `RESULT` line, which
//! `scripts/perf_summary.py` turns into the tables the method describes.

/// The suite's fixtures: the same ontology and context helpers, so the graph
/// this lane times is the graph the conformance cases describe. Only part of
/// it is used from this binary, hence the allowance.
#[allow(dead_code)]
mod conformance;

use std::sync::Arc;
use std::time::{Duration, Instant};

use graph_storage::config::GraphStorageConfig;
use graph_storage::infra::engine::PgGraphEngine;
use graph_storage::infra::storage::migrations::Migrator;
use graph_storage::infra::store::PgGraphStore;
use graph_storage_sdk::models::{
    Direction, EdgeSpec, HopBudget, IngestOptions, IngestRequest, NodeSpec, ProjectionRequest,
    RemainingBudget, SearchMode, SearchRequest,
};
use graph_storage_sdk::plugin_api::{
    EmbeddingPlan, ExpandRequest, GraphEngineV1, GraphStoreError, GraphStoreV1, StoreCtx, VectorArm,
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
    /// Where `netem` is applied when a round-trip time is added.
    container_id: String,
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
        let scale = setting("GEARS_GRAPH_PERF_SCALE", 1.0, 0.001..=10.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "`setting` admits only 0.001..=10, so both products lie in \
                      100..=1_000_000 and are positive -- the cast is lossless over \
                      exactly the range it admits"
        )]
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
    let store = Arc::new(PgGraphStore::new(
        Arc::new(db),
        GraphStorageConfig::default()
            .validated()
            .expect("the perf configuration is valid"),
    ));
    let engine = PgGraphEngine::new(Arc::clone(&store));
    let container_id = container.id().to_owned();
    Some(Stand {
        store,
        engine,
        container_id,
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

/// One timing as one line: a run is read by `scripts/perf_summary.py`, and a
/// table someone retypes from a log is a table with typos in it.
/// `backend` is `-` for a scenario that runs no hop.
fn report(repeat: usize, rtt_ms: f64, backend: &str, scenario: &str, measured: &Measured) {
    println!(
        "RESULT rep={repeat} rtt_ms={rtt_ms} backend={backend} scenario=\"{scenario}\" \
         p50_ms={:.2} p95_ms={:.2} max_ms={:.2} runs={}",
        measured.p50.as_secs_f64() * 1000.0,
        measured.p95.as_secs_f64() * 1000.0,
        measured.worst.as_secs_f64() * 1000.0,
        measured.runs
    );
}

/// A numeric setting from the environment: absent is `default`, anything else
/// must parse and lie in `range`, or the run stops before it seeds.
///
/// Refused rather than replaced by the default, because a typo that fell back
/// silently would run -- and report -- a matrix nobody asked for, after a
/// seed that takes a quarter of an hour.
fn setting<T>(name: &str, default: T, range: std::ops::RangeInclusive<T>) -> T
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    let Ok(raw) = std::env::var(name) else {
        return default;
    };
    raw.trim()
        .parse::<T>()
        .ok()
        .filter(|value| range.contains(value))
        .unwrap_or_else(|| {
            panic!(
                "{name}: `{raw}` is not a number in {}..={}",
                range.start(),
                range.end()
            )
        })
}

fn repeats() -> usize {
    setting("GEARS_GRAPH_PERF_REPEAT", 1, 1..=20)
}

/// `GEARS_GRAPH_PERF_RTT_MS`: the round-trip times to add, in milliseconds.
///
/// A list that names none (empty, or commas only) is refused: it would seed
/// the graph, time nothing and pass without a single `RESULT` line.
fn rtts() -> Vec<f64> {
    const NAME: &str = "GEARS_GRAPH_PERF_RTT_MS";
    let Ok(raw) = std::env::var(NAME) else {
        return vec![0.0];
    };
    let rtts: Vec<f64> = raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            item.parse::<f64>()
                .ok()
                .filter(|ms| (0.0..=100.0).contains(ms))
                .unwrap_or_else(|| panic!("{NAME}: `{item}` is not 0..=100 ms"))
        })
        .collect();
    assert!(
        !rtts.is_empty(),
        "{NAME}: `{raw}` names no round-trip time; unset it, or list at least one (e.g. `0`)"
    );
    rtts
}

/// Delay every packet the database container sends by `ms`, which adds `ms`
/// to each round trip the gear makes; `0` removes the delay.
///
/// The `tc` runs in a helper container joined to the database container's
/// network namespace, so the host needs Docker and nothing else: no root, no
/// `iproute2` in the database image. A delay that does not apply fails the
/// run -- a column labelled "+2 ms" measured at +0 is worse than no column.
fn set_rtt(container_id: &str, ms: f64) {
    if ms > 0.0 {
        let delay = format!("{ms}ms");
        let out = tc(
            container_id,
            &["replace", "dev", "eth0", "root", "netem", "delay", &delay],
        );
        assert!(
            out.status.success(),
            "netem did not apply: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    // Deleting a qdisc that is not there fails, and that is the state wanted,
    // so a failed delete alone proves nothing either way. What decides is
    // whether a delay is still installed: a "+0 ms" column measured over a
    // leftover delay is the baseline every other column is read against.
    if tc(container_id, &["del", "dev", "eth0", "root"])
        .status
        .success()
    {
        return;
    }
    let shown = tc(container_id, &["show", "dev", "eth0"]);
    let qdiscs = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.status.success() && !qdiscs.contains("netem"),
        "netem could not be removed: {qdiscs}{}",
        String::from_utf8_lossy(&shown.stderr)
    );
}

/// `tc qdisc <args>` in a helper container joined to the database
/// container's network namespace.
fn tc(container_id: &str, args: &[&str]) -> std::process::Output {
    let netns = format!("container:{container_id}");
    std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--net",
            &netns,
            "--cap-add",
            "NET_ADMIN",
            "nicolaka/netshoot",
            "tc",
            "qdisc",
        ])
        .args(args)
        .output()
        .expect("docker runs")
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

/// A context with a budget of its own.
///
/// The conformance context's budget starts when the context is made (30 s),
/// and every store call checks it, so one context cannot outlive a seeding of
/// minutes: the lane used to fail with `edges commit: Deadline` (gears-rust
/// #5330). Every ingest batch and every timed round gets its own.
fn fresh<'a>(ctx: &StoreCtx<'a>) -> StoreCtx<'a> {
    StoreCtx {
        tenant: ctx.tenant,
        scope: ctx.scope,
        subject: ctx.subject.clone(),
        snapshot: None,
        budget: RemainingBudget::starting_now(Duration::from_mins(10)),
        cancel: ctx.cancel.clone(),
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
        let ctx = &fresh(ctx);
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
        let ctx = &fresh(ctx);
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
    // Every setting is read before the seed, which is most of the run's
    // wall clock: a bad one costs seconds, not a quarter of an hour.
    let shape = Shape::from_env();
    let rtts = rtts();
    let repeats = repeats();
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

    let ctx = fresh(&ctx);

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
    // Filtered on a declared payload path and ordered, which is the shape
    // the criteria table describes -- an unfiltered first page would measure
    // the table's ceiling, not the query behind it.
    let table_request = conformance::projection(
        &[conformance::OWNED],
        "payload/severity eq 'critical'",
        &[("node_key", toolkit_odata::SortDir::Asc)],
    );
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
    let root = store
        .resolve_node_ids(&ctx, &["perf-1".to_owned()])
        .await
        .expect("the root resolves")
        .first()
        .expect("the root exists")
        .1;

    let shaped = rtts.iter().any(|ms| *ms > 0.0);
    for repeat in 0..repeats {
        for &rtt in &rtts {
            if shaped {
                set_rtt(&stand.container_id, rtt);
            }
            let ctx = fresh(&ctx);

            // --- control: one statement, one round trip -----------------
            // What the stand and the pool cost before the gear does any
            // work; it also calibrates the added round-trip time.
            let probe = measure(40, |_| async {
                store.revision(&ctx).await.expect("the revision reads");
            })
            .await;
            report(repeat, rtt, "-", "one round trip (revision read)", &probe);

            // --- scenario 1: hybrid narrowing ---------------------------
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
            report(repeat, rtt, "-", "hybrid search (arm limit 50)", &hybrid);

            // --- scenario 2: the criteria table -------------------------
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
            report(
                repeat,
                rtt,
                "-",
                "criteria table (filter + page 50)",
                &table,
            );

            // --- scenarios 3 and 4: the hop -----------------------------
            let (traversal, neighborhood, ranked) =
                time_hops(&stand.engine, store, &ctx, &seed_ids, &type_set, root).await;
            report(
                repeat,
                rtt,
                "two_query",
                "depth-3 typed traversal, 8 seeds",
                &traversal,
            );
            report(
                repeat,
                rtt,
                "two_query",
                "depth-3 neighborhood + hydrate",
                &neighborhood,
            );
            report(
                repeat,
                rtt,
                "two_query",
                "depth-3 neighborhood + degrees + hydrate",
                &ranked,
            );

            // The thresholds, asserted only at full scale and with no added
            // round trip: a latency measured on a tenth of the graph is not
            // evidence about the graph the criteria name, and one measured
            // behind an artificial delay is a question about the network,
            // not a criterion the gear failed.
            if rtt <= 0.0 && (shape.scale - 1.0).abs() < f64::EPSILON {
                assert!(
                    hybrid.p95 <= Duration::from_millis(500),
                    "nfr-search-latency: hybrid p95 {hybrid}"
                );
                assert!(
                    neighborhood.p95 <= Duration::from_secs(1),
                    "nfr-traversal-latency: neighborhood p95 {neighborhood}"
                );
                assert!(
                    ranked.p95 <= Duration::from_secs(1),
                    "nfr-traversal-latency: ranked neighborhood p95 {ranked}"
                );
                assert!(
                    traversal.p95 <= Duration::from_secs(1),
                    "typed traversal: p95 {traversal}"
                );
            }
        }
    }
    if shaped {
        set_rtt(&stand.container_id, 0.0);
    }
    println!();
}

/// Scenarios 3 and 4 against one engine: the depth-3 typed traversal, and
/// the depth-3 UI neighborhood with the hydration the UI needs, which is
/// what `nfr-traversal-latency` actually budgets -- a neighborhood nobody can
/// render is not the scenario. The neighborhood is timed twice, without and
/// with each reached node's degree: the degree is what ranks a hub's
/// neighbours under the node budget, and it is a read of its own per hop.
async fn time_hops(
    engine: &PgGraphEngine,
    store: &PgGraphStore,
    reader: &StoreCtx<'_>,
    seed_ids: &[graph_storage_sdk::models::NodeId],
    type_set: &graph_storage_sdk::models::TypeIdSet,
    root: graph_storage_sdk::models::NodeId,
) -> (Measured, Measured, Measured) {
    let traversal = measure(30, |_| {
        let frontier = seed_ids.to_vec();
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
                            with_degrees: false,
                            discriminators: None,
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

    let neighborhood = |with_degrees: bool| {
        measure(30, move |_| async move {
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
                            with_degrees,
                            discriminators: None,
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
    };
    let plain = neighborhood(false).await;
    let ranked = neighborhood(true).await;
    (traversal, plain, ranked)
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

/// Overlapping ingest batches, the shape of gears-rust #5042: writers that
/// upsert random subsets of one shared key pool, with edges between the keys
/// each names, all at once. Two shapes, the two the issue measured on the
/// Studio stand.
///
/// Every batch's outcome is counted -- committed, `Serialization` (a
/// deadlock or a lock timeout the server broke), another conflict, the
/// deadline -- and the latency of every attempt is timed, committed or not.
/// One `RESULT` line per shape for the latency and one `CONTENTION` line for
/// the counts. Asserts nothing: a regression here is a number against the
/// previous run (gears-rust #5517), and the conformance case
/// `overlapping_batches_all_land` is what holds the contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn overlapping_ingest_batches() {
    let Some(stand) = stand().await else {
        return;
    };
    let repeats = repeats();

    // (keys in the pool, nodes per batch, writers, batches per writer)
    for (pool, per_batch, writers, rounds) in [(200, 50, 8, 10), (60, 40, 16, 6)] {
        for repeat in 0..repeats {
            // A tenant of its own per repeat, so every repeat starts with no
            // key of the pool stored: the first batches then race to create
            // keys, the later ones to update them, in every repeat alike.
            let tenant = Uuid::now_v7();
            let scope = AccessScope::for_tenant(tenant);
            let ctx = conformance::ctx(tenant, &scope, None);
            graph_storage::infra::store::ingest::ensure_meta(stand.store.as_ref(), tenant, &scope)
                .await
                .expect("meta rows exist");
            let mut types = conformance::ontology_batch();
            types.retain(|registration| registration.type_id != conformance::OWNED);
            types.push(seeded_type());
            stand
                .store
                .register_types(&ctx, types)
                .await
                .expect("the ontology registers");
            let scenario =
                format!("overlapping ingest: {writers} writers x {per_batch} of {pool} keys");
            let started = Instant::now();
            let tasks: Vec<_> = (0..writers)
                .map(|writer| {
                    let store = Arc::clone(&stand.store);
                    tokio::spawn(async move {
                        let scope = AccessScope::for_tenant(tenant);
                        let base = conformance::ctx(tenant, &scope, None);
                        let coordinator = conformance::coordinator();
                        let mut attempts = Vec::with_capacity(rounds);
                        for round in 0..rounds {
                            let seed = (repeat * 1_000 + writer) * 1_000 + round;
                            let mut keys: Vec<usize> = (0..pool).collect();
                            for i in (1..keys.len()).rev() {
                                keys.swap(i, mix(seed * 131 + i) % (i + 1));
                            }
                            keys.truncate(per_batch);
                            let edges = keys
                                .windows(2)
                                .map(|pair| EdgeSpec {
                                    type_id: conformance::LINK.to_owned(),
                                    src_node_key: format!("perf-{}", pair[0]),
                                    dst_node_key: format!("perf-{}", pair[1]),
                                    ..EdgeSpec::default()
                                })
                                .collect();
                            let request = IngestRequest {
                                nodes: keys.iter().map(|&k| seeded_node(k)).collect(),
                                edges,
                                options: IngestOptions {
                                    embed: Some(false),
                                    ..IngestOptions::default()
                                },
                                replace_scope: None,
                                idempotency_key: None,
                            };
                            // The interactive deadline a REST caller gets.
                            let ctx = StoreCtx {
                                budget: RemainingBudget::starting_now(Duration::from_secs(10)),
                                ..fresh(&base)
                            };
                            let plan = plan_for(store.as_ref(), &ctx, &request, &coordinator).await;
                            let attempt = Instant::now();
                            let outcome = store.ingest(&ctx, request, plan).await;
                            attempts.push((attempt.elapsed(), outcome.err()));
                        }
                        attempts
                    })
                })
                .collect();

            let (mut committed, mut serialization, mut conflict, mut deadline, mut other) =
                (0usize, 0usize, 0usize, 0usize, 0usize);
            let mut samples = Vec::new();
            let mut reasons = std::collections::BTreeMap::new();
            for task in tasks {
                for (elapsed, error) in task.await.expect("the writer does not panic") {
                    samples.push(elapsed);
                    match error {
                        None => committed += 1,
                        Some(GraphStoreError::Serialization) => serialization += 1,
                        Some(GraphStoreError::Conflict { reason }) => {
                            conflict += 1;
                            *reasons.entry(reason).or_insert(0usize) += 1;
                        }
                        Some(GraphStoreError::Deadline) => deadline += 1,
                        Some(error) => {
                            other += 1;
                            eprintln!("{scenario}: {error}");
                        }
                    }
                }
            }
            let wall = started.elapsed();
            let runs = samples.len();
            let measured = Measured {
                p50: percentile(&mut samples, 0.50),
                p95: percentile(&mut samples, 0.95),
                worst: *samples.last().expect("at least one batch"),
                runs,
            };
            report(repeat, 0.0, "-", &scenario, &measured);
            for (reason, times) in &reasons {
                println!(
                    "CONFLICT rep={repeat} scenario=\"{scenario}\" times={times} reason=\"{reason}\""
                );
            }
            println!(
                "CONTENTION rep={repeat} scenario=\"{scenario}\" committed={committed} \
                 serialization={serialization} conflict={conflict} deadline={deadline} \
                 other={other} wall_s={:.1} committed_nodes_per_s={:.0}",
                wall.as_secs_f64(),
                u32::try_from(committed * per_batch).map_or(f64::NAN, f64::from)
                    / wall.as_secs_f64()
            );
        }
    }
}
