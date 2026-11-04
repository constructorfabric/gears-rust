//! The cross-check (`docs/DESIGN.md` §4.9): compares the segment rows with
//! the tables the catalog holds, so drift neither job would notice on its
//! own is repaired or reported.
//!
//! The catalog is not read in the rows' snapshot, so one comparison may see
//! a job's half-finished step; the rescan acts on a finding only once two
//! cross-checks in a row agree on it, and reconcile verifies it again under
//! the partition's lock before it repairs anything.

use std::collections::{BTreeMap, HashMap, HashSet};

use sqlx::PgConnection;
use uuid::Uuid;

use super::catalog::SegmentState;
use super::prioritizer::Key;
use crate::{names, segment};

/// One `evbk__segment` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The partition.
    pub key: Key,
    /// `[first, end)`.
    pub first: i64,
    /// One past its last sequence.
    pub end: i64,
    /// Where it is in its life.
    pub state: SegmentState,
    /// Its table's stored name.
    pub table: String,
}

/// One table of the plugin's schema that is, or looks like, a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    /// Its name.
    pub name: String,
    /// Whether it is a partition of its parent.
    pub attached: bool,
    /// Whether a `DETACH ... CONCURRENTLY` on it is unfinished.
    pub pending: bool,
    /// Its partition bound, while attached.
    pub bound: Option<(i64, i64)>,
}

/// What a partition's own row says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// The highest assigned sequence.
    pub hwm: i64,
    /// The highest removed sequence.
    pub floor: i64,
}

/// What kind of disagreement a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Case {
    /// C3: an expired row whose table is detached or gone.
    Undetached,
    /// C4: an open row above the head whose table is gone.
    Unbuilt,
    /// C5: an open or sealed row at or below the head whose table is gone:
    /// events are lost.
    Lost,
    /// C6: an open or sealed row whose table is detached: events are hidden.
    Hidden,
    /// C7: an attached table with no row.
    Unrecorded,
    /// C8: a detached table with no row, wholly at or below the floor.
    Leftover,
    /// C8b: a detached table with no row, not wholly below the floor.
    Stray,
    /// C9: an attached table whose bound is not its row's range.
    Misbound,
    /// C10: the open and sealed rows leave a gap above the floor.
    Gap,
}

/// What the rescan does with a finding two cross-checks agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Offer the partition to the detacher, which finishes it.
    Detach,
    /// Offer the partition to reconcile, which repairs it, or, when no
    /// repair exists, keeps failing on it until it is reported stuck.
    Reconcile,
    /// Report it, and nothing else.
    Report,
}

impl Case {
    /// What the rescan does with this case.
    pub fn reaction(self) -> Reaction {
        match self {
            Self::Undetached => Reaction::Detach,
            Self::Unbuilt | Self::Unrecorded | Self::Leftover | Self::Lost | Self::Gap => {
                Reaction::Reconcile
            }
            Self::Hidden | Self::Stray | Self::Misbound => Reaction::Report,
        }
    }
}

/// One disagreement, on one segment of one partition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Finding {
    /// The partition.
    pub key: Key,
    /// The kind of disagreement.
    pub case: Case,
    /// The table it is about.
    pub segment: String,
}

/// Every disagreement between `rows`, the catalog's `relations` and the
/// partitions' `stats`, in order. Rows and tables of a partition `stats`
/// does not hold are left alone.
pub fn compare(rows: &[Row], relations: &[Relation], stats: &HashMap<Key, Stats>) -> Vec<Finding> {
    let mut findings = Vec::new();
    rows_against_tables(rows, relations, stats, &mut findings);
    tables_without_rows(rows, relations, stats, &mut findings);
    gaps(rows, stats, &mut findings);
    findings.sort();
    findings
}

/// C3 to C6 and C9: each row against its table.
fn rows_against_tables(
    rows: &[Row],
    relations: &[Relation],
    stats: &HashMap<Key, Stats>,
    findings: &mut Vec<Finding>,
) {
    let by_name: HashMap<&str, &Relation> =
        relations.iter().map(|r| (r.name.as_str(), r)).collect();
    for row in rows {
        let Some(stats) = stats.get(&row.key) else {
            continue;
        };
        let live = matches!(row.state, SegmentState::Open | SegmentState::Sealed);
        let case = match (row.state, by_name.get(row.table.as_str())) {
            // The detacher's own steps.
            (SegmentState::Detaching, _) => None,
            (SegmentState::Expired, None) => Some(Case::Undetached),
            (SegmentState::Expired, Some(r)) if !attached(r) => Some(Case::Undetached),
            (SegmentState::Open, None) if row.first > stats.hwm => Some(Case::Unbuilt),
            (_, None) if live => Some(Case::Lost),
            (_, Some(r)) if live && !attached(r) => Some(Case::Hidden),
            (_, Some(r)) if attached(r) && r.bound != Some((row.first, row.end)) => {
                Some(Case::Misbound)
            }
            _ => None,
        };
        if let Some(case) = case {
            findings.push(Finding {
                key: row.key,
                case,
                segment: row.table.clone(),
            });
        }
    }
}

