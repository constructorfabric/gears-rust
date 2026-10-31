//! `ClickHouse` connection-pool bootstrap and schema migration.
//!
//! Exposes six entry points:
//! - [`build_client`] — constructs and configures the `clickhouse::Client`.
//!   Its transport comes from `new_base_client`, the one place this plugin
//!   chooses an HTTP stack (and therefore a crypto provider).
//! - `configure_insert` — applies this plugin's per-`INSERT` timeouts and
//!   settings (including the insert dedup token) to a freshly acquired
//!   `Insert` handle.
//! - `exec_statement` — runs one migration DDL statement against the
//!   connected `ClickHouse` instance under a deadline; `migrator` calls it.
//! - [`ensure_retention_ttl`] — reconciles `usage_records` TTL with config.
//! - [`ensure_insert_dedup_window`] — retrofits the `usage_records` insert
//!   dedup window onto tables created before the migration carried it (the
//!   body of the single-node `m0002_insert_dedup_window` step).
//! - [`validate_cluster_schema`] — checks the cluster schema at startup.
use std::time::Duration;

use anyhow::Context as _;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::config::{ClickHousePluginConfig, decode_userinfo, is_plaintext_url};
use crate::infra::storage::migrator::{self, m0001_init, table};
use crate::infra::storage::topology::{
    CATALOG_SHARDING_KEY, CATALOG_TABLE, RECORDS_SHARDING_KEY, RECORDS_TABLE, Topology,
};

/// Endpoint components split out of a `database_url` for the `clickhouse`
/// crate's connection-configuration methods.
///
/// The `clickhouse` 0.15.x crate's `Client::with_url` stores the given string
/// verbatim as the base request URL — every query/insert does
/// `Url::parse(&client.url)` and only clears/rewrites the *query string*
/// (`query_pairs_mut().clear()`); the URL's **path** and **userinfo** pass
/// straight through unchanged (see `clickhouse::query::do_execute` /
/// `insert_formatted`). So a `database_url` like
/// `http://user:pass@host:8123/mydb` sent as-is to `with_url` would request
/// the literal HTTP path `/mydb` on every call — which `ClickHouse`'s HTTP
/// interface rejects with "There is no handle /mydb..." (only `/`, `/ping`,
/// etc. are implemented) — while silently discarding the credentials, since
/// `with_url` never touches `Client::authentication` / `Client::database`.
/// Those must instead be extracted here and applied via
/// `with_user`/`with_password`/`with_database`, matching this plugin's own
/// documented config contract (README: `database_url = "https://user:pass@host:8443/db"`).
struct ParsedEndpoint {
    /// Bare scheme + host + port, no userinfo/path/query — safe to pass to
    /// `Client::with_url`.
    base_url: String,
    user: Option<String>,
    password: Option<SecretString>,
    database: Option<String>,
}

/// Split a `database_url` into a bare HTTP endpoint plus optional
/// user/password/database.
///
/// Username and password come from the URL's userinfo and are
/// percent-decoded before being returned: `Url::username` / `Url::password`
/// yield the encoded form, but `Client::with_user` / `with_password` need the
/// literal credentials. Callers embedding `${VAR}`-expanded secrets that
/// contain URL-reserved characters must still percent-encode them in
/// `database_url` so the URL parses, and the encoded bytes must decode to
/// valid UTF-8. The database name is the URL path with leading/trailing
/// slashes trimmed; an empty path yields `None` (`ClickHouse` then uses the
/// server's default database for the resolved user).
///
/// # Errors
///
/// Returns an error if `database_url` is not a valid absolute URL, or if its
/// percent-encoded username or password does not decode to valid UTF-8 (see
/// [`decode_userinfo`]). Neither error carries the credential value.
fn parse_endpoint(database_url: &str) -> anyhow::Result<ParsedEndpoint> {
    let mut url = Url::parse(database_url).context("database_url is not a valid absolute URL")?;

    let user = if url.username().is_empty() {
        None
    } else {
        Some(
            decode_userinfo(url.username())
                .context("database_url username is not valid UTF-8 after percent-decoding")?,
        )
    };
    let password = match url.password() {
        Some(p) => Some(SecretString::from(decode_userinfo(p).context(
            "database_url password is not valid UTF-8 after percent-decoding",
        )?)),
        None => None,
    };
    let database = {
        let path = url.path().trim_matches('/');
        (!path.is_empty()).then(|| path.to_owned())
    };

    // Strip userinfo/path/query so the remaining string is a bare endpoint
    // safe to hand to `Client::with_url` (see struct-level doc comment).
    // `set_username`/`set_password` only fail for schemes that cannot carry
    // credentials (e.g. `file:`); an http(s) URL that parsed successfully
    // above always accepts them, so the `Result` is deliberately ignored.
    url.set_username("").ok();
    url.set_password(None).ok();
    url.set_path("");
    url.set_query(None);

    Ok(ParsedEndpoint {
        base_url: url.to_string(),
        user,
        password,
        database,
    })
}

