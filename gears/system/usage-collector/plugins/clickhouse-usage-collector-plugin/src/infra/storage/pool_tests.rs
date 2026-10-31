use std::time::Duration;

use secrecy::ExposeSecret;

use super::{DEFAULT_RETENTION_SECS, parse_endpoint, parse_ttl_seconds};
use crate::config::is_plaintext_url;
use crate::infra::storage::migrator::render_ddl;
use crate::infra::storage::topology::Topology;

/// Install a rustls `CryptoProvider` for this test process.
///
/// `build_client` fails closed when none is installed (see
/// `pool.rs::new_base_client`). Production installs one in
/// `toolkit::bootstrap::init_procedure` before any `Gear::init` runs; unit
/// tests do not go through bootstrap, so they install one here. Which
/// provider it is does not matter to these tests — none of them opens a
/// socket, they only need the lookup in `new_base_client` to find something.
///
/// Idempotent and race-safe: `install_default` is backed by a process-wide
/// `OnceLock`, so a second call — from another test, possibly on another
/// thread — returns `Err` and is deliberately dropped. `drop` rather than
/// `let _ =` satisfies `clippy::let_underscore_must_use`.
fn install_test_crypto_provider() {
    drop(rustls::crypto::aws_lc_rs::default_provider().install_default());
}

// ---------------------------------------------------------------------------
// database_url -> (base endpoint, user, password, database) splitting
//
// Regression coverage for the bug where `database_url`'s embedded
// user/password/path were passed straight through to
// `clickhouse::Client::with_url`, which sends every request to that literal
// path (ClickHouse's HTTP interface only implements `/`, `/ping`, etc. --
// see `pool.rs::build_client` doc comment) while silently dropping the
// credentials.
// ---------------------------------------------------------------------------

#[test]
fn parse_endpoint_splits_user_password_and_database() {
    let endpoint = parse_endpoint("http://chuser:s3cret@ch:8123/usage").unwrap();
    assert_eq!(endpoint.base_url, "http://ch:8123/");
    assert_eq!(endpoint.user.as_deref(), Some("chuser"));
    assert_eq!(
        endpoint.password.as_ref().map(ExposeSecret::expose_secret),
        Some("s3cret")
    );
    assert_eq!(endpoint.database.as_deref(), Some("usage"));
}

#[test]
fn parse_endpoint_base_url_has_no_leftover_path_or_userinfo() {
    // The exact bug this guards: a leftover path segment makes every request
    // 404 against ClickHouse's HTTP interface, and leftover userinfo would
    // never be honored by `Client::with_url` (see struct-level doc comment).
    let endpoint = parse_endpoint("http://user:pass@localhost:8123/mydb").unwrap();
    assert!(
        !endpoint.base_url.contains("mydb"),
        "base_url must not retain the database path: {}",
        endpoint.base_url
    );
    assert!(
        !endpoint.base_url.contains("user") && !endpoint.base_url.contains("pass"),
        "base_url must not retain userinfo: {}",
        endpoint.base_url
    );
}

#[test]
fn parse_endpoint_with_no_userinfo_or_path_yields_none() {
    let endpoint = parse_endpoint("http://localhost:8123").unwrap();
    assert_eq!(endpoint.base_url, "http://localhost:8123/");
    assert_eq!(endpoint.user, None);
    assert!(endpoint.password.is_none());
    assert_eq!(endpoint.database, None);
}

#[test]
fn parse_endpoint_trims_leading_and_trailing_slashes_from_database() {
    let endpoint = parse_endpoint("http://localhost:8123/usage/").unwrap();
    assert_eq!(endpoint.database.as_deref(), Some("usage"));
}

#[test]
fn parse_endpoint_user_without_password_has_no_password() {
    let endpoint = parse_endpoint("http://chuser@localhost:8123/").unwrap();
    assert_eq!(endpoint.user.as_deref(), Some("chuser"));
    assert!(endpoint.password.is_none());
}

#[test]
fn parse_endpoint_percent_decodes_userinfo_with_reserved_chars() {
    // `@` and `/` must be percent-encoded in the URL; after parse they must
    // be restored to literal credentials for `with_user` / `with_password`.
    let endpoint = parse_endpoint("http://u%3Aser:p%40ss%2Fword@ch:8123/usage").unwrap();
    assert_eq!(endpoint.user.as_deref(), Some("u:ser"));
    assert_eq!(
        endpoint.password.as_ref().map(ExposeSecret::expose_secret),
        Some("p@ss/word")
    );
    assert_eq!(endpoint.database.as_deref(), Some("usage"));
}

#[test]
fn parse_endpoint_decodes_valid_non_ascii_utf8_userinfo() {
    // `%C3%A9` is a well-formed multibyte sequence (U+00E9, `e` with acute);
    // strict decoding must still accept it — only *invalid* UTF-8 is rejected.
    let endpoint = parse_endpoint("http://us%C3%A9r:p%C3%A9ss@ch:8123/usage").unwrap();
    assert_eq!(endpoint.user.as_deref(), Some("us\u{e9}r"));
    assert_eq!(
        endpoint.password.as_ref().map(ExposeSecret::expose_secret),
        Some("p\u{e9}ss")
    );
}

