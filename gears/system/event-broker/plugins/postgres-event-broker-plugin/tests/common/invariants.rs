//! The storage invariants of `tests/sql/invariants.sql`, run from Rust. Shared
//! by the layer 3 tests and the long-haul run (`benches/longhaul`), which
//! includes this file by path.

use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// The SQL that creates `pg_temp.evbk_violations` in a session.
const INVARIANTS: &str = include_str!("../sql/invariants.sql");

/// Which invariants a check reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// L1-L8: hold at every instant, under load.
    Live,
    /// L1-L8 and S1-S6: hold once appends stopped and maintenance settled.
    Settled,
}

impl Mode {
    const fn as_sql(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Settled => "settled",
        }
    }
}

/// One violated invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, sqlx::FromRow)]
pub struct Violation {
    /// `L1`-`L8`, `S1`-`S6`.
    pub invariant: String,
    pub topic_uuid: Option<Uuid>,
    pub partition: Option<i32>,
    /// The segment's first sequence, when a segment is concerned.
    pub first_seq: Option<i64>,
    /// The figures compared, e.g. `count=998 expected=1000 above_hwm=0`.
    pub detail: String,
}

/// A session with the check loaded: `pg_temp` lives as long as the session.
pub struct Invariants {
    conn: PgConnection,
}

impl Invariants {
    /// Connects to `url` and loads the check into the new session.
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        let mut conn = PgConnection::connect(url).await?;
        sqlx::raw_sql(INVARIANTS).execute(&mut conn).await?;
        // A check holds ACCESS SHARE on every parent while it runs, which a
        // DETACH ... CONCURRENTLY waits out: bound how long.
        sqlx::raw_sql("SET statement_timeout = '20s'")
            .execute(&mut conn)
            .await?;
        Ok(Self { conn })
    }

    /// Every violation in `schema`, read in one snapshot.
    pub async fn check(&mut self, schema: &str, mode: Mode) -> Result<Vec<Violation>, sqlx::Error> {
        let mut tx = self
            .conn
            .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .await?;
        let violations = sqlx::query_as("SELECT * FROM pg_temp.evbk_violations($1, $2)")
            .bind(schema)
            .bind(mode.as_sql())
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(violations)
    }
}