/// Default `usage_records` TTL the single-node schema is created with (1 year
/// in seconds); `ensure_retention_ttl` then reconciles it with config.
pub(crate) const DEFAULT_RETENTION_SECS: u64 = 365 * 86_400;

/// TCP keepalive applied to the `ClickHouse` HTTP connector.
const TCP_KEEPALIVE: Duration = Duration::from_mins(1);

/// Idle-socket timeout for the `ClickHouse` HTTP connection pool.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// Mint a bare `clickhouse::Client` — no URL, credentials or settings — over
/// an HTTP stack this crate controls.
///
/// # Why not `clickhouse::Client::default()`
///
/// Under the workspace's `rustls-tls` feature, the crate's
/// `http_client::default()` (`clickhouse-0.15.1/src/http_client.rs:46-76`)
/// hardcodes the **non**-FIPS `aws_lc_rs` provider, ignoring whatever
/// `toolkit::bootstrap::init_crypto_provider` installed process-wide. A FIPS
/// server would pass its `provider.fips()` witness and still negotiate
/// `ClickHouse` connections through non-validated crypto. The provider is
/// reached from *source*, not a distinct crate, so `make fips-policy` cannot
/// catch it.
///
/// So the connector is rebuilt from the installed provider and passed to
/// `Client::with_http_client`. Every other knob `http_client::default()` sets
/// is reproduced verbatim — including **webpki** roots, since the OS trust
/// store would change which CAs are accepted — so only the backend differs,
/// and in a non-FIPS build it resolves to the same one.
///
/// # Errors
///
/// Fail-closed when no rustls `CryptoProvider` is installed: building from an
/// ad-hoc one would mask a misconfigured bootstrap rather than surface it.
fn new_base_client() -> anyhow::Result<clickhouse::Client> {
    // `get_default` reads the process-wide provider rustls itself holds, and
    // returns `None` when none was installed — no fallback that would mint one
    // from the calling crate's own `cfg!`, which is the source-level provider
    // selection this function exists to eliminate. Fail closed instead, so a
    // misconfigured bootstrap surfaces.
    let provider = rustls::crypto::CryptoProvider::get_default().context(
        "cannot build the ClickHouse client: call \
         toolkit::bootstrap::init_crypto_provider() first",
    )?;

    let mut connector = HttpConnector::new();
    connector.set_keepalive(Some(TCP_KEEPALIVE));
    // The crate computes this as `!cfg!(any(native-tls, rustls-tls-aws-lc,
    // rustls-tls-ring))`, which is `false` for the workspace's feature set —
    // https:// URLs must reach the TLS connector rather than be rejected.
    connector.enforce_http(false);

    // The builder takes `impl Into<Arc<CryptoProvider>>`; `get_default` hands
    // back a `&Arc`, so this clones the handle — same provider identity, no
    // deep copy of the provider itself.
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_provider_and_webpki_roots(provider.clone())
        .context("failed to build the ClickHouse TLS connector from the installed CryptoProvider")?
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector);

    let http = HyperClient::builder(TokioExecutor::new())
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .build(connector);

    Ok(clickhouse::Client::with_http_client(http))
}