#[test]
fn parse_endpoint_rejects_invalid_utf8_in_password() {
    // `%FF` is never valid UTF-8. A lossy decode would turn this into a
    // *different* password (U+FFFD) and the gear would start, then loop on
    // auth failures; it must fail here instead, without echoing the secret.
    let Err(err) = parse_endpoint("http://chuser:p%FF@ch:8123/usage") else {
        panic!("invalid UTF-8 in the password must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("password") && msg.contains("UTF-8"),
        "unexpected error: {msg}"
    );
    assert!(
        !msg.contains("p%FF"),
        "error must not leak the credential: {msg}"
    );
}

#[test]
fn parse_endpoint_rejects_invalid_utf8_in_username() {
    let Err(err) = parse_endpoint("http://u%FF:s3cret@ch:8123/usage") else {
        panic!("invalid UTF-8 in the username must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("username") && msg.contains("UTF-8"),
        "unexpected error: {msg}"
    );
    assert!(
        !msg.contains("u%FF") && !msg.contains("s3cret"),
        "error must not leak the credentials: {msg}"
    );
}

#[test]
fn parse_endpoint_rejects_malformed_url() {
    assert!(parse_endpoint("not a url").is_err());
}

// ---------------------------------------------------------------------------
// TLS posture detection
// ---------------------------------------------------------------------------

#[test]
fn plaintext_url_detected_for_http_scheme() {
    assert!(is_plaintext_url("http://user:pass@ch:8123/db"));
    assert!(is_plaintext_url("http://ch:8123/usage"));
}

/// The scheme is matched on the parsed URL, whose scheme the `url` crate
/// lowercases, so case variants of the same cleartext connection are all caught.
#[test]
fn plaintext_url_detected_regardless_of_scheme_case() {
    assert!(is_plaintext_url("HTTP://user:pass@ch:8123/db"));
    assert!(is_plaintext_url("Http://ch:8123/usage"));
}

#[test]
fn tls_url_not_flagged_for_https_scheme() {
    assert!(!is_plaintext_url("https://user:pass@ch:8443/db"));
    assert!(!is_plaintext_url("https://ch:8443/usage"));
    assert!(!is_plaintext_url("HTTPS://ch:8443/usage"));
}

#[test]
fn tls_url_not_flagged_for_non_http_schemes() {
    assert!(!is_plaintext_url("clickhouse://ch:9000/db"));
    assert!(!is_plaintext_url("tcp://ch:9000"));
}

/// A DSN that does not parse cannot be shown to be encrypted, so it counts as
/// plaintext. `validate` rejects it before `build_client`, so this only decides
/// whether the defense-in-depth cleartext warning fires on a path that skipped
/// validation — and a spurious warning is the safe direction there.
#[test]
fn unparseable_url_treated_as_plaintext() {
    assert!(is_plaintext_url("not-a-url"));
    assert!(is_plaintext_url(""));
}

// ---------------------------------------------------------------------------
// TTL parsing and default retention in migration SQL
// ---------------------------------------------------------------------------

#[test]
fn parse_ttl_seconds_from_interval_form() {
    let sql = "CREATE TABLE usage_records (...) TTL created_at + INTERVAL 86400 SECOND DELETE";
    assert_eq!(parse_ttl_seconds(sql), Some(86_400));
}

#[test]
fn parse_ttl_seconds_from_legacy_todatetime_interval_form() {
    let sql = "CREATE TABLE usage_records (...) TTL toDateTime(created_at) + INTERVAL 86400 SECOND DELETE";
    assert_eq!(parse_ttl_seconds(sql), Some(86_400));
}

#[test]
fn parse_ttl_seconds_from_to_interval_second_form() {
    let sql = "CREATE TABLE usage_records (...) TTL created_at + toIntervalSecond(31536000)";
    assert_eq!(parse_ttl_seconds(sql), Some(31_536_000));
}

#[test]
fn parse_ttl_seconds_from_legacy_todatetime_to_interval_second_form() {
    let sql =
        "CREATE TABLE usage_records (...) TTL toDateTime(created_at) + toIntervalSecond(31536000)";
    assert_eq!(parse_ttl_seconds(sql), Some(31_536_000));
}

#[test]
fn parse_ttl_seconds_returns_none_when_missing() {
    let sql = "CREATE TABLE usage_records (...) ENGINE = ReplacingMergeTree(version) ORDER BY (id)";
    assert_eq!(parse_ttl_seconds(sql), None);
}

#[test]
fn build_client_accepts_https_url_with_auth_and_database() {
    use super::build_client;
    use secrecy::SecretString;

    use crate::config::ClickHousePluginConfig;

    let cfg = ClickHousePluginConfig {
        database_url: SecretString::from("https://chuser:secret@clickhouse.example:8443/usage_db"),
        request_timeout_secs: 15,
        ..ClickHousePluginConfig::default()
    };
    cfg.validate().expect("https config is valid");
    install_test_crypto_provider();
    // Construction only — no network I/O.
    let _client = build_client(&cfg).expect("client builds once a provider is installed");
}

#[test]
fn build_client_accepts_plaintext_http_when_override_set() {
    use super::build_client;
    use secrecy::SecretString;

    use crate::config::ClickHousePluginConfig;

    let cfg = ClickHousePluginConfig {
        database_url: SecretString::from("http://default:@localhost:8123/default"),
        allow_insecure_http: true,
        request_timeout_secs: 7,
        ..ClickHousePluginConfig::default()
    };
    cfg.validate().expect("override permits http");
    install_test_crypto_provider();
    let _client = build_client(&cfg).expect("client builds once a provider is installed");
}

/// An unparseable `database_url` is a hard error from `build_client`, not an
/// inert client: a fallback with no credentials would be a silent credential
/// drop. `validate()` rejects such a URL on the production path; this covers
/// the call sites that skip it.
#[test]
fn build_client_rejects_unparseable_url() {
    use super::build_client;
    use secrecy::SecretString;

    use crate::config::ClickHousePluginConfig;

    let cfg = ClickHousePluginConfig {
        database_url: SecretString::from("not a url"),
        allow_insecure_http: true,
        ..ClickHousePluginConfig::default()
    };
    cfg.validate()
        .expect_err("an unparseable database_url must not pass validation");

    install_test_crypto_provider();
    let Err(err) = build_client(&cfg) else {
        panic!("an unparseable URL must not build a client");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("valid absolute URL"),
        "unexpected error: {msg}"
    );
}

/// A percent-encoded credential that decodes to invalid UTF-8 must fail
/// client construction rather than be lossily rewritten into a different
/// secret that would then loop on authentication failures. The error names
/// the component but never the credential.
#[test]
fn build_client_rejects_invalid_utf8_userinfo() {
    use super::build_client;
    use secrecy::SecretString;

    use crate::config::ClickHousePluginConfig;

    let cfg = ClickHousePluginConfig {
        database_url: SecretString::from("https://chuser:p%FF@clickhouse.example:8443/usage_db"),
        request_timeout_secs: 15,
        ..ClickHousePluginConfig::default()
    };
    cfg.validate()
        .expect_err("invalid-UTF-8 credentials must not pass validation");

    install_test_crypto_provider();
    let Err(err) = build_client(&cfg) else {
        panic!("invalid-UTF-8 credentials must not build a client");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("password") && msg.contains("UTF-8"),
        "unexpected error: {msg}"
    );
    assert!(
        !msg.contains("p%FF"),
        "error must not leak the credential: {msg}"
    );
}

// Live-ClickHouse integration tests are gated behind the `clickhouse` cargo feature.
// ── retention_overshoots_partition ───────────────────────────────────────────

/// The warning fires only where the overshoot is comparable to the window.
///
/// With `ttl_only_drop_parts = 1` a row outlives its window by up to one
/// monthly partition span, so the threshold is two spans: above it the
/// overshoot is a fraction of the window, below it a multiple of it.
#[test]
fn retention_overshoot_warns_only_for_short_windows() {
    use super::retention_overshoots_partition;

    assert!(
        retention_overshoots_partition(7 * 86_400),
        "a one-week window can be overshot several times over"
    );
    assert!(
        retention_overshoots_partition(2 * 31 * 86_400 - 1),
        "just under two partition spans still warns"
    );
    assert!(
        !retention_overshoots_partition(2 * 31 * 86_400),
        "two partition spans is the threshold, not past it"
    );
    assert!(
        !retention_overshoots_partition(DEFAULT_RETENTION_SECS),
        "the 1-year default overshoots by a few percent and must stay quiet"
    );
}

// ── configure_insert ──────────────────────────────────────────────────────────
//
// The `clickhouse` crate panics if a setting is changed after the request has
// started (i.e. after the first `write`), so the assertion in both tests below
// is that the helper returns at all. It pins the ordering contract
// `configure_insert`'s `# Panics` section documents; a future refactor that
// moved the call after a `write` would abort the test rather than fail it.
//
// `with_validation(false)` matters: validation is on by default, and it makes
// `Client::insert` issue a table-metadata round-trip that would hang or fail
// against the unreachable port.

/// Build an offline `Insert` handle. Port 1 is reserved and never bound, so no
/// request can actually be issued.
#[cfg(test)]
async fn offline_insert()
-> clickhouse::insert::Insert<crate::infra::storage::entity::UsageRecordRow> {
    clickhouse::Client::default()
        .with_url("http://127.0.0.1:1")
        .with_validation(false)
        .insert("usage_records")
        .await
        .expect("acquiring an Insert handle opens no socket")
}

#[tokio::test]
async fn configure_insert_applies_settings_before_the_first_write() {
    let insert = super::configure_insert(
        offline_insert().await,
        std::time::Duration::from_secs(5),
        true,
        Some("token"),
    );
    drop(insert);
}

/// The disabled arm must still yield a usable handle — it is the escape hatch
/// for a deployment whose `ClickHouse` cannot do async inserts.
#[tokio::test]
async fn configure_insert_with_async_insert_disabled_applies_only_timeouts() {
    let insert = super::configure_insert(
        offline_insert().await,
        std::time::Duration::from_secs(5),
        false,
        None,
    );
    drop(insert);
}

// ── insert dedup window ───────────────────────────────────────────────────────

/// The live `create_table_query` interleaves server defaults into `SETTINGS`;
/// the parser must find the window wherever it sits in that list.
#[test]
fn parse_dedup_window_from_live_engine_full() {
    let live = "CREATE TABLE default.usage_records (...) ENGINE = ReplacingMergeTree(version) \
                PARTITION BY toYYYYMM(created_at) ORDER BY (gts_id, tenant_id, created_at, id) \
                TTL created_at + toIntervalSecond(31536000) \
                SETTINGS ttl_only_drop_parts = 1, non_replicated_deduplication_window = 10000, \
                index_granularity = 8192";
    assert_eq!(super::parse_dedup_window(live), Some(10_000));
}

/// A table provisioned before the setting existed reads back without it — the
/// case `ensure_insert_dedup_window` retrofits.
#[test]
fn parse_dedup_window_returns_none_when_missing() {
    let live = "CREATE TABLE default.usage_records (...) ENGINE = ReplacingMergeTree(version) \
                SETTINGS ttl_only_drop_parts = 1, index_granularity = 8192";
    assert_eq!(super::parse_dedup_window(live), None);
}

// ── parse_ttl_seconds / parse_dedup_window: rejection paths ──────────────────
//
// `ensure_retention_ttl` compares the parsed value against the configured
// retention and issues `MODIFY TTL` when they differ. `None` is the fail-safe
// answer for anything unrecognised — it re-applies the configured TTL rather
// than skipping reconciliation on a value that was misread.

/// `INTERVAL` with no digit run after it is not a TTL this parser understands.
#[test]
fn parse_ttl_seconds_returns_none_when_interval_has_no_digits() {
    assert_eq!(
        parse_ttl_seconds("TTL created_at + INTERVAL SECOND DELETE"),
        None
    );
}

/// A TTL in any unit other than seconds cannot be compared against
/// `retention_period_secs` without a conversion this parser deliberately does
/// not do — so it reads as absent and the reconciliation rewrites it in
/// seconds.
#[test]
fn parse_ttl_seconds_returns_none_for_a_non_second_unit() {
    assert_eq!(
        parse_ttl_seconds("TTL created_at + INTERVAL 30 DAY DELETE"),
        None,
        "only INTERVAL <n> SECOND is recognised"
    );
}

/// A digit run too large for `u64` must read as absent rather than panic or
/// silently truncate.
#[test]
fn parse_ttl_seconds_returns_none_when_the_interval_overflows_u64() {
    assert_eq!(
        parse_ttl_seconds("TTL created_at + INTERVAL 99999999999999999999999 SECOND DELETE"),
        None
    );
}

/// A non-numeric `toIntervalSecond(...)` argument reads as absent, not as a
/// parse that silently picks up a different number.
///
/// Pins a quirk worth knowing about: the literal-form fallback searches for the
/// substring `INTERVAL`, and `toIntervalSecond` uppercased *contains* it, so the
/// fallback re-matches inside the very token that just failed and finds `Second(`
/// where it wants digits. The result is `None` either way, which is the fail-safe
/// answer — `ensure_retention_ttl` treats it as "differs" and re-applies the
/// configured TTL — so this costs a redundant `MODIFY TTL`, never a wrong one.
/// `ClickHouse` always emits a numeric argument in practice, so the path is not
/// reachable from a live `create_table_query`.
#[test]
fn parse_ttl_seconds_returns_none_when_tointervalsecond_has_no_digits() {
    assert_eq!(
        parse_ttl_seconds("TTL created_at + toIntervalSecond(x)"),
        None
    );
}

/// The dedup-window parser shares `extract_u64_after`, so it has the same
/// no-digits guard: a malformed setting reads as absent and
/// `ensure_insert_dedup_window` re-applies the value.
#[test]
fn parse_dedup_window_returns_none_when_the_setting_has_no_digits() {
    assert_eq!(
        super::parse_dedup_window(
            "CREATE TABLE t (...) SETTINGS non_replicated_deduplication_window = abc"
        ),
        None
    );
}

// ── ttl_uses_todatetime_cast ─────────────────────────────────────────────────
//
// Matching interval seconds alone is not enough to skip `MODIFY TTL`: a table
// provisioned with the old `toDateTime(created_at)` wrapping clause saturates
// at 2106 and must be rewritten even when its interval already matches.

/// The legacy clause is detected, so `ensure_retention_ttl` rewrites it even on
/// a matching interval.
#[test]
fn ttl_using_the_legacy_todatetime_cast_is_detected() {
    assert!(super::ttl_uses_todatetime_cast(
        "TTL toDateTime(created_at) + toIntervalSecond(31536000)"
    ));
}

/// The current clause reads `created_at` directly; a table already on it must
/// not be rewritten on every init.
#[test]
fn ttl_on_the_bare_created_at_column_is_not_flagged_as_legacy() {
    assert!(!super::ttl_uses_todatetime_cast(
        "TTL created_at + toIntervalSecond(31536000)"
    ));
}

// ── warn_on_short_retention ──────────────────────────────────────────────────

/// Advisory only — it logs or it does not, and neither outcome changes the DDL.
/// The assertion is that both branches run without panicking, which pins the
/// `tracing::warn!` field list against a future edit that names a field the
/// macro cannot format.
#[test]
fn warn_on_short_retention_runs_on_both_sides_of_the_threshold() {
    super::warn_on_short_retention(7 * 86_400);
    super::warn_on_short_retention(DEFAULT_RETENTION_SECS);
}

// ── Startup DDL against an unreachable backend ───────────────────────────────
//
// `Gear::init` runs all three of these before the plugin reports ready, so each
// must fail loudly rather than return `Ok` when the backend cannot be reached.
// A silent success here would publish a plugin whose tables do not exist.

/// Port 1 is reserved and never bound, so a request fails fast with connection
/// refused rather than blocking.
const OFFLINE_URL: &str = "http://127.0.0.1:1";

fn offline_client() -> clickhouse::Client {
    clickhouse::Client::default().with_url(OFFLINE_URL)
}

/// A socket that accepts connections and then answers nothing, plus a client
/// pointed at it.
///
/// The failure `send_timeout` / `receive_timeout` cannot catch: they are
/// *server* settings, and a black-holed socket never reaches a server that
/// could apply them. Only the client-side deadline ends these calls.
async fn black_holed_client() -> clickhouse::Client {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a local socket");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let mut accepted = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            accepted.push(stream);
        }
    });
    clickhouse::Client::default().with_url(format!("http://{addr}"))
}

