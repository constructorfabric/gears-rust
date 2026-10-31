use std::borrow::Cow;

use percent_encoding::percent_decode_str;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::infra::storage::migrator::MIGRATIONS_TRACKER;
use crate::infra::storage::topology::{CATALOG_TABLE, RECORDS_TABLE};

/// Return `true` when `url` uses a plaintext `http` connection (no TLS).
///
/// Used by [`ClickHousePluginConfig::validate`] to fail closed on an
/// unencrypted `database_url` by default, and by
/// [`crate::infra::storage::pool::build_client`] for the runtime TLS-posture
/// warning. `pub(crate)` so both call sites — and their unit tests — share
/// one definition instead of duplicating the scheme check.
///
/// The scheme is compared after URL parsing, not as a string prefix: URL
/// schemes are case-insensitive and `Url::parse` normalizes them to lowercase,
/// so `HTTP://host/db` is a cleartext connection that a `starts_with("http://")`
/// test would wave through — bypassing the gate and the warning both.
///
/// Input that does not parse as an absolute URL is reported as plaintext: it
/// cannot be shown to be encrypted, and this is the fail-closed direction.
/// `validate` rejects unparseable input earlier, so in practice this only
/// affects `build_client`'s defense-in-depth warning, where a spurious warning
/// is harmless and a missed one is not.
pub(crate) fn is_plaintext_url(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(parsed) => parsed.scheme() == "http",
        Err(_) => true,
    }
}

/// Percent-decode a `database_url` userinfo component (username or password)
/// into the literal credential expected by `Client::with_user` /
/// `with_password`.
///
/// `Url::username` / `Url::password` return the encoded form, so an operator
/// who embeds a secret containing URL-reserved characters must percent-encode
/// it, and this is the one place it is decoded back. Shared by
/// [`ClickHousePluginConfig::validate`] (fail-closed at startup) and
/// [`crate::infra::storage::pool::build_client`] (the actual decode) so both
/// agree on what is acceptable.
///
/// # Errors
///
/// Returns [`std::str::Utf8Error`] when the decoded bytes are not valid UTF-8.
/// This is deliberately *not* a lossy decode: the `clickhouse` client takes
/// `&str`, and replacing an invalid sequence with U+FFFD would silently turn a
/// corrupted or truncated secret into a *different* credential, so the gear
/// would start and then loop on authentication failures with no hint that the
/// config was wrong. The error carries only a byte offset, never the value.
pub(crate) fn decode_userinfo(encoded: &str) -> Result<String, std::str::Utf8Error> {
    percent_decode_str(encoded)
        .decode_utf8()
        .map(Cow::into_owned)
}

