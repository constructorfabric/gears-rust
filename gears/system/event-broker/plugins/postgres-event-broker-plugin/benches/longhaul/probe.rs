//! The run's own look at the database: the live invariants with two-strike
//! confirmation (design D2), and the figures behind the status line and the
//! pass criteria.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{AssertSqlSafe, PgPool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::invariants::{Invariants, Mode, Violation};
use crate::tally::Tally;
use crate::topics::{Kind, Topic};

/// Runs the live check every `every` until `stop`.
pub async fn check_live(
    url: String,
    schema: String,
    every: Duration,
    tally: Arc<Tally>,
    stop: CancellationToken,
) {
    let mut session: Option<Invariants> = None;
    let mut previous: HashMap<Finding, Violation> = HashMap::new();
    let mut confirmed: HashSet<Finding> = HashSet::new();
    let mut ticks = tokio::time::interval(every);
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            _ = ticks.tick() => {}
        }
        if session.is_none() {
            session = Invariants::connect(&url).await.ok();
        }
        let Some(invariants) = session.as_mut() else {
            tally.check_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let found: HashMap<Finding, Violation> = match invariants.check(&schema, Mode::Live).await {
            Ok(found) => found.into_iter().map(|v| (finding(&v), v)).collect(),
            Err(err) => {
                tally.check_errors.fetch_add(1, Ordering::Relaxed);
                tally.log(&format!("invariant check failed: {err}"));
                session = None;
                continue;
            }
        };
        // A finding seen once and gone at the next check is transient; seen
        // twice in a row it is a violation, reported once until it clears.
        // Its figures may move between the two, e.g. L2's count as appends go on.
        for (key, violation) in &found {
            if previous.contains_key(key) && confirmed.insert(key.clone()) {
                tally.violation(describe(violation));
            }
        }
        let transient = previous
            .keys()
            .filter(|k| !found.contains_key(*k) && !confirmed.contains(*k))
            .count();
        tally
            .transient
            .fetch_add(transient as u64, Ordering::Relaxed);
        confirmed.retain(|k| found.contains_key(k));
        previous = found;
    }
}

/// What makes two findings the same: the invariant on the same segment or
/// partition, whatever the figures in `detail`.
pub type Finding = (String, Option<Uuid>, Option<i32>, Option<i64>);

pub fn finding(v: &Violation) -> Finding {
    (v.invariant.clone(), v.topic_uuid, v.partition, v.first_seq)
}

pub fn describe(v: &Violation) -> String {
    format!(
        "{} topic={} partition={} first_seq={} {}",
        v.invariant,
        v.topic_uuid
            .map_or_else(|| "-".to_owned(), |t| t.simple().to_string()),
        v.partition
            .map_or_else(|| "-".to_owned(), |p| p.to_string()),
        v.first_seq
            .map_or_else(|| "-".to_owned(), |s| s.to_string()),
        v.detail
    )
}

/// One partition, as a sample reads it.
#[derive(sqlx::FromRow)]
struct PartitionFigures {
    topic_uuid: Uuid,
    hwm: i64,
    floor: i64,
    /// The partitioned table.
    table_name: String,
    /// Its size on disk as retention measures it: each sealed segment as
    /// measured when sealed, each open one as it is now.
    stored: i64,
    /// The two largest of those sizes, summed.
    largest: i64,
}

/// One status sample.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Sample {
    /// Seconds from the start of the run.
    pub at_s: f64,
    /// Segment rows per state, over every topic.
    pub states: HashMap<String, i64>,
    /// Segment rows per topic.
    pub rows: HashMap<Kind, i64>,
    /// Segment tables of the schema.
    pub tables: i64,
    /// `pg_total_relation_size` of the schema.
    pub schema_bytes: i64,
    /// Segments gone since the previous sample.
    pub dropped: u64,
    /// Age of the oldest stored event of the `age` topic, seconds.
    pub oldest_age_s: f64,
    /// Bytes partitions of the `bytes` topic keep above `max_bytes` plus their
    /// two largest segments; 0 when none does.
    pub bytes_excess: i64,
    /// Sessions whose `application_name` begins with `evbk-`.
    pub sessions: i64,
    /// The run's resident memory.
    pub rss_bytes: u64,
}

/// Samples the database for the status line and the criteria.
pub struct Sampler {
    pool: PgPool,
    schema: String,
    topics: Vec<Topic>,
    max_bytes: i64,
    seen: HashSet<(Uuid, i32, i64)>,
    pub samples: Arc<Mutex<Vec<Sample>>>,
}