/// Generous relative to a connection-refused round trip, short enough that a
/// black-holed socket test finishes quickly.
const OFFLINE_DEADLINE: Duration = Duration::from_millis(300);

#[tokio::test]
async fn exec_statement_fails_and_names_the_statement_when_the_backend_is_unreachable() {
    let ddl = render_ddl(&Topology::SingleNode, DEFAULT_RETENTION_SECS)
        .await
        .unwrap();
    let err = super::exec_statement(&offline_client(), &ddl[0], Duration::from_secs(5))
        .await
        .expect_err("migrations cannot succeed against an unreachable backend");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("migration DDL statement failed"),
        "the failure must be attributed to the migration step, got: {msg}"
    );
    assert!(
        msg.contains("CREATE TABLE IF NOT EXISTS"),
        "the failing statement text must be in the context so an operator can see which DDL \
         did not apply, got: {msg}"
    );
}

#[tokio::test]
async fn exec_statement_reports_the_client_side_deadline_on_a_stalled_statement() {
    let ddl = render_ddl(&Topology::SingleNode, DEFAULT_RETENTION_SECS)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let err = super::exec_statement(&black_holed_client().await, &ddl[0], OFFLINE_DEADLINE)
        .await
        .expect_err("a statement that never answers must not report success");
    let elapsed = started.elapsed();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("client-side deadline"),
        "a stalled statement must be reported as a deadline breach, not a backend error, \
         got: {msg}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "init must fail at roughly the deadline rather than hang, took {elapsed:?}"
    );
}