/// Configuration for the `ClickHouse` Usage Collector storage backend.
/// Durations are whole seconds (repo convention).
#[derive(Debug, Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(default, deny_unknown_fields)]
pub struct ClickHousePluginConfig {
    /// `ClickHouse` HTTP endpoint URL including credentials, e.g.
    /// `https://user:${CH_PASSWORD}@host:8443/db`. Held as a
    /// [`SecretString`]: `Debug` emits `[REDACTED]` (so `tracing::debug!(?cfg)`
    /// and panic-formatter dumps never print the resolved URL), there is no
    /// `Display`, `Serialize`, or `PartialEq` to leak the bytes through a
    /// config-snapshot path or an assertion message, and the buffer is zeroized
    /// on drop. The only read accessor is
    /// [`ExposeSecret::expose_secret`](secrecy::ExposeSecret::expose_secret),
    /// so every read site is grep-able; `${VAR}` templating is expanded via the
    /// `#[expand_vars]` derive before any consumer sees the value.
    /// [`Self::validate`] admits only the `http` and `https` schemes, and
    /// rejects a plaintext `http` URL unless [`Self::allow_insecure_http`] is
    /// set. Both checks read the parsed (lowercase-normalized) scheme, so
    /// `HTTP://` is treated as `http://`.
    #[expand_vars]
    pub database_url: SecretString,
    /// Explicit development/test opt-out for a plaintext (`http://`)
    /// `database_url`. `database_url` embeds credentials
    /// ([`Self::database_url`]), so an unencrypted connection sends them —
    /// and every usage record — over the network in cleartext. Defaults to
    /// `false`: [`Self::validate`] rejects a `http://` `database_url` unless
    /// this is explicitly set to `true`. Has no effect on a `https://` URL, and
    /// does not admit a scheme outside `http`/`https`: it is consent to skip
    /// TLS, not consent to a scheme the `ClickHouse` client cannot speak.
    pub allow_insecure_http: bool,
    /// HTTP request timeout in seconds (applies to reads and writes).
    ///
    /// Drives three distinct mechanisms: the `ClickHouse` server settings
    /// `send_timeout` / `receive_timeout`; the client-side deadline from
    /// `Self::client_deadline`, `CLIENT_DEADLINE_GRACE_SECS` later; and — while
    /// [`Self::async_insert`] is enabled — the budget an `INSERT` has to absorb
    /// the server-side async-insert buffer flush, which is why [`Self::validate`]
    /// enforces a floor of `MIN_ASYNC_INSERT_TIMEOUT_SECS` in that case.
    pub request_timeout_secs: u64,
    /// Send **single-row** `usage_records` `INSERT`s with the `ClickHouse`
    /// settings `async_insert = 1` and `wait_for_async_insert = 1`, applied
    /// per-statement via `clickhouse::insert::Insert::with_setting` rather than
    /// on the shared `Client`, so no `SELECT` is affected — not even the
    /// table-metadata read the `clickhouse` crate performs inside
    /// `Client::insert` itself, which runs before this setting is applied.
    ///
    /// `async_insert = 1` moves part formation from the request into a
    /// server-side buffer that coalesces concurrent inserts sharing one
    /// (query, settings, format) triple into shared parts. That is the whole
    /// point here: `create_usage_record` issues one `INSERT` per record, so a
    /// request-shaped ingest stream otherwise writes one small part per request
    /// and drives `ReplacingMergeTree` part count and merge pressure up. Every
    /// such `INSERT` this plugin emits is identical in SQL text, settings, and
    /// format, so the whole ingest stream lands in one queue.
    ///
    /// **Scope.** This setting governs the single-row path only. Multi-row
    /// `usage_records` `INSERT`s — `create_usage_records` and the deactivation
    /// cascade — stay synchronous whatever this says, because the async buffer
    /// does not guarantee that one statement's rows become visible in a single
    /// commit and both depend on that (the cascade's marker rows must flip
    /// together). `usage_type_catalog` writes are likewise always synchronous:
    /// that table is control-plane (written only by `create_usage_type`, never
    /// deleted, unpartitioned, tens of rows), so there is no concurrent insert
    /// stream for the buffer to coalesce and no part count to reduce. Nothing
    /// is given up by either exclusion — a multi-row statement already carries
    /// its rows in one part, and the part explosion this setting exists to fix
    /// is specific to one-row-per-request writes.
    ///
    /// `wait_for_async_insert = 1` is pinned, not separately configurable.
    /// Two properties depend on it:
    ///
    /// * **Durability ack.** With `0` the `INSERT` returns as soon as the row
    ///   is buffered, so a server restart loses acknowledged usage records.
    ///   This plugin is the system of record; it cannot answer `Ok` for a row
    ///   that is not committed.
    /// * **Read-your-writes.** `create_usage_record` reads (the dedup
    ///   point-lookup) and then writes, and a retry of the same request must
    ///   observe the earlier insert or it inserts a second time under an
    ///   idempotency key already in use. Only `1` makes the row queryable by
    ///   the time the call returns.
    ///
    /// The cost is latency: the `INSERT` blocks until the buffer flushes,
    /// bounded by the server-side `async_insert_busy_timeout_ms` (adaptive on
    /// `ClickHouse` 24.x+: `async_insert_busy_timeout_min_ms` 50ms →
    /// `async_insert_busy_timeout_max_ms` 200ms) plus the part commit. That
    /// budget is deliberately left to the server rather than mirrored into a
    /// config field — it is a property of the cluster's whole insert workload,
    /// not of this plugin, the server adapts it automatically, and a
    /// plugin-pinned value would defeat that adaptation. An operator who must
    /// pin it should do so in a `ClickHouse` settings profile for the plugin's
    /// DB user, the same mechanism the README already recommends for bounding
    /// read cost.
    ///
    /// Because the flush wait is charged against the request budget,
    /// [`Self::validate`] requires
    /// `request_timeout_secs >= MIN_ASYNC_INSERT_TIMEOUT_SECS` while this is
    /// enabled.
    ///
    /// **Insert dedup token.** Every `usage_records` `INSERT` carries an
    /// `insert_deduplication_token` (see `record_store::insert_dedup_token`),
    /// and `configure_insert` adds `async_insert_deduplicate = 1` when this is
    /// on. `ClickHouse` enforces the token on synchronous inserts against the
    /// table's `non_replicated_deduplication_window`; for *asynchronous*
    /// inserts it enforces it only on `Replicated*` engines. On the shipped
    /// non-replicated table, therefore, a racing duplicate single-record
    /// create is collapsed by `optimize_on_insert` when both land in one
    /// flush and by the background merge otherwise — visible twice to
    /// `list`/`aggregate` (which no longer resolve versions at read time) until
    /// then, while `get` resolves it immediately. Batches and deactivation
    /// markers are always synchronous and always engine-deduplicated.
    ///
    /// Set to `false` for the pre-async behaviour (one synchronous part write
    /// per request) on a deployment whose `ClickHouse` version or settings
    /// profile makes async inserts unavailable — or to make the engine-side
    /// dedup of single-record creates deterministic.
    pub async_insert: bool,
    /// `usage_records` retention window in seconds; rows older are deleted via
    /// `ClickHouse` TTL. Must be in `(0, MAX_RETENTION_SECS]`.
    pub retention_period_secs: u64,
    /// Vendor name for GTS instance registration.
    pub vendor: String,
    /// Plugin priority (lower = higher priority).
    pub priority: i16,
    /// Opt-in sharded cluster topology. `None` (the default) is the
    /// single-node deployment: the plugin provisions and reconciles its own
    /// schema. `Some` switches to the cluster schema — `Distributed` tables
    /// named `usage_records` / `usage_type_catalog` over
    /// `ReplicatedReplacingMergeTree` local tables — which the plugin
    /// validates at startup. By default the operator provisions it (the DDL
    /// is `migrator::render_ddl`, printed by the startup check when missing); with
    /// [`ClickHouseClusterConfig::manage_schema`] the plugin runs the cluster
    /// migrations itself.
    pub cluster: Option<ClickHouseClusterConfig>,
}