/// Build a configured `clickhouse::Client` from the plugin config.
///
/// The DSN (embedding credentials) is unwrapped from its `SecretString` only
/// here at the connection boundary — it is never logged. `ClickHouse`
/// credentials and usage data are always sent in
/// cleartext when the URL scheme is `http://`; [`ClickHousePluginConfig::validate`]
/// (called by `Gear::init` before this function) already fails closed on a
/// plaintext `database_url` unless `allow_insecure_http` is explicitly set,
/// so a call reaching here with a `http://` URL has already had that
/// override deliberately enabled. This function still emits a
/// [`tracing::warn!`] in that case — defense-in-depth observability for any
/// call path that does not route through `validate()` first, and a durable
/// operator-visible signal every time an insecure connection is actually
/// made.
///
/// Timeout configuration is forwarded via `ClickHouse` session settings
/// `send_timeout` and `receive_timeout` (both bound to
/// `cfg.request_timeout_secs`).  The `clickhouse` 0.15.x `Client` is a
/// lightweight handle over an internal `hyper` connection pool.
///
/// Settings attached here ride on **every** request this client makes,
/// `SELECT`s included. Settings that must apply to writes only are attached
/// per-statement by `configure_insert` instead.
///
/// # Errors
///
/// Propagates [`new_base_client`]'s fail-closed error when no rustls
/// `CryptoProvider` has been installed process-wide, and [`parse_endpoint`]'s
/// error when `database_url` is not a valid absolute URL or its percent-encoded
/// credentials do not decode to valid UTF-8.
pub fn build_client(cfg: &ClickHousePluginConfig) -> anyhow::Result<clickhouse::Client> {
    // The config's `SecretString` is zeroized on drop, but that guarantee stops
    // at this boundary: `clickhouse` 0.15.1 stores the user and password as
    // plain `String`s on the `Client` for its whole lifetime
    // (`clickhouse-0.15.1/src/lib.rs:87-91`), so one unzeroized copy of the
    // credentials outlives this call regardless. Zeroize still shortens the
    // window for the config-side copy; do not read it as end-to-end scrubbing.
    let url = cfg.database_url.expose_secret();

    // TLS posture check — mirrors the reference plugin's sslmode-warn pattern.
    // Reaching this branch means `allow_insecure_http` was explicitly set
    // (validate() already rejected an unqualified `http://` database_url);
    // `https://` (the production default) requires no warning.
    if is_plaintext_url(url) {
        tracing::warn!(
            "connecting to `ClickHouse` with http:// scheme: credentials and usage \
             data are sent in cleartext. Use https:// for encrypted transport \
             in production."
        );
    }

    // On the production path this cannot fail: `Gear::init` always calls
    // `ClickHousePluginConfig::validate` before `build_client`, and `validate`
    // rejects both an unparseable `database_url` and one whose credentials do
    // not decode to UTF-8. For call sites that skip validation (direct
    // unit/integration use of `build_client`) an error is the honest result:
    // falling back to a client with `user: None` / `password: None` would be
    // another silent credential drop, which is exactly what this path guards
    // against.
    let endpoint = parse_endpoint(url)?;

    // `send_timeout` and `receive_timeout` are standard `ClickHouse` HTTP API
    // settings accepted as URL query parameters or per-request headers.
    // The `clickhouse` crate passes them as request headers on every query.
    // Both are set to `request_timeout_secs` — the crate exposes a single
    // combined timeout knob rather than separate send/receive splits.
    let timeout_str = cfg.request_timeout_secs.to_string();

    let mut client = new_base_client()?
        .with_url(endpoint.base_url)
        .with_setting("send_timeout", &timeout_str)
        .with_setting("receive_timeout", &timeout_str);

    if let Some(user) = endpoint.user {
        client = client.with_user(user);
    }
    if let Some(password) = endpoint.password {
        client = client.with_password(password.expose_secret());
    }
    if let Some(database) = endpoint.database {
        client = client.with_database(database);
    }

    // Cluster topology only: the `Distributed`-table settings every request
    // needs (see `Topology::client_settings`). Client-wide rather than per
    // call site, so no query path can be left without them.
    for (name, value) in Topology::from_config(cfg).client_settings() {
        client = client.with_setting(name, value);
    }

    Ok(client)
}

/// Apply this plugin's per-`INSERT` `ClickHouse` configuration to a freshly
/// acquired `Insert` handle.
///
/// The single place all three insert sites (`record_store`'s single-row and
/// batch writes, `catalog_store`'s type write) go through, so a change to
/// insert-time timeouts or settings cannot land on two of the three.
///
/// * **Timeouts** — `with_timeouts` bounds the subsequent `write` / `end`
///   awaits natively (yielding `ChError::TimedOut`, already classified
///   retryable), which the crate documents as far cheaper than wrapping each
///   of them in `tokio::time::timeout`.
/// * **`async_insert`** — when `async_insert` is `true`, `async_insert = 1`
///   plus `wait_for_async_insert = 1` plus `async_insert_deduplicate = 1`. Set
///   here, on the `Insert`'s own cloned client, rather than on the shared
///   `Client` in [`build_client`]: that client also serves every `SELECT` —
///   including the table-metadata read the crate issues inside
///   `Client::insert` when validation is on — and `async_insert` on a read is
///   at best noise. See [`crate::config::ClickHousePluginConfig::async_insert`]
///   for why `wait_for_async_insert` is pinned to `1` rather than exposed.
/// * **`dedup_token`** — when `Some`, sent as `insert_deduplication_token`, so
///   the engine drops the inserted block if a block with the same token landed
///   within the table's dedup window (`non_replicated_deduplication_window`,
///   see [`ensure_insert_dedup_window`]). Enforced on synchronous inserts. On
///   asynchronous inserts it is enforced only for `Replicated*` tables (which
///   is what `async_insert_deduplicate = 1` requests, and why it is set — it
///   is accepted and inert on the shipped non-replicated engine); the token
///   is still carried so the same call site is correct on either engine.
///
/// `with_setting` rather than the crate's `with_option`: the latter is
/// `#[deprecated(since = "0.14.3")]`, and the workspace lints deprecation.
///
/// # Panics
///
/// The `clickhouse` crate panics if a setting is changed after the request has
/// started, i.e. after the first `write`. Call this on the handle
/// `Client::insert` returned, before any `write` — which is what every call
/// site does.
///
/// No `#[must_use]` of its own: `clickhouse::insert::Insert` already carries
/// one, so dropping the return value is caught either way.
pub(crate) fn configure_insert<T>(
    insert: clickhouse::insert::Insert<T>,
    request_timeout: Duration,
    async_insert: bool,
    dedup_token: Option<&str>,
) -> clickhouse::insert::Insert<T> {
    let mut insert = insert.with_timeouts(Some(request_timeout), Some(request_timeout));
    if async_insert {
        insert = insert
            .with_setting("async_insert", "1")
            .with_setting("wait_for_async_insert", "1")
            .with_setting("async_insert_deduplicate", "1");
    }
    if let Some(token) = dedup_token {
        insert = insert.with_setting("insert_deduplication_token", token);
    }
    insert
}