impl Sampler {
    pub fn new(pool: PgPool, schema: String, topics: Vec<Topic>, max_bytes: u64) -> Self {
        Self {
            pool,
            schema,
            topics,
            max_bytes: i64::try_from(max_bytes).unwrap_or(i64::MAX),
            seen: HashSet::new(),
            samples: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn kind(&self, uuid: Uuid) -> Option<Kind> {
        self.topics.iter().find(|t| t.uuid == uuid).map(|t| t.kind)
    }

    /// Takes one sample and keeps it.
    pub async fn sample(&mut self, tally: &Tally) -> Result<Sample, sqlx::Error> {
        let s = &self.schema;
        let mut sample = Sample {
            at_s: tally.started.elapsed().as_secs_f64(),
            rss_bytes: rss(),
            ..Sample::default()
        };

        let rows: Vec<(Uuid, i32, i64, String)> = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT topic_uuid, partition, first_seq, state::text FROM {s}.evbk__segment"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut now = HashSet::new();
        for (topic, partition, first, state) in rows {
            *sample.states.entry(state).or_insert(0) += 1;
            if let Some(kind) = self.kind(topic) {
                *sample.rows.entry(kind).or_insert(0) += 1;
                if kind != Kind::Keep {
                    now.insert((topic, partition, first));
                }
            }
        }
        sample.dropped = self.seen.difference(&now).count() as u64;
        self.seen = now;

        (sample.tables, sample.schema_bytes) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE c.relname ~ '^evbk__[0-9a-f]{32}_p[0-9]+_s[0-9a-f]{16}$'), \
                    COALESCE(sum(pg_total_relation_size(c.oid)) FILTER (WHERE c.relkind = 'r'), 0)::bigint \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
        )
        .bind(s)
        .fetch_one(&self.pool)
        .await?;

        let partitions: Vec<PartitionFigures> = sqlx::query_as(AssertSqlSafe(format!(
            "WITH sized AS (SELECT topic_uuid, partition, CASE WHEN state = 'open' \
                  THEN COALESCE(pg_total_relation_size(to_regclass(format('%I.%I', '{s}', table_name))), 0) \
                  ELSE bytes END AS size \
                  FROM {s}.evbk__segment WHERE state IN ('open', 'sealed')) \
             SELECT p.topic_uuid, p.hwm, r.floor, p.table_name::text AS table_name, \
                    COALESCE((SELECT sum(z.size) FROM sized z \
                      WHERE z.topic_uuid = p.topic_uuid AND z.partition = p.partition), 0)::bigint AS stored, \
                    COALESCE((SELECT sum(t.size) FROM (SELECT z.size FROM sized z \
                      WHERE z.topic_uuid = p.topic_uuid AND z.partition = p.partition \
                      ORDER BY z.size DESC LIMIT 2) t), 0)::bigint AS largest \
               FROM {s}.evbk__partition p JOIN {s}.evbk__retention r \
                 ON r.topic_uuid = p.topic_uuid AND r.partition = p.partition"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut oldest: Option<DateTime<Utc>> = None;
        for PartitionFigures {
            topic_uuid: topic,
            hwm,
            floor,
            table_name: table,
            stored,
            largest,
        } in partitions
        {
            let Some(kind) = self.kind(topic) else {
                continue;
            };
            if kind == Kind::Age && hwm > floor {
                let first: Option<DateTime<Utc>> = sqlx::query_scalar(AssertSqlSafe(format!(
                    "SELECT sequence_time FROM {s}.{table} WHERE sequence = $1"
                )))
                .bind(floor + 1)
                .fetch_optional(&self.pool)
                .await?;
                oldest = match (oldest, first) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
            if kind == Kind::Bytes {
                sample.bytes_excess = sample.bytes_excess.max(stored - self.max_bytes - largest);
            }
        }
        if let Some(oldest) = oldest {
            sample.oldest_age_s = (Utc::now() - oldest).as_seconds_f64();
        }

        sample.sessions = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
              WHERE datname = current_database() AND application_name LIKE 'evbk-%'",
        )
        .fetch_one(&self.pool)
        .await?;

        self.samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sample.clone());
        Ok(sample)
    }
}

/// This process's resident set, from `/proc` (Linux); 0 elsewhere.
fn rss() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|kb| kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}