/// Sharded-cluster topology for [`ClickHousePluginConfig::cluster`].
///
/// Every identifier here is interpolated into SQL (`ON CLUSTER <name>`,
/// `ALTER TABLE <local> …`), so [`ClickHousePluginConfig::validate`] admits
/// only plain `ClickHouse` identifiers.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClickHouseClusterConfig {
    /// Cluster name as declared in the server's `remote_servers` config.
    pub name: String,
    /// Shard-local `ReplicatedReplacingMergeTree` table behind the
    /// `usage_records` `Distributed` table. Must not be a name the plugin
    /// creates itself (`usage_records`, `usage_type_catalog`,
    /// `usage_collector_migrations`).
    pub records_local_table: String,
    /// Shard-local `ReplicatedReplacingMergeTree` table behind the
    /// `usage_type_catalog` `Distributed` table. Same reserved names as
    /// [`Self::records_local_table`].
    pub catalog_local_table: String,
    /// Optional `insert_quorum` sent with every insert: a replica count, or
    /// `auto` (majority). Unset leaves the server default (no quorum), so an
    /// insert is acknowledged once one replica of the target shard has it.
    pub insert_quorum: Option<String>,
    /// Run the cluster migrations (`ON CLUSTER` DDL) at startup instead of
    /// expecting an operator-provisioned schema. Off by default: the plugin's
    /// user then needs `CREATE` / `ALTER` and the `CLUSTER` privilege.
    pub manage_schema: bool,
    /// Budget for one cluster migration statement (the migrations and the TTL
    /// reconciliation), in seconds. `ON CLUSTER` DDL waits for every host, so
    /// it does not fit the request-path deadline. Sets the server-side
    /// `send_timeout`, `receive_timeout` and `distributed_ddl_task_timeout`
    /// (`migrator::migration_client`); the client-side deadline adds a grace.
    pub migration_statement_timeout_secs: u64,
}

