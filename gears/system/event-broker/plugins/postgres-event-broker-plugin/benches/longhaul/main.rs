//! The long-haul run against a `PostgreSQL` server the operator owns
//! (`make longhaul-eb-pg`, `longhaul/README.md`): three topics written at a
//! steady rate with bursts, read three ways, checked while it runs, until
//! Ctrl-C or `--duration`.

#![allow(
    clippy::cast_precision_loss,
    reason = "rates and counts are reported, never fed back into exact arithmetic"
)]

mod args;
mod checks;
#[path = "../../tests/common/invariants.rs"]
mod invariants;
mod probe;
mod reader;
mod report;
mod tally;
mod topics;
mod writer;

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::Parser;
use event_broker_backend_bench::Histogram;
use event_broker_sdk::backend::{Backend, BackendError, BuildContext, Provider, Role};
use event_broker_sdk::{Position, Sequence};
use postgres_event_broker_plugin::PostgresBackendProvider;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::args::{Args, SegmentSize};
use crate::invariants::{Invariants, Mode, Violation};
use crate::probe::Sampler;
use crate::report::{Bounds, Report, Settled};
use crate::tally::Tally;
use crate::topics::{Kind, Topic};

/// How long tail readers get to read up to the final heads.
const DRAIN: Duration = Duration::from_mins(2);

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("longhaul: {err}");
            ExitCode::from(2)
        }
    }
}

/// Whether every criterion passed; `Err` when the run could not start.
async fn run(args: Args) -> Result<bool, String> {
    let url = std::env::var("EB_LONGHAUL_URL").map_err(|_| {
        "EB_LONGHAUL_URL is not set: the server to run against (longhaul/README.md)".to_owned()
    })?;
    let settings = args.backend_settings(&url)?;
    eprintln!(
        "settings: {}",
        serde_json::to_string(&args::redacted(&settings)).unwrap_or_default()
    );

    let pool = PgPool::connect(&url)
        .await
        .map_err(|err| format!("connect to EB_LONGHAUL_URL: {err}"))?;
    prepare_schema(&pool, &args).await?;

    let serde_json::Value::Object(map) = &settings else {
        return Err("settings are not an object".to_owned());
    };
    let backend = PostgresBackendProvider::default()
        .build(&BuildContext::new(map, Role::Standalone))
        .await
        .map_err(|err| format!("build: {err}"))?;
    backend
        .start()
        .await
        .map_err(|err| format!("start: {err}"))?;
    let topics = provision(&*backend, &args).await?;

    let args = Arc::new(args);
    let stop = CancellationToken::new();
    let tally = Arc::new(Tally::new(args.stop_on_violation.then(|| stop.clone())));
    let warmup = warmup(&args, &settings);
    tally.log(&format!(
        "warmup until {}",
        humantime::format_duration(round(warmup))
    ));

    let writers_stop = CancellationToken::new();
    let mut writers = spawn_writers(&args, &topics, &backend, &tally, &writers_stop);
    let readers_stop = CancellationToken::new();
    let mut readers = spawn_readers(&args, &topics, &backend, &tally, &url, &readers_stop);
    let db_probe = Sampler::new(
        pool.clone(),
        args.schema.clone(),
        topics.clone(),
        args.max_bytes,
    );
    let kept_samples = Arc::clone(&db_probe.samples);
    let status = tokio::spawn(status_loop(
        Arc::clone(&args),
        Arc::clone(&tally),
        args.status_every,
        readers_stop.clone(),
        db_probe,
    ));

    wait_for_end(&args, &tally, &stop).await;
    // The wrap-up below can take minutes; a second Ctrl-C cuts it short and
    // reports what is known.
    let hurry = CancellationToken::new();
    tokio::spawn({
        let (hurry, tally) = (hurry.clone(), Arc::clone(&tally));
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                tally.log("second Ctrl-C: reporting now");
                hurry.cancel();
            }
        }
    });
    tally.log("finishing: Ctrl-C again to skip the drain and settle, and report at once");

    writers_stop.cancel();
    while writers.join_next().await.is_some() {}
    let heads = final_heads(&*backend, &topics, &hurry).await;
    tally.log(&format!(
        "writers stopped; tail readers draining (up to {})",
        humantime::format_duration(DRAIN)
    ));
    let drained = drain(&tally, &heads, &hurry).await;
    readers_stop.cancel();
    while readers.join_next().await.is_some() {}
    if status.await.is_err() {
        tally.log("the status task panicked");
    }

    end_checks(&tally, &topics, &heads, drained);
    let rescan = args::rescan_interval(&settings);
    let settled = settle(&url, &args.schema, rescan, &tally, &hurry).await;
    backend.stop(CancellationToken::new()).await;

    let taken = kept_samples
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let report = report_of(&args, &settings, &tally, warmup, &settled, taken);
    Ok(publish(&report, &args.out))
}

