# cyberware-timescaledb-usage-collector-plugin

Production TimescaleDB storage plugin for the cyberware usage-collector subsystem.
This crate implements the `UsageCollectorPluginClientV1` contract from
`usage-collector-sdk`, persisting usage records into a TimescaleDB hypertable and
serving aggregation queries via continuous aggregates and raw cursor-paginated
reads. It targets durable high-throughput ingest with bounded connection-pool
sizing, retention policy management, and OpenTelemetry-based observability.
Configure via the `timescaledb-usage-collector-plugin` module config block
(see the crate docs for the schema). The key must match the ModKit module name
(`#[modkit::module(name = "timescaledb-usage-collector-plugin")]`) because
`ctx.config()` reads `modules.<module-name>.config` verbatim.

## Architectural deviation: raw sqlx instead of `SecureConn`

This plugin deliberately bypasses the framework's `SecureConn` / `SecureORM`
abstraction (which `MODKIT-SEC-001` requires for module DB access) because
the storage paths it implements have no SecureORM equivalent: the
TimescaleDB-specific DDL (`CREATE EXTENSION`, `create_hypertable`,
`CREATE MATERIALIZED VIEW ... WITH (timescaledb.continuous)`,
`add_continuous_aggregate_policy`, `add_retention_policy`), the cross-partition
idempotency-key table, and the dynamically-built aggregation / pagination SQL
in `infra/pg_query_port.rs` all need direct `sqlx::PgPool` access. The
framework lint `de0706_no_direct_sqlx` is suppressed crate-wide in
`infra/` and at the module bootstrap (`src/module.rs`) to acknowledge this.

The substitute boundary for authorization is the scope-fragment translator in
`domain/scope.rs`: every read and write composes the WHERE clause through
`scope_to_sql`, which fails closed on empty scopes and rejects `InGroup` /
`InGroupSubtree` predicates rather than silently dropping them. That fragment
— and not `SecureConn` — is the authorization invariant this plugin commits
to. The full trade-off (including the conditions under which a
SecureConn-equivalent for TimescaleDB DDL would be preferred) is recorded in
[ADR-0003](../../docs/ADR/0003-cpt-cf-usage-collector-adr-timescaledb-plugin-raw-sqlx.md).

## Operational notes

- **Idempotency-key cleanup is in-process and periodic.**
  `setup_retention_policy` (`src/infra/retention.rs`) installs a TimescaleDB
  retention policy on `usage_records` that runs continuously. The companion
  `usage_idempotency_keys` table is a plain table with no hypertable retention
  job, so the plugin issues the equivalent `DELETE` once at startup and then
  again on a recurring interval from a background task in `module.rs`
  (`run_idempotency_cleanup_loop`). Operators who prefer a database-side
  schedule can additionally install a TimescaleDB user-defined `add_job`
  running the same statement.