/// Execute one migration DDL statement, bounded by `deadline`.
///
/// A hung `init` is worse than a failed one: it never surfaces as an error,
/// and the gear's readiness gauge is already published as 0 by then.
///
/// # Errors
///
/// Returns an error if the statement fails or exceeds `deadline`; the context
/// message includes the statement text.
pub(crate) async fn exec_statement(
    client: &clickhouse::Client,
    stmt: &str,
    deadline: Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(deadline, client.query(stmt).execute())
        .await
        .map_err(|_elapsed| {
            anyhow::anyhow!(
                "migration DDL statement exceeded the {}s client-side deadline:\n{stmt}",
                deadline.as_secs()
            )
        })?
        .with_context(|| format!("migration DDL statement failed:\n{stmt}"))
}

/// Parse retention seconds from a `CREATE TABLE` / `create_table_query` string.
///
/// Accepts both the literal form we emit (`INTERVAL <n> SECOND`) and
/// `ClickHouse`'s rewritten form (`toIntervalSecond(<n>)`). Returns `None` when
/// no recognisable TTL interval is present.
pub(crate) fn parse_ttl_seconds(create_table_query: &str) -> Option<u64> {
    // Prefer the rewritten form ClickHouse often stores in system.tables.
    if let Some(secs) = extract_u64_after(create_table_query, "toIntervalSecond(") {
        return Some(secs);
    }
    // Literal INTERVAL form from our DDL / ALTER.
    let upper = create_table_query.to_ascii_uppercase();
    let interval_idx = upper.find("INTERVAL")?;
    let after_interval = &create_table_query[interval_idx + "INTERVAL".len()..];
    let trimmed = after_interval.trim_start();
    let digits_end = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits_end == 0 {
        return None;
    }
    let secs: u64 = trimmed[..digits_end].parse().ok()?;
    let rest = trimmed[digits_end..].trim_start();
    if rest.to_ascii_uppercase().starts_with("SECOND") {
        Some(secs)
    } else {
        None
    }
}

fn extract_u64_after(haystack: &str, needle: &str) -> Option<u64> {
    let idx = haystack.find(needle)?;
    let after = &haystack[idx + needle.len()..];
    let digits_end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if digits_end == 0 {
        return None;
    }
    after[..digits_end].parse().ok()
}

/// True when the live TTL still casts `created_at` through 32-bit `toDateTime`.
///
/// Matching interval seconds alone is not enough to skip `MODIFY TTL`: a
/// table provisioned with the old wrapping clause would otherwise keep
/// saturating at 2106 until `retention_period_secs` changed.
fn ttl_uses_todatetime_cast(create_table_query: &str) -> bool {
    create_table_query.contains("toDateTime(created_at)")
}

/// Upper bound on the span of one `usage_records` partition, in seconds.
///
/// The table is `PARTITION BY toYYYYMM(created_at)`, so a partition covers one
/// calendar month — 31 days at most.
const PARTITION_SPAN_SECS: u64 = 31 * 86_400;

/// Retention below which the whole-partition TTL mode is worth warning about.
///
/// Two partition spans: at or above it the overshoot below is a small fraction
/// of the window, under it the overshoot is comparable to the window itself.
const SHORT_RETENTION_WARN_SECS: u64 = 2 * PARTITION_SPAN_SECS;

