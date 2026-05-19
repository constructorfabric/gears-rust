<!-- Updated: 2026-05-19 by Constructor Tech -->

# ADR-0003: Bypass `SecureConn` / `SecureORM` in the TimescaleDB Storage Plugin


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Route every access through `SecureConn` / `SecureORM`](#route-every-access-through-secureconn--secureorm)
  - [Raw `sqlx::PgPool` with a scope-fragment translator as the authorization boundary](#raw-sqlxpgpool-with-a-scope-fragment-translator-as-the-authorization-boundary)
  - [Wait for a TimescaleDB-aware `SecureORM` extension before shipping the plugin](#wait-for-a-timescaledb-aware-secureorm-extension-before-shipping-the-plugin)
- [More Information](#more-information)
  - [Implementation notes](#implementation-notes)
- [Review Cadence](#review-cadence)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-timescaledb-plugin-raw-sqlx`

## Context and Problem Statement

`MODKIT-SEC-001` requires every module's database access to go through `SecureConn` / `SecureORM`, which composes authorization (`AccessScope` predicates) into the underlying SeaORM query and rejects un-scoped reads at the framework boundary. The TimescaleDB production storage plugin cannot satisfy that contract end-to-end: it owns TimescaleDB-specific DDL that has no `SecureORM` equivalent (`CREATE EXTENSION`, `create_hypertable`, `CREATE MATERIALIZED VIEW ... WITH (timescaledb.continuous)`, `add_continuous_aggregate_policy`, `add_retention_policy`), a cross-partition `usage_idempotency_keys` plain table whose dedup semantics rely on a two-step transaction rather than `ON CONFLICT` on the hypertable, and a dynamically-built aggregation / pagination SQL surface in `infra/pg_query_port.rs` that emits positional `$N` binds composed against a translator-generated `WHERE` fragment. None of those paths can be expressed through SeaORM today, so a strict reading of `MODKIT-SEC-001` would block the plugin from ever landing.

## Decision Drivers

* The plugin owns TimescaleDB DDL (`create_hypertable`, continuous aggregates, retention/policy jobs) that `SecureORM` does not model and cannot model without a TimescaleDB-specific extension
* Authorization for usage records is read-time, scope-based, and tenant-partitioned — every read composes an `AccessScope` predicate that the framework's PDP already produced upstream, so the substantive authorization decision is **not** what `SecureConn` would add; what `SecureConn` would add is uniform enforcement
* Cross-partition idempotency on `usage_records` requires a transaction across two tables (`usage_idempotency_keys` claim + hypertable insert); SeaORM has no idiomatic equivalent for the claim half because the partition column constraint precludes `ON CONFLICT` on `usage_records`
* The aggregation query surface is dynamic (group-by dimension, time bucket, aggregation function, filter shape) and composes a positional-bind SQL string — expressible as raw `sqlx` but not as a SeaORM entity query
* The framework lint `de0706_no_direct_sqlx` must be suppressed crate-wide for any plugin that takes this approach, and that suppression must be justified in a checked-in document so reviewers can audit the deviation without re-deriving the trade-off

## Considered Options

* Route every access through `SecureConn` / `SecureORM`
* Raw `sqlx::PgPool` with a scope-fragment translator as the authorization boundary
* Wait for a TimescaleDB-aware `SecureORM` extension before shipping the plugin

## Decision Outcome

Chosen option: "Raw `sqlx::PgPool` with a scope-fragment translator as the authorization boundary", because it is the only option that lets the plugin own the TimescaleDB DDL and dynamic-aggregation surface it needs while preserving a single, auditable authorization invariant (`scope_to_sql` in `domain/scope.rs`) that every read and write goes through. The framework lint is suppressed crate-wide at `src/infra/mod.rs` and `src/module.rs`; this ADR is the checked-in record of the trade-off the suppression acknowledges.

### Consequences

* Good, because the plugin can own TimescaleDB-specific DDL and policies in `infra/migrations.rs`, `infra/continuous_aggregate.rs`, and `infra/retention.rs` without having to fork the platform's ORM stack
* Good, because the authorization invariant is concentrated in one file (`domain/scope.rs::scope_to_sql`) and one composition point per read path — easier to audit, easier to unit-test, and the translator fails closed on empty `AccessScope` and rejects `InGroup` / `InGroupSubtree` predicates rather than silently dropping them (returning `ScopeTranslationError::UnsupportedPredicate`, mapped to `PermissionDenied` at the public boundary)
* Good, because dynamic aggregation SQL stays in one place (`infra/pg_query_port.rs::build_aggregation_sql`) with positional `$N` binds and no string interpolation of user-controlled values, keeping the SQL-injection surface flat and reviewable
* Good, because the noop plugin path stays available unchanged — the deviation is scoped to this one production plugin
* Bad, because future contributors must remember that the `de0706_no_direct_sqlx` allow is intentional in this crate and not a drift to be cleaned up; the ADR + README are the durable signal
* Bad, because the plugin does not benefit from cross-cutting `SecureConn` features added later (e.g., per-query tracing, audit hooks at the framework boundary) without explicit plumbing
* Bad, because correctness of authorization depends on every read path actually calling `scope_to_sql`; this is a code-review invariant rather than a type-system one

### Confirmation

* Code review: every read path that produces a `WHERE` clause composes it through `scope_to_sql` (`domain/scope.rs`); no read path constructs `WHERE` text directly
* Code review: every dynamic SQL site in `infra/pg_query_port.rs` uses positional `$N` binds; no `format!`-into-SQL of caller-supplied values outside of identifier whitelists (aggregation function, group-by column, time-bucket interval — each validated against an enum or fixed set)
* Unit tests: `domain/scope.rs` rejects empty scopes (`ScopeTranslationError::EmptyScope`) and `InGroup` / `InGroupSubtree` predicates (`ScopeTranslationError::UnsupportedPredicate`); error mapping at the public boundary surfaces `PermissionDenied`
* Lint: `#![allow(de0706_no_direct_sqlx)]` appears only in `src/infra/mod.rs` and `src/module.rs` (the plugin bootstrap), nowhere else, and is documented at the point of suppression

## Pros and Cons of the Options

### Route every access through `SecureConn` / `SecureORM`

Implement every read and write through the framework's `SecureORM` wrapper, even for the TimescaleDB-specific operations.

* Good, because uniform enforcement of `MODKIT-SEC-001` with no plugin-specific deviation
* Good, because the plugin benefits from future cross-cutting features added at the `SecureConn` boundary
* Bad, because TimescaleDB DDL (`create_hypertable`, continuous-aggregate views with `WITH (timescaledb.continuous)`, retention/policy jobs) is not expressible through SeaORM today and would require forking or extending the framework's ORM stack before the plugin can land
* Bad, because the cross-partition idempotency claim cannot be expressed as `ON CONFLICT` on the hypertable; the two-step transaction has no idiomatic SeaORM equivalent
* Bad, because the dynamic-aggregation query surface (group-by dimension, time bucket, aggregation function, filter shape) is awkward to express as SeaORM entity queries; the resulting code would be less reviewable for SQL-injection risk than positional-bind raw SQL

### Raw `sqlx::PgPool` with a scope-fragment translator as the authorization boundary

Use raw `sqlx::PgPool` for all reads, writes, and DDL. Concentrate authorization in `domain/scope.rs::scope_to_sql`, which converts an `AccessScope` into a positional-bind `WHERE` fragment and fails closed on empty scopes and unsupported predicates. Suppress `de0706_no_direct_sqlx` crate-wide and document the deviation in this ADR.

* Good, because the plugin owns the TimescaleDB-specific DDL and dynamic-aggregation surface it needs without forking the framework
* Good, because the authorization invariant is concentrated and unit-testable, and the translator fails closed
* Good, because the SQL-injection surface stays small — positional binds for all caller-supplied values, identifier whitelists for the small set of dynamic identifiers
* Bad, because the deviation must be tracked durably (this ADR) so future contributors do not "fix" the allow lint and silently break the design
* Bad, because correctness depends on every read path actually using the translator; this is a code-review invariant rather than a type-level one

### Wait for a TimescaleDB-aware `SecureORM` extension before shipping the plugin

Defer the plugin until the framework grows a TimescaleDB-aware `SecureORM` extension that can model hypertables, continuous aggregates, retention policies, and the cross-partition idempotency pattern natively.

* Good, because the plugin would land with full `MODKIT-SEC-001` compliance and no per-crate deviation
* Bad, because no such extension exists or is scoped, and the production storage plugin is on the critical path for the usage-collector subsystem
* Bad, because the framework would acquire a TimescaleDB-specific surface area on behalf of a single plugin, conflating framework concerns with vendor specifics

## More Information

The framework lint `de0706_no_direct_sqlx` is suppressed at:

- `src/infra/mod.rs` — covers `pg_insert_port.rs`, `pg_query_port.rs`, `migrations.rs`, `continuous_aggregate.rs`, `retention.rs`
- `src/module.rs` — covers the bootstrap path that constructs the `PgPool` and registers the plugin client with the gateway

The authorization translator lives at `src/domain/scope.rs::scope_to_sql`. It returns a `ScopeFragment` (a positional-bind `WHERE` fragment + the matching argument list); callers compose the fragment into the read SQL and bind the arguments in order. Rejected predicates surface as `ScopeTranslationError::UnsupportedPredicate`, mapped to `UsageCollectorError::PermissionDenied` at the public boundary.

### Implementation notes

The plugin's `Debug` impl on `Config` redacts `database_url`. The `database_url` field is the only secret the plugin handles; everything else (pool size, retention window, aggregation bucket) is non-sensitive.

The plugin does not write SQL anywhere except `src/infra/` and migrations; the domain layer (`src/domain/`) is pure and contains no SQL strings. This separation is what keeps the deviation reviewable: a reader auditing authorization only needs to read `domain/scope.rs` and confirm that every `infra/` SQL site composes the resulting fragment.

## Review Cadence

This decision is stable as long as the framework's `SecureORM` lacks first-class TimescaleDB support. Revisit if:

- A TimescaleDB-aware `SecureConn` / `SecureORM` extension lands in the framework with first-class support for hypertables, continuous aggregates, retention policies, and a cross-partition idempotency idiom
- The plugin acquires a second authorization boundary (e.g., row-level group membership) that the scope-fragment translator cannot express, weakening the "one auditable boundary" argument
- A future plugin needs to reuse the same deviation, in which case the trade-off should be lifted to a framework-level policy rather than a per-plugin ADR

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **FEATURE**: [features/0004-cpt-cf-usage-collector-feature-production-storage-plugin.md](../features/0004-cpt-cf-usage-collector-feature-production-storage-plugin.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-usage-collector-feature-production-storage-plugin` — formalizes the trade-off the feature spec assumed when it specified TimescaleDB-specific DDL and a dynamic-aggregation surface
* `cpt-cf-usage-collector-component-storage-plugin` — the storage-plugin component owns the deviation; consumers see only the canonical `UsageCollectorPluginClientV1` contract
* `cpt-cf-usage-collector-principle-fail-closed` — the scope-fragment translator fails closed on empty `AccessScope` and unsupported predicates, preserving the fail-closed invariant `SecureConn` would otherwise enforce