/// The fenced writers of every topic.
fn spawn_writers(
    args: &Arc<Args>,
    topics: &[Topic],
    backend: &Arc<dyn Backend>,
    tally: &Arc<Tally>,
    stop: &CancellationToken,
) -> JoinSet<()> {
    let mut writers = JoinSet::new();
    let keys: Arc<[Vec<u32>]> = topics::keys_by_partition(args.keys, args.partitions).into();
    let per_writer = f64::from(args.rate) / 3.0 / f64::from(args.writers.max(1));
    for topic in topics {
        for i in 0..args.writers.max(1) {
            let writer = writer::Writer {
                id: u32::try_from(topic.kind.index()).unwrap_or(0) * 1000 + i,
                topic: topic.clone(),
                rate: per_writer,
                keys: Arc::clone(&keys),
                args: Arc::clone(args),
                backend: Arc::clone(backend),
                tally: Arc::clone(tally),
            };
            writers.spawn(writer.run(stop.clone()));
        }
    }
    writers
}

/// The tail, lagging and history readers of every topic, and the live check.
fn spawn_readers(
    args: &Args,
    topics: &[Topic],
    backend: &Arc<dyn Backend>,
    tally: &Arc<Tally>,
    url: &str,
    stop: &CancellationToken,
) -> JoinSet<()> {
    let mut tasks = JoinSet::new();
    let topic_rate = f64::from(args.rate) / 3.0;
    for topic in topics {
        tasks.spawn(reader::tail(
            Arc::clone(backend),
            Arc::clone(tally),
            topic.clone(),
            stop.clone(),
        ));
        tasks.spawn(reader::lagging(
            Arc::clone(backend),
            Arc::clone(tally),
            topic.clone(),
            topic_rate * 0.9,
            stop.clone(),
        ));
        tasks.spawn(reader::history(
            Arc::clone(backend),
            Arc::clone(tally),
            topic.clone(),
            args.history_batch,
            stop.clone(),
        ));
    }
    tasks.spawn(probe::check_live(
        url.to_owned(),
        args.schema.clone(),
        args.check_every,
        Arc::clone(tally),
        stop.clone(),
    ));
    tasks
}