/// Whether whole-partition TTL expiry materially overshoots `retention_period_secs`.
///
/// `usage_records` is provisioned `SETTINGS ttl_only_drop_parts = 1`, so TTL
/// drops a partition once every row in it has expired rather than rewriting
/// parts to delete rows individually. A row therefore lives for
/// `retention_period_secs` **plus** up to the remaining span of its own
/// partition. Against the 1-year default that is a few percent; against a
/// one-week window it is several multiples, which an operator reading only
/// `retention_period_secs` would not expect.
pub(crate) fn retention_overshoots_partition(retention_period_secs: u64) -> bool {
    retention_period_secs < SHORT_RETENTION_WARN_SECS
}

/// Log [`retention_overshoots_partition`] once per `init`.
///
/// Advisory only: the DDL cannot adapt, because `PARTITION BY` is fixed at
/// `CREATE TABLE` while retention is configuration.
fn warn_on_short_retention(retention_period_secs: u64) {
    if retention_overshoots_partition(retention_period_secs) {
        tracing::warn!(
            retention_period_secs,
            partition_span_secs = PARTITION_SPAN_SECS,
            "configured retention is shorter than two usage_records partitions; expiry drops \
             whole monthly partitions (ttl_only_drop_parts = 1), so a row can outlive the \
             configured window by up to one partition span"
        );
    }
}

/// Reconcile `usage_records` TTL with the configured retention window.
///
/// Reads the live `create_table_query` from `system.tables`. When the table
/// has no TTL, the parsed interval seconds differ from
/// `retention_period_secs`, or the live clause still wraps `created_at` in
/// `toDateTime`, issues
/// `ALTER TABLE usage_records MODIFY TTL created_at + INTERVAL <n> SECOND DELETE`.
///
/// On a cluster (only reached when the plugin manages the cluster schema) the
/// TTL lives on the shard-local table, so both the read and the `ALTER` target
/// it, the latter `ON CLUSTER` ([`Topology::alter_target`]).
///
/// Both statements are bounded by `deadline`, for the same reason as
/// [`exec_statement`].
///
/// # Errors
///
/// Returns an error if the table is missing, either statement fails, or either
/// exceeds `deadline`.
pub async fn ensure_retention_ttl(
    client: &clickhouse::Client,
    topology: &Topology,
    retention_period_secs: u64,
    deadline: Duration,
) -> anyhow::Result<()> {
    let (table, on_cluster) = topology.alter_target(RECORDS_TABLE);
    let create_sql = read_create_sql(client, table, deadline).await?;

    warn_on_short_retention(retention_period_secs);

    let current = parse_ttl_seconds(&create_sql);
    if current == Some(retention_period_secs) && !ttl_uses_todatetime_cast(&create_sql) {
        tracing::debug!(
            retention_period_secs,
            "usage_records TTL already matches configured retention"
        );
        return Ok(());
    }

    let alter = format!(
        "ALTER TABLE {table}{on_cluster} MODIFY TTL {}",
        table::ttl_expr("created_at", retention_period_secs)
    );
    tracing::info!(
        previous = ?current,
        retention_period_secs,
        "updating usage_records TTL to match configured retention"
    );
    tokio::time::timeout(deadline, client.query(&alter).execute())
        .await
        .map_err(|_elapsed| {
            anyhow::anyhow!(
                "retention TTL alter exceeded the {}s client-side deadline:\n{alter}",
                deadline.as_secs()
            )
        })?
        .with_context(|| format!("failed to apply retention TTL:\n{alter}"))?;

    Ok(())
}

/// Number of recent inserted blocks `usage_records` deduplicates
/// `insert_deduplication_token`s against (`non_replicated_deduplication_window`).
///
/// Each synchronous single-record create is one block; each batch or marker
/// statement is one block per `toYYYYMM` partition it touches. The window only
/// has to outlast the race it guards — two retries of one write milliseconds
/// apart — so 10 000 blocks is ample headroom rather than a retention horizon.
/// `migrator::m0001_init` declares it on `usage_records` (single node only).
pub(crate) const INSERT_DEDUP_WINDOW_BLOCKS: u64 = 10_000;

