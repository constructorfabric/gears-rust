//! `PostgresCache` — the native `ClusterCacheBackend` implementation over a
//! `sqlx::PgPool` (DESIGN.md §4).

use std::sync::Arc;

use async_trait::async_trait;
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend, ClusterError,
    ProviderErrorKind,
};
use sqlx::PgPool;

pub mod reaper;
pub mod watch;

use crate::pg_error::map_sqlx_error;
use watch::{NotifyEvent, WatchRegistry, validate_key_len};

/// The native Postgres cache backend. Read-through: every `get` hits the
/// database directly (DESIGN.md §4.3, "no read-path cache" — §11).
pub struct PostgresCache {
    pool: PgPool,
    /// The schema-qualified table name (`"<schema>.cluster_cache"`), computed
    /// once at construction. `schema` comes from operator config
    /// (`PostgresClusterConfig`/`PostgresLockConfig`), the same trust boundary
    /// as `connection_string` — not sanitized against a hostile value the way
    /// tenant-supplied input would be.
    table: String,
    watch_registry: Arc<WatchRegistry>,
}

impl PostgresCache {
    #[must_use]
    pub fn new(pool: PgPool, schema: &str) -> Arc<Self> {
        Arc::new(Self {
            pool,
            table: format!("{schema}.cluster_cache"),
            watch_registry: WatchRegistry::new(),
        })
    }

    /// The watch registry the LISTEN fan-out task dispatches into. Exposed to
    /// `plugin.rs` so it can spawn `watch::spawn_listen_task` against the same
    /// registry this cache registers watches into. `pub(crate)`: only the
    /// wiring in `plugin.rs` uses it, and `WatchRegistry` is not a nameable
    /// public type (PGR-L3).
    #[must_use]
    pub(crate) fn watch_registry(&self) -> Arc<WatchRegistry> {
        Arc::clone(&self.watch_registry)
    }

    /// The underlying pool, for the reaper and shutdown path. `pub(crate)`:
    /// intra-crate wiring only (PGR-L3).
    #[must_use]
    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// The schema-qualified table name, for the reaper. `pub(crate)`:
    /// intra-crate wiring only (PGR-L3).
    #[must_use]
    pub(crate) fn table(&self) -> String {
        self.table.clone()
    }
}

/// Converts a `Ttl` to the millisecond lifetime bound to the write query, or
/// `None` for `Ttl::Indefinite` (DESIGN.md §4.1).
///
/// The actual `expires_at` is computed **in SQL as `now() + interval`** using
/// the database clock (PGR-C2), not `chrono::Utc::now()` on the writing
/// instance: reads (`get`/`contains`/CAS) and the reaper all compare against
/// Postgres `now()`, so anchoring the write to a service instance's own
/// (possibly skewed) wall clock could make entries expire early or linger. This
/// helper only validates and hands the SQL a duration to add to the DB clock.
fn ttl_to_millis(ttl: Ttl) -> Result<Option<i64>, ClusterError> {
    match ttl {
        Ttl::Indefinite => Ok(None),
        Ttl::Of(duration) => {
            let millis =
                i64::try_from(duration.as_millis()).map_err(|_| ClusterError::InvalidConfig {
                    reason: format!("ttl {duration:?} is out of range: exceeds i64 milliseconds"),
                })?;
            Ok(Some(millis))
        }
    }
}

/// The SQL fragment computing `expires_at` from a bound millisecond lifetime
/// (`$n`) against the database clock: `NULL` (indefinite) when the bind is
/// `NULL`, else `now() + $n ms` (PGR-C2). `n` is the 1-based bind position of
/// the `ttl_to_millis` value.
fn expires_at_sql(n: usize) -> String {
    format!(
        "CASE WHEN ${n}::bigint IS NULL THEN NULL \
         ELSE now() + (${n}::bigint * interval '1 millisecond') END"
    )
}

/// The `version` column is `BIGINT` (`i64`); the SDK contract is `u64`
/// (`>= 1`, DESIGN.md §2.2). This plugin never writes a version beyond
/// `i64::MAX` in practice (each write increments by exactly 1), but the
/// conversion is checked rather than cast, per the "no unwrap/expect, use
/// proper Result types" rule.
fn version_to_i64(version: u64) -> Result<i64, ClusterError> {
    i64::try_from(version).map_err(|_| ClusterError::Provider {
        kind: ProviderErrorKind::Other,
        message: format!("version {version} exceeds the storable i64 range"),
    })
}