impl Default for ClickHouseClusterConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            records_local_table: "usage_records_local".to_owned(),
            catalog_local_table: "usage_type_catalog_local".to_owned(),
            insert_quorum: None,
            manage_schema: false,
            migration_statement_timeout_secs: 3600,
        }
    }
}

/// Tables the plugin creates under fixed names, which a configured local
/// table must not reuse.
const RESERVED_TABLE_NAMES: [&str; 3] = [RECORDS_TABLE, CATALOG_TABLE, MIGRATIONS_TRACKER];

/// Whether `s` is a plain `ClickHouse` identifier (`[A-Za-z_][A-Za-z0-9_]*`),
/// safe to interpolate unquoted into SQL.
pub(crate) fn is_plain_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl ClickHouseClusterConfig {
    /// Client-side deadline for one cluster migration statement:
    /// [`Self::migration_statement_timeout_secs`] plus the same grace
    /// [`ClickHousePluginConfig::client_deadline`] adds, so the server's own
    /// timeout fires first.
    #[must_use]
    pub fn migration_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.migration_statement_timeout_secs
                .saturating_add(CLIENT_DEADLINE_GRACE_SECS),
        )
    }

    /// # Errors
    ///
    /// Returns an error string for an identifier that is not a plain
    /// `ClickHouse` identifier, two local tables sharing a name, a local table
    /// named after a table the plugin creates itself, an `insert_quorum` that is neither `auto` nor a positive integer, or a
    /// zero migration statement timeout.
    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("cluster.name", &self.name),
            ("cluster.records_local_table", &self.records_local_table),
            ("cluster.catalog_local_table", &self.catalog_local_table),
        ] {
            if !is_plain_identifier(value) {
                return Err(format!(
                    "{field} = `{value}` is not a plain ClickHouse identifier \
                     ([A-Za-z_][A-Za-z0-9_]*); it is interpolated into SQL"
                ));
            }
        }
        if self.records_local_table == self.catalog_local_table {
            return Err(
                "cluster.records_local_table and cluster.catalog_local_table must differ"
                    .to_owned(),
            );
        }
        // Exact match: `ClickHouse` table names are case-sensitive.
        for (field, value) in [
            ("cluster.records_local_table", &self.records_local_table),
            ("cluster.catalog_local_table", &self.catalog_local_table),
        ] {
            if RESERVED_TABLE_NAMES.contains(&value.as_str()) {
                return Err(format!(
                    "{field} = `{value}` is reserved: the plugin creates a table of that name itself"
                ));
            }
        }
        if let Some(quorum) = &self.insert_quorum
            && quorum != "auto"
            && !quorum.parse::<u64>().is_ok_and(|n| n > 0)
        {
            return Err(format!(
                "cluster.insert_quorum = `{quorum}` must be `auto` or a positive replica count"
            ));
        }
        if self.migration_statement_timeout_secs == 0 {
            return Err(
                "cluster.migration_statement_timeout_secs must be greater than 0".to_owned(),
            );
        }
        Ok(())
    }
}

impl Default for ClickHousePluginConfig {
    fn default() -> Self {
        Self {
            database_url: SecretString::default(),
            allow_insecure_http: false,
            request_timeout_secs: 30,
            // On by default: the write path is one INSERT per request, so
            // without server-side coalescing a request-shaped ingest stream
            // writes one small part per record.
            async_insert: true,
            // Same window the migration DDL bakes in, so a config-less start
            // needs no `MODIFY TTL` reconciliation at startup.
            retention_period_secs: crate::infra::storage::pool::DEFAULT_RETENTION_SECS,
            vendor: "constructorfabric".to_owned(),
            // One below the TimescaleDB plugin's default of 10, so with both
            // registered under default config the choice is deterministic
            // rather than decided by types-registry iteration order.
            priority: 11,
            cluster: None,
        }
    }
}