#[tokio::test]
async fn ensure_retention_ttl_fails_when_the_create_table_query_cannot_be_read() {
    let err = super::ensure_retention_ttl(
        &offline_client(),
        &Topology::SingleNode,
        3600,
        Duration::from_secs(5),
    )
    .await
    .expect_err("retention reconciliation cannot succeed without reading the live table");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("create_table_query"),
        "the failure must name the metadata read it could not complete, got: {msg}"
    );
}

#[tokio::test]
async fn ensure_retention_ttl_reports_the_client_side_deadline_on_a_stalled_metadata_read() {
    let started = std::time::Instant::now();
    let err = super::ensure_retention_ttl(
        &black_holed_client().await,
        &Topology::SingleNode,
        3600,
        OFFLINE_DEADLINE,
    )
    .await
    .expect_err("a metadata read that never answers must not report success");
    let elapsed = started.elapsed();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("client-side deadline"),
        "expected a deadline breach, got: {msg}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "must fail at roughly the deadline rather than hang, took {elapsed:?}"
    );
}

#[tokio::test]
async fn ensure_insert_dedup_window_fails_when_the_create_table_query_cannot_be_read() {
    let err = super::ensure_insert_dedup_window(&offline_client(), Duration::from_secs(5))
        .await
        .expect_err("dedup-window reconciliation cannot succeed without reading the live table");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("create_table_query"),
        "the failure must name the metadata read it could not complete, got: {msg}"
    );
}

#[tokio::test]
async fn ensure_insert_dedup_window_reports_the_client_side_deadline_on_a_stalled_metadata_read() {
    let started = std::time::Instant::now();
    let err = super::ensure_insert_dedup_window(&black_holed_client().await, OFFLINE_DEADLINE)
        .await
        .expect_err("a metadata read that never answers must not report success");
    let elapsed = started.elapsed();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("client-side deadline"),
        "expected a deadline breach, got: {msg}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "must fail at roughly the deadline rather than hang, took {elapsed:?}"
    );
}