fn i64_to_version(version: i64) -> Result<u64, ClusterError> {
    u64::try_from(version).map_err(|_| ClusterError::Provider {
        kind: ProviderErrorKind::Other,
        message: format!("stored version {version} is negative — data corruption"),
    })
}

/// Escapes `%`, `_`, and `\` in a `scan_prefix` prefix so `LIKE $1` matches the
/// prefix literally rather than treating the caller's own text as `LIKE`
/// metacharacters (DESIGN.md §4.4 says "the caller must not include a
/// wildcard", covering the trailing `%` this method appends — a key
/// containing a literal `%`/`_` in its own text still needs escaping to be
/// matched as data).
fn escape_like(prefix: &str) -> String {
    let mut escaped = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[async_trait]
impl ClusterCacheBackend for PostgresCache {
    fn consistency(&self) -> CacheConsistency {
        // READ COMMITTED provides linearizability for the single-row operations
        // this cache uses; CAS is an atomic `UPDATE ... WHERE version = $expected`
        // regardless of isolation level (DESIGN.md §4.5).
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        // The NOTIFY channel carries a single key per payload — prefix routing
        // is infeasible at the Postgres level without one channel per prefix
        // (DESIGN.md §4.3).
        CacheFeatures::new(false)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(&format!(
            "SELECT value, version FROM {} WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())",
            self.table
        ))
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        row.map(|(value, version)| {
            Ok(CacheEntry {
                value,
                version: i64_to_version(version)?,
            })
        })
        .transpose()
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        validate_key_len(req.key)?;
        let ttl_millis = ttl_to_millis(req.ttl)?;

        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query(&format!(
            "INSERT INTO {table} (key, value, version, expires_at) VALUES ($1, $2, 1, {expires_at}) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, \
             version = {table}.version + 1, expires_at = EXCLUDED.expires_at",
            table = self.table,
            expires_at = expires_at_sql(3),
        ))
        .bind(req.key)
        .bind(req.value)
        .bind(ttl_millis)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        watch::notify(&mut *tx, NotifyEvent::Changed, req.key).await?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        // Live-entry predicate (PGR-C5): an expired-but-not-yet-reaped row is
        // logically absent to `get`/`contains`/CAS, so deleting it must not
        // report `true` or emit a spurious `Deleted` — the reaper will physically
        // remove it and emit `Expired`. Matching the filter keeps `delete`
        // consistent with the read path.
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let deleted: Option<i32> = sqlx::query_scalar(&format!(
            "DELETE FROM {} WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) \
             RETURNING 1",
            self.table
        ))
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        let existed = deleted.is_some();
        if existed {
            watch::notify(&mut *tx, NotifyEvent::Deleted, key).await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(existed)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let exists: Option<i32> = sqlx::query_scalar(&format!(
            "SELECT 1 FROM {} WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())",
            self.table
        ))
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(exists.is_some())
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        validate_key_len(req.key)?;
        let ttl_millis = ttl_to_millis(req.ttl)?;

        // `ON CONFLICT (key) DO UPDATE ... WHERE <row is expired>`, not
        // `DO NOTHING`: a key whose row physically lingers past `expires_at`
        // (not yet swept by the TTL reaper) is logically *absent*, exactly as
        // `get`/`contains`/`compare_and_swap` treat it via their
        // `(expires_at IS NULL OR expires_at > now())` filter. `DO NOTHING`
        // returned zero rows for such a row → `Ok(None)` ("present"), which
        // stalled `CasBasedLeaderElectionBackend::claim()`'s failover
        // `put_if_absent` until the reaper physically deleted the row (up to
        // `cache_reaper_interval_ms`, default 10s) — a liveness regression on
        // the leader-election failover path (DESIGN.md §4.1). The guarded
        // upsert overwrites an expired row (returning it as a freshly-created
        // version-1 entry) while a live entry still yields no row → `Ok(None)`.
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let inserted: Option<(Vec<u8>, i64)> = sqlx::query_as(&format!(
            "INSERT INTO {table} (key, value, version, expires_at) VALUES ($1, $2, 1, {expires_at}) \
             ON CONFLICT (key) DO UPDATE \
               SET value = EXCLUDED.value, version = 1, expires_at = EXCLUDED.expires_at \
               WHERE {table}.expires_at IS NOT NULL AND {table}.expires_at <= now() \
             RETURNING value, version",
            table = self.table,
            expires_at = expires_at_sql(3),
        ))
        .bind(req.key)
        .bind(req.value)
        .bind(ttl_millis)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        if let Some((value, version)) = inserted {
            watch::notify(&mut *tx, NotifyEvent::Changed, req.key).await?;
            tx.commit().await.map_err(map_sqlx_error)?;
            Ok(Some(CacheEntry {
                value,
                version: i64_to_version(version)?,
            }))
        } else {
            tx.rollback().await.map_err(map_sqlx_error)?;
            Ok(None)
        }
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        validate_key_len(key)?;
        let ttl_millis = ttl_to_millis(ttl)?;
        let expected_version_i64 = version_to_i64(expected_version)?;

        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let updated: Option<i64> = sqlx::query_scalar(&format!(
            "UPDATE {table} SET value = $3, version = version + 1, expires_at = {expires_at} \
             WHERE key = $1 AND version = $2 AND (expires_at IS NULL OR expires_at > now()) \
             RETURNING version",
            table = self.table,
            expires_at = expires_at_sql(4),
        ))
        .bind(key)
        .bind(expected_version_i64)
        .bind(new_value)
        .bind(ttl_millis)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        if let Some(version) = updated {
            watch::notify(&mut *tx, NotifyEvent::Changed, key).await?;
            tx.commit().await.map_err(map_sqlx_error)?;
            Ok(CacheEntry {
                value: new_value.to_vec(),
                version: i64_to_version(version)?,
            })
        } else {
            tx.rollback().await.map_err(map_sqlx_error)?;
            let current = self.get(key).await?;
            Err(ClusterError::CasConflict {
                key: key.to_owned(),
                current,
            })
        }
    }

    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        // Overridden (not the default get-then-delete) for atomicity, per the
        // trait doc's guidance for a backend with an atomic store. Survives the
        // delete+recreate version-reset scenario documented in
        // `[cluster-cache-version-reset-caveat]` (DESIGN.md §2.2): a successor
        // that re-claimed after a TTL lapse writes a different value, so this
        // guarded delete is a safe no-op against it.
        // Live-entry predicate (PGR-C5): as in `delete`, an expired-but-unreaped
        // row is logically absent, so a value match against it must not report a
        // successful delete or emit `Deleted`.
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let deleted: Option<i32> = sqlx::query_scalar(&format!(
            "DELETE FROM {} WHERE key = $1 AND value = $2 \
             AND (expires_at IS NULL OR expires_at > now()) RETURNING 1",
            self.table
        ))
        .bind(key)
        .bind(expected_value)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        let existed = deleted.is_some();
        if existed {
            watch::notify(&mut *tx, NotifyEvent::Deleted, key).await?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(existed)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        Ok(self.watch_registry.register(key))
    }

    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        // DESIGN.md §4.3: exact watches only — prefix routing is infeasible at
        // the Postgres NOTIFY level. Callers polyfill via `PollingPrefixWatch`.
        Err(ClusterError::Unsupported {
            feature: "prefix_watch",
        })
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let pattern = format!("{}%", escape_like(prefix));
        let keys: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT key FROM {} WHERE key LIKE $1 ESCAPE '\\' AND (expires_at IS NULL OR expires_at > now())",
            self.table
        ))
        .bind(pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn ttl_indefinite_has_no_millis() {
        assert_eq!(ttl_to_millis(Ttl::Indefinite).unwrap(), None);
    }

    #[test]
    fn ttl_of_duration_is_positive_millis() {
        assert_eq!(
            ttl_to_millis(Ttl::Of(Duration::from_mins(1))).unwrap(),
            Some(60_000)
        );
    }

    #[test]
    fn expires_at_sql_uses_the_database_clock() {
        // Guards PGR-C2: expiry is computed as `now() + interval` in SQL, never
        // from a client-side timestamp bind.
        let sql = expires_at_sql(3);
        assert!(sql.contains("now()"), "must anchor to the DB clock: {sql}");
        assert!(
            sql.contains("$3::bigint"),
            "must bind the ms lifetime: {sql}"
        );
    }

    #[test]
    fn version_round_trips_through_i64() {
        for version in [1_u64, 2, 1_000_000, u64::from(u32::MAX)] {
            let as_i64 = version_to_i64(version).unwrap();
            assert_eq!(i64_to_version(as_i64).unwrap(), version);
        }
    }

    #[test]
    fn version_to_i64_rejects_values_beyond_i64_max() {
        let too_large = u64::try_from(i64::MAX).unwrap() + 1;
        assert!(version_to_i64(too_large).is_err());
    }

    #[test]
    fn i64_to_version_rejects_negative_values() {
        assert!(i64_to_version(-1).is_err());
    }

    #[test]
    fn escape_like_escapes_percent_underscore_and_backslash() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like("100%_off\\"), "100\\%\\_off\\\\");
    }
}