/// Read the live `create_table_query` of `table` (`usage_records` or its
/// shard-local table) from `system.tables`.
///
/// Shared by [`ensure_retention_ttl`] and [`ensure_insert_dedup_window`], both
/// of which reconcile a table-level setting by parsing this text.
async fn read_create_sql(
    client: &clickhouse::Client,
    table: &str,
    deadline: Duration,
) -> anyhow::Result<String> {
    tokio::time::timeout(
        deadline,
        client
            .query(
                "SELECT create_table_query \
                 FROM system.tables \
                 WHERE database = currentDatabase() AND name = ?",
            )
            .bind(table)
            .fetch_one::<String>(),
    )
    .await
    .map_err(|_elapsed| {
        anyhow::anyhow!(
            "reading {table} create_table_query exceeded the {}s client-side deadline",
            deadline.as_secs()
        )
    })?
    .with_context(|| format!("failed to read {table} create_table_query from system.tables"))
}

/// Parse `non_replicated_deduplication_window` from a `CREATE TABLE` /
/// `create_table_query` string; `None` when the setting is absent.
pub(crate) fn parse_dedup_window(create_table_query: &str) -> Option<u64> {
    extract_u64_after(create_table_query, "non_replicated_deduplication_window = ")
}

/// Reconcile the `usage_records` insert dedup window with
/// [`INSERT_DEDUP_WINDOW_BLOCKS`].
///
/// The migration's `CREATE TABLE IF NOT EXISTS` carries the setting for fresh
/// deployments but cannot retrofit a table created before it did, so this
/// reads the live `create_table_query` and issues
/// `ALTER TABLE usage_records MODIFY SETTING non_replicated_deduplication_window = <n>`
/// when the live value differs. `MODIFY SETTING` is metadata-only and safe to
/// repeat; the read keeps startup silent when nothing changes — the same
/// posture as [`ensure_retention_ttl`].
///
/// On a table an operator provisioned as `Replicated*`, the setting is
/// accepted but inert (`replicated_deduplication_window` governs instead), so
/// this is harmless there.
///
/// # Errors
///
/// Returns an error if the table is missing, either statement fails, or either
/// exceeds `deadline`.
pub async fn ensure_insert_dedup_window(
    client: &clickhouse::Client,
    deadline: Duration,
) -> anyhow::Result<()> {
    let create_sql = read_create_sql(client, RECORDS_TABLE, deadline).await?;
    let current = parse_dedup_window(&create_sql);
    if current == Some(INSERT_DEDUP_WINDOW_BLOCKS) {
        tracing::debug!(
            window_blocks = INSERT_DEDUP_WINDOW_BLOCKS,
            "usage_records insert dedup window already set"
        );
        return Ok(());
    }

    let alter = format!(
        "ALTER TABLE usage_records MODIFY SETTING \
         non_replicated_deduplication_window = {INSERT_DEDUP_WINDOW_BLOCKS}"
    );
    tracing::info!(
        previous = ?current,
        window_blocks = INSERT_DEDUP_WINDOW_BLOCKS,
        "setting usage_records insert dedup window"
    );
    tokio::time::timeout(deadline, client.query(&alter).execute())
        .await
        .map_err(|_elapsed| {
            anyhow::anyhow!(
                "insert dedup window alter exceeded the {}s client-side deadline:\n{alter}",
                deadline.as_secs()
            )
        })?
        .with_context(|| format!("failed to apply insert dedup window:\n{alter}"))?;

    Ok(())
}

// ── Cluster topology ──────────────────────────────────────────────────────────

/// The `system.tables` columns [`check_cluster_schema`] inspects.
#[derive(Debug, Clone, clickhouse::Row, serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub(crate) struct SystemTableRow {
    pub name: String,
    pub engine: String,
    pub engine_full: String,
    pub sorting_key: String,
    pub partition_key: String,
    pub create_table_query: String,
}