/// C7 and C8: tables no row names, placed by their name.
fn tables_without_rows(
    rows: &[Row],
    relations: &[Relation],
    stats: &HashMap<Key, Stats>,
    findings: &mut Vec<Finding>,
) {
    let recorded: HashSet<&str> = rows.iter().map(|r| r.table.as_str()).collect();
    for relation in relations {
        if recorded.contains(relation.name.as_str()) {
            continue;
        }
        let Some((topic, partition, first)) = names::parse_segment(&relation.name) else {
            continue;
        };
        let key = (topic, partition);
        let Some(stats) = stats.get(&key) else {
            continue;
        };
        // A detached table has no bound left: the next recorded segment
        // ends its range.
        let end = rows
            .iter()
            .filter(|r| r.key == key && r.first > first)
            .map(|r| r.first)
            .min();
        let case = match end {
            _ if attached(relation) => Case::Unrecorded,
            Some(end) if end - 1 <= stats.floor => Case::Leftover,
            _ => Case::Stray,
        };
        findings.push(Finding {
            key,
            case,
            segment: relation.name.clone(),
        });
    }
}

/// C10: the open and sealed rows of each partition, from its floor up.
fn gaps(rows: &[Row], stats: &HashMap<Key, Stats>, findings: &mut Vec<Finding>) {
    let mut live: BTreeMap<Key, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        if matches!(row.state, SegmentState::Open | SegmentState::Sealed) {
            live.entry(row.key).or_default().push(row);
        }
    }
    for (key, mut rows) in live {
        let Some(stats) = stats.get(&key) else {
            continue;
        };
        rows.sort_by_key(|r| r.first);
        let mut expected = stats.floor + 1;
        for row in rows {
            if row.first > expected {
                findings.push(Finding {
                    key,
                    case: Case::Gap,
                    segment: row.table.clone(),
                });
            }
            expected = row.end;
        }
    }
}

/// Attached and not on its way out.
fn attached(relation: &Relation) -> bool {
    relation.attached && !relation.pending
}

/// What a query covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every partition.
    All,
    /// One partition.
    Partition(Key),
}

/// One `evbk__segment` row, as read.
#[derive(sqlx::FromRow)]
struct SegmentRow {
    topic_uuid: Uuid,
    partition: i32,
    first: i64,
    end: i64,
    state: SegmentState,
    table: String,
}

/// The `evbk__segment` rows in `scope`.
pub async fn rows(conn: &mut PgConnection, scope: Scope) -> Result<Vec<Row>, sqlx::Error> {
    const SELECT: &str = "SELECT topic_uuid, partition, first_seq AS first, end_seq AS \"end\", state, \
         table_name AS \"table\" FROM evbk__segment";
    let found: Vec<SegmentRow> = match scope {
        Scope::All => sqlx::query_as(SELECT).fetch_all(conn).await?,
        Scope::Partition((topic, partition)) => {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{SELECT} WHERE topic_uuid = $1 AND partition = $2"
            )))
            .bind(topic)
            .bind(names::partition_column(partition))
            .fetch_all(conn)
            .await?
        }
    };
    Ok(found
        .into_iter()
        .filter_map(|row| {
            Some(Row {
                key: (row.topic_uuid, names::partition_from_column(row.partition)?),
                first: row.first,
                end: row.end,
                state: row.state,
                table: row.table,
            })
        })
        .collect())
}

/// One table, as the catalog has it.
#[derive(sqlx::FromRow)]
struct RelationRow {
    name: String,
    attached: bool,
    pending: bool,
    /// `FOR VALUES ...`, while attached.
    bound: Option<String>,
}

/// The tables in `scope` that a row names, or that the naming rules would
/// name a segment.
pub async fn relations(
    conn: &mut PgConnection,
    scope: Scope,
) -> Result<Vec<Relation>, sqlx::Error> {
    let (pattern, (topic, partition)) = match scope {
        Scope::All => (
            "^evbk__[0-9a-f]{32}_p[0-9]+_s[0-9a-f]{16}$".to_owned(),
            (Uuid::nil(), event_broker_sdk::backend::Partition::new(0)),
        ),
        Scope::Partition((topic, partition)) => (
            format!("^{}_s[0-9a-f]{{16}}$", names::parent(topic, partition)),
            (topic, partition),
        ),
    };
    let found: Vec<RelationRow> = sqlx::query_as(
        "SELECT c.relname::text AS name, i.inhrelid IS NOT NULL AS attached, \
         COALESCE(i.inhdetachpending, false) AS pending, pg_get_expr(c.relpartbound, c.oid) AS bound \
         FROM pg_class c LEFT JOIN pg_inherits i ON i.inhrelid = c.oid \
         WHERE c.relnamespace = current_schema()::regnamespace AND c.relkind = 'r' \
         AND (c.relname ~ $1 OR c.relname IN (SELECT table_name FROM evbk__segment \
              WHERE $2 OR (topic_uuid = $3 AND partition = $4)))",
    )
    .bind(pattern)
    .bind(scope == Scope::All)
    .bind(topic)
    .bind(names::partition_column(partition))
    .fetch_all(conn)
    .await?;
    Ok(found
        .into_iter()
        .map(|row| Relation {
            name: row.name,
            attached: row.attached,
            pending: row.pending,
            bound: row.bound.as_deref().and_then(segment::parse_bound),
        })
        .collect())
}