// Run with: cargo test -p cf-gears-clickhouse-usage-collector-plugin --features clickhouse
#[cfg(feature = "clickhouse")]
mod integration {
    use std::time::Duration;

    use secrecy::ExposeSecret;

    use super::super::{
        DEFAULT_RETENTION_SECS, INSERT_DEDUP_WINDOW_BLOCKS, build_client,
        ensure_insert_dedup_window, ensure_retention_ttl, exec_statement, parse_dedup_window,
        parse_endpoint, parse_ttl_seconds,
    };
    use crate::config::ClickHousePluginConfig;
    use crate::infra::storage::migrator::render_ddl;
    use crate::infra::storage::test_ch_server;
    use crate::infra::storage::topology::Topology;

    /// Ceiling on bringing up this test's database on the shared server. Must
    /// stay below the 300s per-test kill in the workspace `.config/nextest.toml`.
    const START_BUDGET: Duration = Duration::from_secs(240);

    /// `gts_id` for the row whose `created_at` sits past `DateTime`'s 2106
    /// ceiling, proving a `DateTime64` TTL does not expire it on write.
    const POST_2106_GTS: &str = "post-2106-ttl";

    /// Read the live `usage_records` DDL back from `system.tables`.
    async fn live_create_sql(client: &clickhouse::Client) -> String {
        client
            .query(
                "SELECT create_table_query \
                 FROM system.tables \
                 WHERE database = currentDatabase() AND name = 'usage_records'",
            )
            .fetch_one::<String>()
            .await
            .expect("create_table_query must be readable")
    }

    #[tokio::test]
    #[ignore = "requires Docker (testcontainers)"]
    async fn rendered_single_node_ddl_creates_tables() {
        let (port, database) = tokio::time::timeout(START_BUDGET, async {
            let port = test_ch_server::server_port()
                .await
                .unwrap_or_else(|e| panic!("the shared ClickHouse test server must come up: {e}"));
            let database = test_ch_server::fresh_database(port)
                .await
                .unwrap_or_else(|e| panic!("the per-test database must be created: {e}"));
            (port, database)
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("live bring-up must finish within {START_BUDGET:?}"));
        let password = test_ch_server::PASSWORD;
        let url = format!("http://default:{password}@127.0.0.1:{port}/{database}");

        let cfg: ClickHousePluginConfig = serde_json::from_str(&format!(
            r#"{{"database_url": "{url}", "allow_insecure_http": true}}"#
        ))
        .expect("valid test config");

        let endpoint =
            parse_endpoint(cfg.database_url.expose_secret()).expect("parseable test URL");
        if let Some(db) = endpoint.database.as_deref()
            && db != "default"
        {
            let mut bootstrap = clickhouse::Client::default().with_url(&endpoint.base_url);
            if let Some(user) = &endpoint.user {
                bootstrap = bootstrap.with_user(user);
            }
            if let Some(password) = &endpoint.password {
                bootstrap = bootstrap.with_password(password.expose_secret());
            }
            bootstrap
                .query(&format!("CREATE DATABASE IF NOT EXISTS `{db}`"))
                .execute()
                .await
                .expect("CREATE DATABASE IF NOT EXISTS must succeed");
        }

        super::install_test_crypto_provider();
        let client = build_client(&cfg).expect("client builds once a provider is installed");
        for stmt in render_ddl(&Topology::SingleNode, DEFAULT_RETENTION_SECS)
            .await
            .expect("render")
        {
            exec_statement(&client, &stmt, cfg.client_deadline())
                .await
                .expect("migration must succeed against a live ClickHouse instance");
        }

        // The server accepted the partition key and stores it as the table's
        // own metadata — not merely that our DDL text contained the clause.
        let partition_key: String = client
            .query(
                "SELECT partition_key FROM system.tables \
                 WHERE database = currentDatabase() AND name = 'usage_records'",
            )
            .fetch_one()
            .await
            .expect("partition_key must be readable");
        assert!(
            partition_key.contains("toYYYYMM(created_at)"),
            "live usage_records must be partitioned monthly on created_at: {partition_key}"
        );
        let ddl_with_partition = live_create_sql(&client).await;
        assert!(
            ddl_with_partition.contains("ttl_only_drop_parts = 1"),
            "live usage_records must keep whole-partition TTL mode: {ddl_with_partition}"
        );

        // Matching seconds must still rewrite a legacy toDateTime TTL.
        client
            .query(&format!(
                "ALTER TABLE usage_records MODIFY TTL \
                 toDateTime(created_at) + INTERVAL {DEFAULT_RETENTION_SECS} SECOND DELETE"
            ))
            .execute()
            .await
            .expect("forcing the legacy toDateTime TTL must succeed");
        let legacy_sql = live_create_sql(&client).await;
        assert!(
            legacy_sql.contains("toDateTime(created_at)"),
            "precondition: live TTL must still wrap created_at: {legacy_sql}"
        );
        ensure_retention_ttl(
            &client,
            &Topology::SingleNode,
            DEFAULT_RETENTION_SECS,
            cfg.client_deadline(),
        )
        .await
        .expect("ensure_retention_ttl must rewrite a toDateTime TTL even when seconds match");
        let rewritten = live_create_sql(&client).await;
        assert!(
            !rewritten.contains("toDateTime(created_at)"),
            "legacy toDateTime TTL must be rewritten to DateTime64: {rewritten}"
        );
        assert_eq!(
            parse_ttl_seconds(&rewritten),
            Some(DEFAULT_RETENTION_SECS),
            "rewritten TTL must keep the matching interval: {rewritten}"
        );

        // Default DDL TTL is 1 year; ensure with a different window must alter.
        let ten_years = 10 * 365 * 86_400;
        ensure_retention_ttl(
            &client,
            &Topology::SingleNode,
            ten_years,
            cfg.client_deadline(),
        )
        .await
        .expect("ensure_retention_ttl must alter when config differs from default");

        let create_sql = live_create_sql(&client).await;
        assert_eq!(
            parse_ttl_seconds(&create_sql),
            Some(ten_years),
            "live TTL must match the configured retention after ensure: {create_sql}"
        );
        assert!(
            !create_sql.contains("toDateTime(created_at)"),
            "configured TTL must keep DateTime64 created_at: {create_sql}"
        );

        // Idempotent when already matched.
        ensure_retention_ttl(
            &client,
            &Topology::SingleNode,
            ten_years,
            cfg.client_deadline(),
        )
        .await
        .expect("ensure_retention_ttl must no-op when TTL already matches");

        // The DDL carries the insert dedup window and the server stores it as
        // table metadata.
        assert_eq!(
            parse_dedup_window(&create_sql),
            Some(INSERT_DEDUP_WINDOW_BLOCKS),
            "fresh usage_records must carry the insert dedup window: {create_sql}"
        );
        // A pre-existing table without it (simulated by turning it off) is
        // retrofitted, and a second run is a no-op.
        client
            .query(
                "ALTER TABLE usage_records MODIFY SETTING non_replicated_deduplication_window = 0",
            )
            .execute()
            .await
            .expect("forcing the window off must succeed");
        let without = live_create_sql(&client).await;
        assert_eq!(
            parse_dedup_window(&without),
            Some(0),
            "precondition: live window must read back as 0: {without}"
        );
        ensure_insert_dedup_window(&client, cfg.client_deadline())
            .await
            .expect("ensure_insert_dedup_window must restore the window");
        let restored = live_create_sql(&client).await;
        assert_eq!(
            parse_dedup_window(&restored),
            Some(INSERT_DEDUP_WINDOW_BLOCKS),
            "window must be restored: {restored}"
        );
        ensure_insert_dedup_window(&client, cfg.client_deadline())
            .await
            .expect("ensure_insert_dedup_window must no-op when already set");

        // A created_at after DateTime's 2106 ceiling must not expire immediately.
        client
            .query(
                "INSERT INTO usage_records (id, tenant_id, gts_id, value, created_at, \
                 resource_id, resource_type, subject_id, subject_type, idempotency_key, \
                 corrects_id, status, metadata, ingested_at, version) VALUES \
                 (generateUUIDv4(), generateUUIDv4(), ?, 1, \
                  toDateTime64('2107-01-01 00:00:00', 6), \
                  'res-1', 'vm', NULL, NULL, 'idem-post-2106', NULL, 'active', map(), \
                  now64(6), 1)",
            )
            .bind(POST_2106_GTS)
            .execute()
            .await
            .expect("inserting a post-2106 created_at must succeed");
        client
            .query("ALTER TABLE usage_records MATERIALIZE TTL")
            .execute()
            .await
            .expect("MATERIALIZE TTL must succeed");
        // `OPTIMIZE … FINAL` is merge-forcing DDL, unrelated to the `FINAL`
        // read modifier the store no longer emits — forcing the merge is the
        // only way to make TTL deletion observable in a test. The `FINAL` on
        // the count below is likewise deliberate: this asserts TTL, not a
        // store read path, so resolving with the engine keeps the check
        // independent of how the store spells its own resolution.
        client
            .query("OPTIMIZE TABLE usage_records FINAL")
            .execute()
            .await
            .expect("OPTIMIZE FINAL must apply TTL during merge");
        let remaining: u64 = client
            .query("SELECT count() FROM usage_records FINAL WHERE gts_id = ?")
            .bind(POST_2106_GTS)
            .fetch_one()
            .await
            .expect("count of post-2106 row must be readable");
        assert_eq!(
            remaining, 1,
            "post-2106 created_at plus a 10-year TTL must not expire on merge"
        );
    }
}

// ---------------------------------------------------------------------------
// Cluster schema validation
// ---------------------------------------------------------------------------

mod cluster_schema {
    use super::super::{check_cluster_schema, engine_args};
    use crate::infra::storage::test_mock::{
        cluster_topology as topology, good_cluster_rows as good_rows,
    };
    use crate::infra::storage::topology::Topology;