/// Drop all whitespace so `ClickHouse`'s formatting of a key expression
/// (`gts_id, tenant_id`) compares equal to the canonical form (`gts_id,tenant_id`).
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Top-level arguments of the first `<engine>(...)` call in `engine_full`,
/// whitespace-squashed and with surrounding quotes removed; `None` when the
/// text holds no well-formed call.
pub(crate) fn engine_args(engine_full: &str, engine: &str) -> Option<Vec<String>> {
    let head = format!("{engine}(");
    let body = &engine_full[engine_full.find(&head)? + head.len()..];
    let mut args = Vec::new();
    let mut current = String::new();
    let mut depth = 0_u32;
    let mut in_string = false;
    for c in body.chars() {
        match c {
            '\'' => {
                in_string = !in_string;
                current.push(c);
            }
            '(' if !in_string => {
                depth += 1;
                current.push(c);
            }
            ')' if !in_string && depth == 0 => {
                args.push(current);
                return Some(
                    args.iter()
                        .map(|a| squash(a).trim_matches(['\'', '`', '"']).to_owned())
                        .collect(),
                );
            }
            ')' if !in_string => {
                depth -= 1;
                current.push(c);
            }
            ',' if !in_string && depth == 0 => args.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    None
}

/// Whether a `ReplacingMergeTree`-family `engine_full` names exactly the
/// plugin's version column: `(version)` or, with Keeper arguments (which
/// `ClickHouse` stores even when defaulted), `('path', 'replica', version)`.
/// A missing version, another column, or an extra `is_deleted` argument fails.
fn has_version_column(engine_full: &str, engine: &str) -> bool {
    engine_args(engine_full, engine).is_some_and(
        |args| matches!(args.as_slice(), [v] | [_, _, v] if v == m0001_init::VERSION_COLUMN),
    )
}

/// Check the operator-provisioned cluster schema seen on the connected node.
///
/// `rows` are this database's `system.tables` rows for the four tables the
/// topology names; `database` is the connected database. Returns one
/// human-readable problem per deviation — empty when the schema is usable.
/// Pure so the rules are unit-testable without a cluster.
pub(crate) fn check_cluster_schema(
    rows: &[SystemTableRow],
    topology: &Topology,
    database: &str,
) -> Vec<String> {
    let Topology::Cluster {
        name: cluster,
        records_local,
        catalog_local,
        ..
    } = topology
    else {
        return Vec::new();
    };
    let find = |table: &str| rows.iter().find(|r| r.name == table);
    let mut problems = Vec::new();

    let local_engine = table::cluster_local_engine(None).name();
    let records_key = m0001_init::RECORDS_ORDER_BY.join(",");
    let catalog_key = m0001_init::CATALOG_ORDER_BY.join(",");
    for (local, sorting_key, partition_key) in [
        (
            records_local.as_str(),
            records_key.as_str(),
            m0001_init::RECORDS_PARTITION_KEY,
        ),
        (catalog_local.as_str(), catalog_key.as_str(), ""),
    ] {
        let Some(row) = find(local) else {
            problems.push(format!("local table `{local}` does not exist"));
            continue;
        };
        if row.engine != local_engine {
            problems.push(format!(
                "local table `{local}` uses engine {}, expected {local_engine}",
                row.engine
            ));
        } else if !has_version_column(&row.engine_full, local_engine) {
            problems.push(format!(
                "local table `{local}` is `{}`, expected {local_engine}(..., {version}): \
                 without the version column merges keep the last insert instead of the \
                 highest version",
                row.engine_full,
                version = m0001_init::VERSION_COLUMN,
            ));
        }
        if squash(&row.sorting_key) != sorting_key {
            problems.push(format!(
                "local table `{local}` is ORDER BY ({}), expected ({sorting_key})",
                row.sorting_key
            ));
        }
        if squash(&row.partition_key) != partition_key {
            problems.push(format!(
                "local table `{local}` is PARTITION BY `{}`, expected `{partition_key}`",
                row.partition_key
            ));
        }
    }

    for (logical, local, sharding_key) in [
        (RECORDS_TABLE, records_local.as_str(), RECORDS_SHARDING_KEY),
        (CATALOG_TABLE, catalog_local.as_str(), CATALOG_SHARDING_KEY),
    ] {
        let Some(row) = find(logical) else {
            problems.push(format!("Distributed table `{logical}` does not exist"));
            continue;
        };
        if row.engine != "Distributed" {
            problems.push(format!(
                "`{logical}` uses engine {}, expected Distributed over `{local}`",
                row.engine
            ));
            continue;
        }
        let ok = engine_args(&row.engine_full, "Distributed").is_some_and(|args| {
            matches!(
                args.as_slice(),
                [c, db, l, key, ..]
                    if c == cluster
                        && (db == database || db == "currentDatabase()")
                        && l == local
                        && key == sharding_key
            )
        });
        if !ok {
            problems.push(format!(
                "`{logical}` is `{}`, expected Distributed('{cluster}', '{database}', \
                 '{local}', {sharding_key})",
                row.engine_full
            ));
        }
    }
    problems
}

/// Await one startup probe under `deadline`, naming it in both the timeout
/// and the failure error.
pub(crate) async fn timed<T>(
    deadline: Duration,
    what: &str,
    fut: impl Future<Output = clickhouse::error::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(deadline, fut)
        .await
        .map_err(|_elapsed| {
            anyhow::anyhow!(
                "{what} exceeded the {}s client-side deadline",
                deadline.as_secs()
            )
        })?
        .with_context(|| format!("{what} failed"))
}

/// Validate the cluster schema at startup.
///
/// The schema comes from the operator (who runs `migrator::render_ddl`,
/// which a failure here prints) or, with `cluster.manage_schema`, from the cluster
/// migrations that run just before this; either way this never creates or
/// alters anything. It fails init when:
///
/// * the cluster is not declared in `system.clusters`;
/// * a table is missing or has the wrong engine, sort key, partition key, or
///   `Distributed` target / sharding key, or a local table's engine lacks the
///   `version` column ([`check_cluster_schema`]);
/// * any replica of the cluster lacks one of the four tables — the plugin
///   may be pointed at any node, and an `ON CLUSTER` mutation needs the local
///   tables everywhere. An unreachable replica fails this probe too, which is
///   the right default for a deployment check.
///
/// A `usage_records` TTL that differs from `retention_period_secs` is only
/// warned about: reconciling it would be DDL (the managed path reconciles it
/// with `ensure_retention_ttl` first).
///
/// # Errors
///
/// Returns an error listing every problem found, or when a probe fails or
/// exceeds `deadline`.
pub async fn validate_cluster_schema(
    client: &clickhouse::Client,
    topology: &Topology,
    retention_period_secs: u64,
    deadline: Duration,
) -> anyhow::Result<()> {
    let Topology::Cluster {
        name: cluster,
        records_local,
        catalog_local,
        ..
    } = topology
    else {
        return Ok(());
    };

    let database: String = timed(
        deadline,
        "reading currentDatabase()",
        client.query("SELECT currentDatabase()").fetch_one(),
    )
    .await?;

    let replicas: u64 = timed(
        deadline,
        "reading system.clusters",
        client
            .query("SELECT count() FROM system.clusters WHERE cluster = ?")
            .bind(cluster)
            .fetch_one(),
    )
    .await?;
    if replicas == 0 {
        anyhow::bail!(
            "cluster `{cluster}` is not declared in system.clusters on the connected node; \
             check cluster.name against the server's remote_servers config"
        );
    }

    let names = [
        RECORDS_TABLE,
        CATALOG_TABLE,
        records_local.as_str(),
        catalog_local.as_str(),
    ];
    let rows: Vec<SystemTableRow> = timed(
        deadline,
        "reading system.tables",
        client
            .query(
                "SELECT name, engine, engine_full, sorting_key, partition_key, create_table_query \
                 FROM system.tables WHERE database = ? AND name IN (?, ?, ?, ?)",
            )
            .bind(&database)
            .bind(names[0])
            .bind(names[1])
            .bind(names[2])
            .bind(names[3])
            .fetch_all(),
    )
    .await?;

    let mut problems = check_cluster_schema(&rows, topology, &database);

    // `cluster` is a validated plain identifier (`ClickHouseClusterConfig::validate`),
    // so interpolating it into the table-function argument is safe.
    let present: u64 = timed(
        deadline,
        "reading system.tables across the cluster",
        client
            .query(&format!(
                "SELECT count() FROM clusterAllReplicas('{cluster}', system.tables) \
                 WHERE database = ? AND name IN (?, ?, ?, ?)"
            ))
            .bind(&database)
            .bind(names[0])
            .bind(names[1])
            .bind(names[2])
            .bind(names[3])
            .fetch_one(),
    )
    .await?;
    // Four tables (`names`) per replica.
    let expected = replicas.saturating_mul(4);
    if present != expected {
        problems.push(format!(
            "only {present} of {expected} expected tables exist across the {replicas} replicas \
             of cluster `{cluster}`; create them with ON CLUSTER"
        ));
    }

    if !problems.is_empty() {
        let ddl = match migrator::render_ddl(topology, retention_period_secs).await {
            Ok(stmts) => format!("{};", stmts.join(";\n\n")),
            Err(e) => format!("(the DDL could not be rendered: {e:#})"),
        };
        anyhow::bail!(
            "the cluster schema in database `{database}` is not usable (set \
             cluster.manage_schema, or run the DDL below against database `{database}`):\n- {}\n\n{ddl}",
            problems.join("\n- ")
        );
    }

    warn_on_short_retention(retention_period_secs);
    let live_ttl = rows
        .iter()
        .find(|r| &r.name == records_local)
        .and_then(|r| parse_ttl_seconds(&r.create_table_query));
    if live_ttl != Some(retention_period_secs) {
        tracing::warn!(
            live_ttl_secs = ?live_ttl,
            retention_period_secs,
            table = %records_local,
            "cluster usage_records TTL does not match retention_period_secs; the plugin does \
             not alter operator-provisioned tables, so run ALTER TABLE ... ON CLUSTER ... MODIFY TTL \
             or set cluster.manage_schema"
        );
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "pool_tests.rs"]
mod pool_tests;