/// Until Ctrl-C, `--duration`, or the first violation under
/// `--stop-on-violation`.
async fn wait_for_end(args: &Args, tally: &Tally, stop: &CancellationToken) {
    let deadline = async {
        match args.duration {
            Some(duration) => tokio::time::sleep(duration).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tally.log("interrupted"),
        () = deadline => tally.log("duration reached"),
        () = stop.cancelled() => tally.log("stopping at the first violation"),
    }
}

fn report_of(
    args: &Args,
    settings: &serde_json::Value,
    tally: &Tally,
    warmup: Duration,
    settled: &Settled,
    samples: Vec<probe::Sample>,
) -> Report {
    let criteria = report::criteria(
        &samples,
        &Bounds {
            warmup,
            window: args.burst_every,
            max_age: args.max_age,
            fill: segment_fill(args, settings),
            grace: args.retention_grace,
        },
        tally.violations.load(Ordering::Relaxed),
        settled,
    );
    let load = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    Report {
        args: serde_json::to_value(args).unwrap_or_default(),
        settings: args::redacted(settings),
        elapsed_s: tally.started.elapsed().as_secs_f64(),
        warmup_s: warmup.as_secs_f64(),
        acked: Kind::ALL
            .iter()
            .map(|k| (*k, load(&tally.acked[k.index()])))
            .collect(),
        unresolved: Kind::ALL
            .iter()
            .map(|k| (*k, load(&tally.unresolved[k.index()])))
            .collect(),
        read: load(&tally.read),
        skipped: load(&tally.skipped),
        tail_skipped: load(&tally.tail_skipped),
        append_errors: load(&tally.append_errors),
        read_errors: load(&tally.read_errors),
        watch_errors: load(&tally.watch_errors),
        check_errors: load(&tally.check_errors),
        transient: load(&tally.transient),
        violations: load(&tally.violations),
        violation_log: tally.kept_violations(),
        outages: tally.outages(),
        settle_s: match settled {
            Settled::Clean(took) => Some(took.as_secs_f64()),
            Settled::Broken(_) | Settled::NotChecked(_) => None,
        },
        criteria,
        samples,
    }
}

/// Prints the markdown report, writes the JSON one; whether every criterion
/// passed.
fn publish(report: &Report, out: &std::path::Path) -> bool {
    println!("{}", report.markdown());
    if let Some(dir) = out.parent()
        && let Err(err) = std::fs::create_dir_all(dir)
    {
        eprintln!("report directory {}: {err}", dir.display());
    }
    match serde_json::to_vec_pretty(report) {
        Ok(json) => {
            if let Err(err) = std::fs::write(out, json) {
                eprintln!("report not written to {}: {err}", out.display());
            }
        }
        Err(err) => eprintln!("report not serialized: {err}"),
    }
    report.passed()
}

/// Refuses an existing schema unless `--reset`, which drops it.
async fn prepare_schema(pool: &PgPool, args: &Args) -> Result<(), String> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
            .bind(&args.schema)
            .fetch_one(pool)
            .await
            .map_err(|err| format!("schema lookup: {err}"))?;
    if exists && !args.reset {
        return Err(format!(
            "schema {} exists; pass --reset to drop it, or --schema to use another",
            args.schema
        ));
    }
    if exists {
        drop_schema(pool, &args.schema).await?;
        eprintln!("dropped schema {}", args.schema);
    }
    Ok(())
}

/// Segment tables dropped per transaction while resetting.
const DROP_BATCH: i64 = 500;

/// Drops `schema`: its segment tables first, [`DROP_BATCH`] per transaction,
/// then the rest. One `DROP SCHEMA ... CASCADE` over thousands of segments
/// takes a lock on each in one transaction and runs out of the server's lock
/// table.
async fn drop_schema(pool: &PgPool, schema: &str) -> Result<(), String> {
    // `schema` is checked by the plugin as a bare identifier, but the plugin
    // has not seen it yet: quote it here.
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    loop {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT format('%I', c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname = $1 AND c.relkind = 'r' \
                AND c.relname ~ '^evbk__[0-9a-f]{32}_p[0-9]+_s[0-9a-f]{16}$' LIMIT $2",
        )
        .bind(schema)
        .bind(DROP_BATCH)
        .fetch_all(pool)
        .await
        .map_err(|err| format!("list segment tables of {schema}: {err}"))?;
        if tables.is_empty() {
            break;
        }
        let list: Vec<String> = tables.iter().map(|t| format!("{quoted}.{t}")).collect();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TABLE {}",
            list.join(", ")
        )))
        .execute(pool)
        .await
        .map_err(|err| format!("drop segment tables of {schema}: {err}"))?;
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {quoted} CASCADE")))
        .execute(pool)
        .await
        .map_err(|err| format!("drop schema {schema}: {err}"))?;
    Ok(())
}