/// Upper bound on `retention_period_secs` (100 years in seconds).
///
/// `ClickHouse`'s TTL interval expression is evaluated as a `DateTime` offset;
/// a pathological retention window would overflow the `DateTime` type and
/// surface as a confusing DDL failure at schema-provisioning time. 100 years
/// is far beyond any realistic usage-data retention while staying safely inside
/// `ClickHouse`'s `DateTime64` range.
const MAX_RETENTION_SECS: u64 = 100 * 365 * 86_400;

/// Grace added to the server-side timeout to form the client-side deadline
/// (see [`ClickHousePluginConfig::client_deadline`]).
pub(crate) const CLIENT_DEADLINE_GRACE_SECS: u64 = 5;

/// Floor on `request_timeout_secs` while
/// [`ClickHousePluginConfig::async_insert`] is enabled.
///
/// `wait_for_async_insert = 1` blocks the `INSERT` until the server flushes
/// its async-insert buffer — up to `async_insert_busy_timeout_ms` (server
/// default; adaptive 50-200ms on `ClickHouse` 24.x+) plus the part commit. A 1s
/// server-side budget leaves under 800ms of headroom for the commit and
/// surfaces under load as intermittent `Transient` insert timeouts, which read
/// as backend flakiness rather than as the misconfiguration they are. Failing
/// at startup with both values named is strictly better.
///
/// The default `request_timeout_secs` of 30 clears this by more than an order
/// of magnitude; only a deliberately tiny configured budget trips it.
pub(crate) const MIN_ASYNC_INSERT_TIMEOUT_SECS: u64 = 2;