    #[test]
    fn reference_schema_passes() {
        assert_eq!(
            check_cluster_schema(&good_rows(), &topology(), "db"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn single_node_is_never_checked() {
        assert!(check_cluster_schema(&[], &Topology::SingleNode, "db").is_empty());
    }

    #[test]
    fn missing_tables_are_each_reported() {
        let problems = check_cluster_schema(&[], &topology(), "db");
        assert_eq!(problems.len(), 4, "{problems:?}");
    }

    /// A non-replicated local table loses the async/insert dedup guarantees the
    /// cluster mode is built on.
    #[test]
    fn non_replicated_local_engine_is_rejected() {
        let mut rows = good_rows();
        rows[0].engine = "ReplacingMergeTree".to_owned();
        let problems = check_cluster_schema(&rows, &topology(), "db");
        assert!(
            problems
                .iter()
                .any(|p| p.contains("ReplicatedReplacingMergeTree")),
            "{problems:?}"
        );
    }

    #[test]
    fn wrong_sort_key_is_rejected() {
        let mut rows = good_rows();
        rows[0].sorting_key = "tenant_id, created_at".to_owned();
        let problems = check_cluster_schema(&rows, &topology(), "db");
        assert!(
            problems.iter().any(|p| p.contains("ORDER BY")),
            "{problems:?}"
        );
    }

    /// A monthly partition is what the whole-partition TTL drop is sized for.
    #[test]
    fn wrong_partition_key_is_rejected() {
        let mut rows = good_rows();
        rows[0].partition_key = "toYYYYMMDD(created_at)".to_owned();
        let problems = check_cluster_schema(&rows, &topology(), "db");
        assert!(
            problems.iter().any(|p| p.contains("PARTITION BY")),
            "{problems:?}"
        );
    }

    /// Sharding by anything but `tenant_id` breaks the shard-colocation
    /// invariant that `distributed_product_mode = 'local'` relies on.
    #[test]
    fn wrong_sharding_key_is_rejected() {
        let mut rows = good_rows();
        rows[2].engine_full = "Distributed('uc', 'db', 'usage_records_local', rand())".to_owned();
        let problems = check_cluster_schema(&rows, &topology(), "db");
        assert!(
            problems.iter().any(|p| p.contains("cityHash64(tenant_id)")),
            "{problems:?}"
        );
    }

    #[test]
    fn distributed_over_the_wrong_cluster_or_table_is_rejected() {
        for engine_full in [
            "Distributed('other', 'db', 'usage_records_local', cityHash64(tenant_id))",
            "Distributed('uc', 'other_db', 'usage_records_local', cityHash64(tenant_id))",
            "Distributed('uc', 'db', 'usage_type_catalog_local', cityHash64(tenant_id))",
        ] {
            let mut rows = good_rows();
            rows[2].engine_full = engine_full.to_owned();
            assert_eq!(
                check_cluster_schema(&rows, &topology(), "db").len(),
                1,
                "{engine_full}"
            );
        }
    }

    #[test]
    fn logical_table_that_is_not_distributed_is_rejected() {
        let mut rows = good_rows();
        rows[3].engine = "ReplacingMergeTree".to_owned();
        let problems = check_cluster_schema(&rows, &topology(), "db");
        assert!(
            problems.iter().any(|p| p.contains("expected Distributed")),
            "{problems:?}"
        );
    }

    #[test]
    fn engine_args_are_split_at_top_level_only() {
        assert_eq!(
            engine_args(
                "Distributed('uc', 'db', 't', cityHash64(tenant_id, gts_id)) SETTINGS fsync_after_insert = 0",
                "Distributed",
            ),
            Some(vec![
                "uc".to_owned(),
                "db".to_owned(),
                "t".to_owned(),
                "cityHash64(tenant_id,gts_id)".to_owned(),
            ])
        );
        assert_eq!(
            engine_args(
                "ReplicatedReplacingMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}', version) ORDER BY gts_id",
                "ReplicatedReplacingMergeTree",
            ),
            Some(vec![
                "/clickhouse/tables/{uuid}/{shard}".to_owned(),
                "{replica}".to_owned(),
                "version".to_owned(),
            ])
        );
        assert_eq!(engine_args("MergeTree ORDER BY id", "Distributed"), None);
        assert_eq!(engine_args("Distributed('uc', 'db'", "Distributed"), None);
    }

    /// Without the version column, merges keep the last inserted row instead
    /// of the highest version, undoing version-based state resolution.
    #[test]
    fn local_engine_without_version_is_rejected() {
        for args in [
            "",
            "'/clickhouse/tables/{uuid}/{shard}', '{replica}'",
            "'/clickhouse/tables/{uuid}/{shard}', '{replica}', ingested_at",
            "'/clickhouse/tables/{uuid}/{shard}', '{replica}', version, is_deleted",
        ] {
            for i in [0, 1] {
                let mut rows = good_rows();
                rows[i].engine_full =
                    format!("ReplicatedReplacingMergeTree({args}) ORDER BY gts_id");
                let problems = check_cluster_schema(&rows, &topology(), "db");
                assert_eq!(problems.len(), 1, "{args}: {problems:?}");
                assert!(problems[0].contains("version"), "{problems:?}");
            }
        }

        let mut rows = good_rows();
        rows[1].engine_full = "ReplicatedReplacingMergeTree ORDER BY gts_id".to_owned();
        assert_eq!(
            check_cluster_schema(&rows, &topology(), "db").len(),
            1,
            "an engine with no arguments at all has no version column"
        );
    }

    #[test]
    fn local_engine_with_version_only_argument_passes() {
        let mut rows = good_rows();
        rows[1].engine_full = "ReplicatedReplacingMergeTree(version) ORDER BY gts_id".to_owned();
        assert_eq!(
            check_cluster_schema(&rows, &topology(), "db"),
            Vec::<String>::new()
        );
    }
}

// ---------------------------------------------------------------------------
// Startup DDL against a mocked server
//
// `clickhouse::test::Mock` answers each request with the next queued handler
// and fails the test on an extra request or an unused handler, so each test
// below pins the exact request sequence: an `ALTER` that must not be sent is
// proven absent by not queuing a handler for it.
// ---------------------------------------------------------------------------

mod mocked {
    use std::time::Duration;

    use clickhouse::test::Mock;

    use super::super::{
        INSERT_DEDUP_WINDOW_BLOCKS, ensure_insert_dedup_window, ensure_retention_ttl,
        validate_cluster_schema,
    };
    use crate::infra::storage::test_mock::{
        capture, client, cluster_topology, create_sql, fail, good_cluster_rows, ok, rows, ttl,
        valid_cluster_probes,
    };
    use crate::infra::storage::topology::Topology;

    const DEADLINE: Duration = Duration::from_secs(5);
    const RETENTION: u64 = 31_536_000;

    // ── ensure_retention_ttl ──────────────────────────────────────────────

    /// A live TTL on the bare column with the configured interval is left
    /// alone: the read is the only request.
    #[tokio::test]
    async fn retention_ttl_that_already_matches_is_not_altered() {
        let mock = Mock::new();
        rows(&mock, [create_sql(&ttl(RETENTION), None)]);

        ensure_retention_ttl(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect("a matching TTL needs no ALTER");
    }

    #[tokio::test]
    async fn retention_ttl_with_a_different_interval_is_altered() {
        let mock = Mock::new();
        rows(&mock, [create_sql(&ttl(86_400), None)]);
        let alter = capture(&mock);

        ensure_retention_ttl(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect("the TTL is reconciled");

        assert_eq!(
            alter.query().await.trim(),
            format!(
                "ALTER TABLE usage_records MODIFY TTL created_at + INTERVAL {RETENTION} SECOND DELETE"
            )
        );
    }

    /// The legacy `toDateTime(created_at)` clause saturates at 2106, so it is
    /// rewritten even when its interval already matches.
    #[tokio::test]
    async fn retention_ttl_on_the_legacy_cast_is_altered_even_when_the_interval_matches() {
        let mock = Mock::new();
        rows(
            &mock,
            [create_sql(
                &format!("toDateTime(created_at) + toIntervalSecond({RETENTION})"),
                None,
            )],
        );
        let alter = capture(&mock);

        ensure_retention_ttl(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect("the legacy TTL is rewritten");

        assert!(
            alter
                .query()
                .await
                .contains("MODIFY TTL created_at + INTERVAL")
        );
    }

    /// A table with no TTL at all gets one. A short window also takes the
    /// whole-partition overshoot warning branch.
    #[tokio::test]
    async fn retention_ttl_is_added_to_a_table_without_one() {
        let mock = Mock::new();
        rows(&mock, [create_sql("", None)]);
        let alter = capture(&mock);

        ensure_retention_ttl(&client(&mock), &Topology::SingleNode, 86_400, DEADLINE)
            .await
            .expect("the TTL is added");

        assert!(alter.query().await.contains("INTERVAL 86400 SECOND DELETE"));
    }

    /// On a cluster the TTL lives on the shard-local table, altered on every
    /// host.
    #[tokio::test]
    async fn retention_ttl_on_a_cluster_alters_the_local_table_on_cluster() {
        let mock = Mock::new();
        rows(&mock, [create_sql("", None)]);
        let alter = capture(&mock);

        ensure_retention_ttl(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect("the TTL is reconciled");

        assert!(
            alter
                .query()
                .await
                .starts_with("ALTER TABLE usage_records_local ON CLUSTER uc MODIFY TTL"),
        );
    }

    #[tokio::test]
    async fn retention_ttl_reports_a_failed_alter_with_its_statement() {
        let mock = Mock::new();
        rows(&mock, [create_sql("", None)]);
        fail(&mock);

        let err = ensure_retention_ttl(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect_err("a rejected ALTER fails startup");

        let msg = format!("{err:#}");
        assert!(msg.contains("failed to apply retention TTL"), "{msg}");
        assert!(msg.contains("MODIFY TTL"), "{msg}");
    }

    #[tokio::test]
    async fn retention_ttl_reports_a_failed_metadata_read() {
        let mock = Mock::new();
        fail(&mock);

        let err = ensure_retention_ttl(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect_err("an unreadable table fails startup");

        assert!(
            format!("{err:#}")
                .contains("failed to read usage_records create_table_query from system.tables"),
            "{err:#}"
        );
    }

    // ── ensure_insert_dedup_window ────────────────────────────────────────

    #[tokio::test]
    async fn dedup_window_that_is_already_set_is_not_altered() {
        let mock = Mock::new();
        rows(&mock, [create_sql("", Some(INSERT_DEDUP_WINDOW_BLOCKS))]);

        ensure_insert_dedup_window(&client(&mock), DEADLINE)
            .await
            .expect("a matching window needs no ALTER");
    }

    #[tokio::test]
    async fn dedup_window_is_retrofitted_onto_a_table_without_it() {
        for window in [None, Some(100)] {
            let mock = Mock::new();
            rows(&mock, [create_sql("", window)]);
            let alter = capture(&mock);

            ensure_insert_dedup_window(&client(&mock), DEADLINE)
                .await
                .expect("the window is reconciled");

            assert_eq!(
                alter.query().await.trim(),
                format!(
                    "ALTER TABLE usage_records MODIFY SETTING \
                     non_replicated_deduplication_window = {INSERT_DEDUP_WINDOW_BLOCKS}"
                ),
                "{window:?}"
            );
        }
    }

    #[tokio::test]
    async fn dedup_window_reports_a_failed_alter() {
        let mock = Mock::new();
        rows(&mock, [create_sql("", None)]);
        fail(&mock);

        let err = ensure_insert_dedup_window(&client(&mock), DEADLINE)
            .await
            .expect_err("a rejected ALTER fails startup");

        assert!(
            format!("{err:#}").contains("failed to apply insert dedup window"),
            "{err:#}"
        );
    }

    // ── validate_cluster_schema ───────────────────────────────────────────

    /// A single node has no cluster schema to check, and nothing is asked.
    #[tokio::test]
    async fn validation_is_a_no_op_on_a_single_node() {
        let mock = Mock::new();

        validate_cluster_schema(&client(&mock), &Topology::SingleNode, RETENTION, DEADLINE)
            .await
            .expect("nothing to validate");
    }

    #[tokio::test]
    async fn validation_passes_on_a_usable_cluster() {
        let mock = Mock::new();
        valid_cluster_probes(&mock, 2, RETENTION);

        validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect("the reference schema is usable");
    }

    /// A TTL drift on an operator-provisioned cluster is only warned about:
    /// fixing it would be DDL the plugin does not run there.
    #[tokio::test]
    async fn validation_passes_with_only_a_warning_when_the_ttl_differs() {
        let mock = Mock::new();
        valid_cluster_probes(&mock, 2, 86_400);

        validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect("a TTL drift does not fail startup");
    }

    #[tokio::test]
    async fn validation_fails_when_the_cluster_is_not_declared() {
        let mock = Mock::new();
        rows(&mock, ["db".to_owned()]);
        rows(&mock, [0_u64]);

        let err = validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect_err("an undeclared cluster fails startup");

        assert!(
            format!("{err:#}").contains("is not declared in system.clusters"),
            "{err:#}"
        );
    }

    /// A deviating schema fails with every problem and the DDL that fixes it.
    #[tokio::test]
    async fn validation_fails_with_the_problems_and_the_ddl_on_a_deviating_schema() {
        let mock = Mock::new();
        rows(&mock, ["db".to_owned()]);
        rows(&mock, [2_u64]);
        rows(&mock, good_cluster_rows().into_iter().skip(1));
        rows(&mock, [8_u64]);

        let err = validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect_err("a missing local table fails startup");

        let msg = format!("{err:#}");
        assert!(msg.contains("set cluster.manage_schema"), "{msg}");
        assert!(
            msg.contains("local table `usage_records_local` does not exist"),
            "{msg}"
        );
        assert!(msg.contains("CREATE TABLE IF NOT EXISTS"), "{msg}");
    }

    /// The connected node can hold every table while a replica lacks some.
    #[tokio::test]
    async fn validation_fails_when_a_replica_lacks_tables() {
        let mock = Mock::new();
        rows(&mock, ["db".to_owned()]);
        rows(&mock, [2_u64]);
        rows(&mock, good_cluster_rows());
        rows(&mock, [6_u64]);

        let err = validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect_err("a replica missing tables fails startup");

        assert!(
            format!("{err:#}").contains("only 6 of 8 expected tables exist across the 2 replicas"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn validation_names_the_probe_that_failed() {
        let mock = Mock::new();
        fail(&mock);

        let err = validate_cluster_schema(&client(&mock), &cluster_topology(), RETENTION, DEADLINE)
            .await
            .expect_err("a failed probe fails startup");

        assert!(
            format!("{err:#}").contains("reading currentDatabase() failed"),
            "{err:#}"
        );
    }

    /// Statements that only need a success answer still go through `ok`.
    #[tokio::test]
    async fn exec_statement_succeeds_on_an_accepted_statement() {
        let mock = Mock::new();
        ok(&mock);

        super::super::exec_statement(&client(&mock), "SELECT 1", DEADLINE)
            .await
            .expect("an accepted statement succeeds");
    }
}