/// Provisions the topics once the backend answers.
async fn provision(backend: &dyn Backend, args: &Args) -> Result<Vec<Topic>, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    loop {
        match topics::provision(backend, args.partitions, args.max_age, args.max_bytes).await {
            Ok(topics) => return Ok(topics),
            Err(err) if tokio::time::Instant::now() < deadline => {
                eprintln!("waiting for the backend: {err}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

/// `max_age` plus the time to fill three segments of a partition at the
/// base rate.
fn warmup(args: &Args, settings: &serde_json::Value) -> Duration {
    args.max_age + segment_fill(args, settings) * 3
}

/// How long one segment of a partition takes to fill at the base rate: a
/// segment seals, and so starts to age, only once full.
fn segment_fill(args: &Args, settings: &serde_json::Value) -> Duration {
    let per_partition = f64::from(args.rate) / 3.0 / f64::from(args.partitions.max(1));
    match args::segment_size(settings) {
        SegmentSize::Events(events) => {
            Duration::from_secs_f64(events as f64 / per_partition.max(f64::EPSILON))
        }
        SegmentSize::Period(period) => period,
    }
}

fn round(d: Duration) -> Duration {
    Duration::from_secs(d.as_secs())
}

/// Prints the status line every `every`.
async fn status_loop(
    args: Arc<Args>,
    tally: Arc<Tally>,
    every: Duration,
    stop: CancellationToken,
    mut db: Sampler,
) {
    let mut ticks = tokio::time::interval(every);
    let mut last_written = 0;
    let mut last_read = 0;
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let s = match db.sample(&tally).await {
            Ok(s) => s,
            Err(err) => {
                tally.log(&format!("status sample failed: {err}"));
                continue;
            }
        };
        let written = tally.total_acked();
        let read = tally.read.load(Ordering::Relaxed);
        let secs = every.as_secs_f64();
        let latency = tally.take_latency();
        let state = |name: &str| s.states.get(name).copied().unwrap_or(0);
        tally.log(&format!(
            "write {} of {} behind={} append={} | read {} delivery={} read={} | segments open={} sealed={} expired={} detaching={} dropped={} tables={} db={:.0}MiB | err={} trans={} viol={}",
            per_sec((written - last_written) as f64 / secs),
            per_sec(
                f64::from(args.rate)
                    * args.burst_factor_at(tally.started.elapsed())
            ),
            p99(&latency.behind),
            p99(&latency.append),
            per_sec((read - last_read) as f64 / secs),
            p99(&latency.delivery),
            p99(&latency.read),
            state("open"),
            state("sealed"),
            state("expired"),
            state("detaching"),
            s.dropped,
            s.tables,
            s.schema_bytes as f64 / 1_048_576.0,
            tally.append_errors.load(Ordering::Relaxed)
                + tally.read_errors.load(Ordering::Relaxed)
                + tally.watch_errors.load(Ordering::Relaxed)
                + tally.check_errors.load(Ordering::Relaxed),
            tally.transient.load(Ordering::Relaxed),
            tally.violations.load(Ordering::Relaxed),
        ));
        last_written = written;
        last_read = read;
    }
}

/// The 99th percentile as `4ms` or `115.3s`; `-` with no sample.
fn p99(histogram: &Histogram) -> String {
    if histogram.is_empty() {
        return "-".to_owned();
    }
    let ms = histogram.percentiles().p99_ns as f64 / 1e6;
    if ms < 1000.0 {
        format!("{ms:.0}ms")
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

/// `2.0k/s`, `950/s`.
fn per_sec(rate: f64) -> String {
    if rate >= 1000.0 {
        format!("{:.1}k/s", rate / 1000.0)
    } else {
        format!("{rate:.0}/s")
    }
}

/// Each partition's head once writers stopped.
async fn final_heads(
    backend: &dyn Backend,
    topics: &[Topic],
    hurry: &CancellationToken,
) -> Vec<(Kind, u32, i64)> {
    let mut heads = Vec::new();
    for topic in topics {
        for p in 0..topic.partitions {
            let head = loop {
                match backend.resolve(&topic.at(p), Position::Latest).await {
                    Ok(head) => break head.as_i64(),
                    Err(BackendError::Unavailable { .. }) if !hurry.is_cancelled() => {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Err(_) => break Sequence::NONE.as_i64(),
                }
            };
            heads.push((topic.kind, p, head));
        }
    }
    heads
}

/// Waits until every tail reader reached its partition's final head; false
/// if [`DRAIN`] passed or `hurry` came first.
async fn drain(tally: &Tally, heads: &[(Kind, u32, i64)], hurry: &CancellationToken) -> bool {
    let deadline = tokio::time::Instant::now() + DRAIN;
    while tokio::time::Instant::now() < deadline && !hurry.is_cancelled() {
        let at = tally.tail_positions();
        if heads
            .iter()
            .all(|(k, p, head)| *head == 0 || at.get(&(*k, *p)).copied().unwrap_or(0) >= *head)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tally.log(
        "tail readers did not reach the final heads; the keep topic's completeness is not judged",
    );
    false
}

/// Exactly once per topic, and the `keep` topic read whole by its tail.
fn end_checks(tally: &Tally, topics: &[Topic], heads: &[(Kind, u32, i64)], drained: bool) {
    for topic in topics {
        let stored: i64 = heads
            .iter()
            .filter(|(k, _, _)| *k == topic.kind)
            .map(|(_, _, h)| h)
            .sum();
        let stored = u64::try_from(stored).unwrap_or(0);
        let acked = tally.acked[topic.kind.index()].load(Ordering::Relaxed);
        let unresolved = tally.unresolved[topic.kind.index()].load(Ordering::Relaxed);
        if stored < acked || stored > acked + unresolved {
            tally.violation(format!(
                "{}: heads sum to {stored}, acknowledged {acked}, unanswered at stop {unresolved}",
                topic.kind.name()
            ));
        }
    }
    let at = tally.tail_positions();
    for (kind, p, head) in heads.iter().filter(|(k, _, _)| drained && *k == Kind::Keep) {
        let read = at.get(&(*kind, *p)).copied().unwrap_or(0);
        if read < *head {
            tally.violation(format!("keep/{p}: tail read up to {read} of {head}"));
        }
    }
    if tally.tail_skipped.load(Ordering::Relaxed) > 0 {
        tally.log(&format!(
            "tail readers fell below the floor and skipped {} events",
            tally.tail_skipped.load(Ordering::Relaxed)
        ));
    }
}

/// Waits for maintenance to settle, then checks the settled invariants until
/// none is broken or ten rescans passed (design D7), unless `hurry` comes
/// first.
async fn settle(
    url: &str,
    schema: &str,
    rescan: Duration,
    tally: &Tally,
    hurry: &CancellationToken,
) -> Settled {
    if hurry.is_cancelled() {
        return Settled::NotChecked("cut short by a second Ctrl-C");
    }
    tally.log(&format!(
        "settling: checking the settled invariants for up to {}",
        humantime::format_duration(rescan * 10)
    ));
    let started = tokio::time::Instant::now();
    let deadline = started + rescan * 10;
    let mut wait = rescan * 2;
    let mut session: Option<Invariants> = None;
    // Maintenance may still be working off a backlog: a detach finishing
    // during a check hides rows from it (design D2). A finding counts only
    // when the next check repeats it.
    let mut previous: Vec<Violation> = Vec::new();
    loop {
        tokio::select! {
            () = hurry.cancelled() => return Settled::NotChecked("cut short by a second Ctrl-C"),
            () = tokio::time::sleep(wait) => {}
        }
        wait = rescan;
        if session.is_none() {
            session = Invariants::connect(url).await.ok();
        }
        let found = match session.as_mut() {
            Some(invariants) => match invariants.check(schema, Mode::Settled).await {
                Ok(found) => found,
                Err(err) => {
                    tally.log(&format!("settled check failed: {err}"));
                    session = None;
                    continue;
                }
            },
            None => continue,
        };
        if found.is_empty() {
            return Settled::Clean(started.elapsed());
        }
        let confirmed: Vec<Violation> = found
            .iter()
            .filter(|v| previous.iter().any(|p| same_finding(p, v)))
            .cloned()
            .collect();
        if tokio::time::Instant::now() >= deadline {
            return if confirmed.is_empty() {
                Settled::NotChecked("still changing at the deadline: no finding repeated")
            } else {
                Settled::Broken(summarize(&confirmed))
            };
        }
        previous = found;
    }
}

fn same_finding(a: &Violation, b: &Violation) -> bool {
    probe::finding(a) == probe::finding(b)
}

/// One line per invariant and topic: how many segments or partitions, and
/// the first of them in full.
fn summarize(found: &[Violation]) -> Vec<String> {
    // (the first finding of a group, how many the group holds)
    let mut groups: Vec<(&Violation, usize)> = Vec::new();
    for v in found {
        match groups
            .iter_mut()
            .find(|(first, _)| first.invariant == v.invariant && first.topic_uuid == v.topic_uuid)
        {
            Some(group) => group.1 += 1,
            None => groups.push((v, 1)),
        }
    }
    groups
        .into_iter()
        .map(|(first, count)| format!("{count} x {}", probe::describe(first)))
        .collect()
}