impl ClickHousePluginConfig {
    /// Client-side deadline for a single `ClickHouse` request.
    ///
    /// [`Self::request_timeout_secs`] is forwarded to `ClickHouse` as the
    /// server settings `send_timeout` / `receive_timeout`, which the *server*
    /// applies to its own socket handling. They therefore do nothing when a
    /// connection is accepted and then never answered, or when an intermediary
    /// holds the socket open — cases where a request would otherwise hang
    /// indefinitely. This deadline is the client-side backstop for exactly
    /// those cases.
    ///
    /// It sits [`CLIENT_DEADLINE_GRACE_SECS`] *past* the server-side budget so
    /// that when the server is answering, its own timeout fires first and the
    /// caller gets the server's descriptive error rather than a bare
    /// client-side timeout.
    pub(crate) fn client_deadline(&self) -> std::time::Duration {
        // `validate` bounds `request_timeout_secs` only from below (> 0), so a
        // pathologically large configured value must not overflow the add.
        std::time::Duration::from_secs(
            self.request_timeout_secs
                .saturating_add(CLIENT_DEADLINE_GRACE_SECS),
        )
    }
    /// Validate invariants not expressible in the type system.
    ///
    /// # Errors
    ///
    /// Returns an error string for an empty `database_url`, one whose scheme is
    /// neither `http` nor `https`, one whose percent-encoded username or
    /// password does not decode to valid UTF-8 (see [`decode_userinfo`]), a
    /// plaintext `http` `database_url` without
    /// [`Self::allow_insecure_http`], a zero timeout, a
    /// `request_timeout_secs` below `MIN_ASYNC_INSERT_TIMEOUT_SECS` while
    /// [`Self::async_insert`] is enabled, a retention window outside
    /// `(0, MAX_RETENTION_SECS]`, or a blank `vendor`.
    pub fn validate(&self) -> Result<(), String> {
        if self.database_url.expose_secret().trim().is_empty() {
            return Err("database_url must not be empty".to_owned());
        }
        let parsed = match url::Url::parse(self.database_url.expose_secret()) {
            Ok(parsed) => parsed,
            Err(e) => return Err(format!("database_url must be a valid absolute URL: {e}")),
        };
        // The `clickhouse` crate speaks only ClickHouse's HTTP interface, so a
        // native-protocol or non-network scheme is a misconfiguration that would
        // otherwise surface as an opaque request failure on the first query.
        // Only the scheme is interpolated: `database_url` embeds credentials.
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!(
                "database_url scheme `{}` is not supported; the ClickHouse client speaks the \
                 HTTP interface only, so database_url must use https:// (or http:// with \
                 allow_insecure_http = true for local development/test)",
                parsed.scheme()
            ));
        }
        // Credentials embedded in the URL are percent-encoded and must decode
        // to valid UTF-8; otherwise `build_client` would either alter the secret
        // (lossy decode) or drop it, and the gear would start and then loop on
        // authentication failures. Only the component name and the decoder's
        // byte offset are reported — never the credential itself.
        // (An absent username is the empty string, which decodes trivially.)
        if let Err(e) = decode_userinfo(parsed.username()) {
            return Err(format!(
                "database_url username is not valid UTF-8 after percent-decoding ({e}); \
                 check the percent-encoding of the embedded credential"
            ));
        }
        if let Some(Err(e)) = parsed.password().map(decode_userinfo) {
            return Err(format!(
                "database_url password is not valid UTF-8 after percent-decoding ({e}); \
                 check the percent-encoding of the embedded credential"
            ));
        }
        if self.request_timeout_secs == 0 {
            return Err("request_timeout_secs must be > 0".to_owned());
        }
        // Ordered after the zero check so a zero value keeps the clearer error.
        // `wait_for_async_insert = 1` charges the server-side buffer flush
        // against the request budget, so too small a budget turns every insert
        // into an intermittent timeout that reads as backend flakiness.
        if self.async_insert && self.request_timeout_secs < MIN_ASYNC_INSERT_TIMEOUT_SECS {
            return Err(format!(
                "request_timeout_secs = {} is too small with async_insert = true: \
                 wait_for_async_insert = 1 blocks each INSERT until the server flushes its \
                 async-insert buffer (async_insert_busy_timeout_ms, server default), so the \
                 request budget must be at least {MIN_ASYNC_INSERT_TIMEOUT_SECS}s; raise \
                 request_timeout_secs or set async_insert = false",
                self.request_timeout_secs
            ));
        }
        if self.retention_period_secs == 0 {
            return Err("retention_period_secs must be > 0".to_owned());
        }
        if self.retention_period_secs > MAX_RETENTION_SECS {
            return Err(format!(
                "retention_period_secs must be <= {MAX_RETENTION_SECS} (100 years); \
                 a larger window overflows the ClickHouse DateTime64 TTL expression"
            ));
        }
        if self.vendor.trim().is_empty() {
            return Err(
                "vendor must not be empty; it is part of the GTS instance identity registered \
                 with types-registry (e.g. vendor = \"constructorfabric\")"
                    .to_owned(),
            );
        }
        // Fail closed on a plaintext database_url: it embeds credentials and
        // carries every usage record, so an unencrypted connection is a
        // credential- and data-exposure risk, not just a style choice. The
        // override must be set explicitly and is not implied by any other
        // field (e.g. a `http://` scheme alone is never sufficient consent).
        if is_plaintext_url(self.database_url.expose_secret()) && !self.allow_insecure_http {
            return Err(
                "database_url uses a plaintext http:// scheme, which sends credentials and \
                 usage data unencrypted; use https:// or set allow_insecure_http = true to \
                 explicitly opt out for local development/test only"
                    .to_owned(),
            );
        }
        if let Some(cluster) = &self.cluster {
            cluster.validate()?;
            // An async insert into a `Distributed` table is buffered on the
            // initiator, not on the shard that commits it, so neither the
            // `wait_for_async_insert` durability ack nor the insert dedup
            // token reaches the replicated table the way `async_insert`'s
            // contract assumes. Refuse the combination rather than weaken it.
            if self.async_insert {
                return Err(
                    "async_insert = true is not supported with a cluster topology: inserts go \
                     through Distributed tables synchronously (distributed_foreground_insert = 1) \
                     so the insert dedup token reaches the replicated shard table; set \
                     async_insert = false"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
