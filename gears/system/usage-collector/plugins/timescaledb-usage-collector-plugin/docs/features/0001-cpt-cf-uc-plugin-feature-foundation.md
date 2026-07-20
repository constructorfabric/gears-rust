# Feature: Foundation — Bootstrap, Schema & SPI Wiring

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Backend Bind at Startup](#backend-bind-at-startup)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Idempotent Schema Provisioning](#idempotent-schema-provisioning)
  - [Backend Error Classification](#backend-error-classification)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement the Plugin Module lifecycle](#implement-the-plugin-module-lifecycle)
  - [Implement the SPI Storage Adapter shell and error classification](#implement-the-spi-storage-adapter-shell-and-error-classification)
  - [Implement idempotent Schema Migrations](#implement-idempotent-schema-migrations)
  - [Implement GTS-scoped registration](#implement-gts-scoped-registration)
  - [Enforce the TLS-required, secret-wrapped DSN](#enforce-the-tls-required-secret-wrapped-dsn)
  - [Publish the consistency profile](#publish-the-consistency-profile)
  - [Preserve vendor isolation](#preserve-vendor-isolation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-featstatus-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p1` - `cpt-cf-uc-plugin-feature-foundation`

## 1. Feature Context

### 1.1 Overview

Establish the plugin's runtime substrate and its single public surface — the storage SPI — so every capability plugs into one identical execution shape: a `#[toolkit::gear]` `init` that loads typed config, builds a TLS-required connection pool, provisions the schema idempotently, and performs the GTS + ClientHub registration handshake.

### 1.2 Purpose

Foundation owns the cross-cutting plumbing every other feature builds on: the Plugin Module lifecycle, the SPI Storage Adapter (the host's only entry point and the owner of backend-error classification and keyset cursor encoding), the idempotent Schema Migrations, and the pure-persistence / SPI-conformance / TLS-credential-non-disclosure / vendor-isolation guarantees. It performs no record, query, catalog, retention, or metric behavior itself — it exposes the shape those features realize.

**Requirements**: `cpt-cf-uc-plugin-fr-schema-provisioning`, `cpt-cf-uc-plugin-fr-registration`, `cpt-cf-uc-plugin-fr-error-classification`, `cpt-cf-uc-plugin-nfr-spi-stability`, `cpt-cf-uc-plugin-nfr-transport-security`, `cpt-cf-uc-plugin-nfr-consistency-profile`

**Principles**: `cpt-cf-uc-plugin-principle-pure-persistence`, `cpt-cf-uc-plugin-principle-spi-conformance`

**Constraints**: `cpt-cf-uc-plugin-constraint-vendor-isolation`

### 1.3 Actors

| Actor                                | Role in Feature                                                                                                                                       |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | The Usage Collector core; triggers gear `init`, then discovers and binds the registered backend by vendor/priority and is the sole caller of the SPI. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-foundation`
- **Design elements**: `cpt-cf-uc-plugin-design-timescaledb`, `cpt-cf-uc-plugin-tech-stack`, `cpt-cf-uc-plugin-component-module`, `cpt-cf-uc-plugin-component-adapter`, `cpt-cf-uc-plugin-component-migrations`, `cpt-cf-uc-plugin-db-schema`
- **Interfaces**: `cpt-cf-uc-plugin-interface-storage-spi`, `cpt-cf-uc-plugin-interface-spi`
- **Contracts**: `cpt-cf-uc-plugin-contract-timescaledb`, `cpt-cf-uc-plugin-contract-gts-registration`
- **Use cases**: `cpt-cf-uc-plugin-usecase-bind-startup`
- **Dependencies**: None

### 1.5 Out of Scope

- Record insert, dedup, compensation, and deactivation semantics — Feature 2 (`cpt-cf-uc-plugin-feature-record-persistence`).
- Aggregation and keyset raw-list execution and injection-safe translation — Feature 3 (`cpt-cf-uc-plugin-feature-query-aggregation`).
- Usage-type CRUD and FK-rejection lift — Feature 4 (`cpt-cf-uc-plugin-feature-usage-type-catalog`).
- The retention policy's runtime expiry behavior — Feature 5 (`cpt-cf-uc-plugin-feature-retention`) (the DDL that registers the policy is created here; its runtime effect is owned there).
- The `uc_timescaledb_*` metric inventory — Feature 6 (`cpt-cf-uc-plugin-feature-observability`).
- Database deployment topology (HA, sizing, region), backup/restore, and DR — owned by the operator's TimescaleDB deployment guide.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-uc-plugin-usecase-bind-startup`

### Backend Bind at Startup

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-foundation-bind-startup`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- Config loads, pool builds over TLS, schema provisions, the backend registers under its GTS instance scope, and the host binds it by vendor/priority.

**Error Scenarios**:

- Invalid config, unreachable database, or missing TimescaleDB extension — `init` fails fast; the plugin does not register and the host does not bind it.
- Non-TLS endpoint — the connection is refused.

**Steps**:

1. [ ] - `p1` - Host process starts with the TimescaleDB plugin enabled; ToolKit invokes the gear `init` - `inst-boot-1`
2. [ ] - `p1` - Load and validate the typed plugin configuration (database DSN, pool bounds, connection timeout, retention window, vendor, priority) - `inst-boot-2`
3. [ ] - `p1` - Build the `sqlx` connection pool over the TLS-required, secret-wrapped DSN - `inst-boot-3`
4. [ ] - `p1` - **IF** the endpoint is non-TLS or the pool cannot be built - `inst-boot-4`
   1. [ ] - `p1` - **RETURN** gear initialization failure without registering - `inst-boot-4a`
5. [ ] - `p1` - Run the idempotent Schema Migrations (`cpt-cf-uc-plugin-algo-foundation-schema-provisioning`) - `inst-boot-5`
6. [ ] - `p1` - **IF** provisioning fails - `inst-boot-6`
   1. [ ] - `p1` - **RETURN** gear initialization failure without registering - `inst-boot-6a`
7. [ ] - `p1` - Build the `PluginV1<UsageCollectorPluginSpecV1>` registration and publish the instance to `types-registry` - `inst-boot-7`
8. [ ] - `p1` - Register the SPI Storage Adapter as a scoped `UsageCollectorPluginV1` client via ClientHub under the GTS instance scope, carrying the configured vendor and priority - `inst-boot-8`
9. [ ] - `p1` - **RETURN** backend registered and ready; the host performs vendor/priority selection - `inst-boot-9`

## 3. Processes / Business Logic (CDSL)

### Idempotent Schema Provisioning

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-foundation-schema-provisioning`

**Input**: The connection pool from `init`.

**Output**: A provisioned schema (`cpt-cf-uc-plugin-db-schema`), re-runnable as a no-op on restart.

**Steps**:

1. [ ] - `p1` - DB: CREATE EXTENSION timescaledb if absent - `inst-prov-1`
2. [ ] - `p1` - DB: CREATE the `usage_type_catalog` table if absent (`cpt-cf-uc-plugin-dbtable-usage-type-catalog`) - `inst-prov-2`
3. [ ] - `p1` - DB: CREATE the `usage_records` hypertable partitioned on `created_at` if absent (`cpt-cf-uc-plugin-dbtable-usage-records`) - `inst-prov-3`
4. [ ] - `p1` - DB: ENSURE the `UNIQUE (tenant_id, gts_id, idempotency_key, created_at)` dedup constraint (`usage_records_dedup_uniq`) - `inst-prov-4`
5. [ ] - `p1` - DB: ENSURE the `usage_records.gts_id → usage_type_catalog.gts_id` foreign key `ON DELETE RESTRICT` - `inst-prov-5`
6. [ ] - `p1` - DB: ENSURE the time-windowed read/aggregation indexes - `inst-prov-6`
7. [ ] - `p1` - Register the declarative `usage_records` retention policy idempotently (runtime effect owned by Feature 5) - `inst-prov-7`
8. [ ] - `p1` - **RETURN** provisioning complete; each step is a no-op when the object already exists - `inst-prov-8`

### Backend Error Classification

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-foundation-error-classification`

**Input**: A backend/SQL error surfaced from a store operation.

**Output**: A `UsageCollectorPluginError` classified as `Transient` vs `Internal` plus the typed domain variants, per `cpt-cf-uc-plugin-principle-spi-conformance`.

**Steps**:

1. [ ] - `p1` - Inspect the backend error kind - `inst-err-1`
2. [ ] - `p1` - **IF** the error is a typed domain condition (idempotency conflict, record not found, already inactive, usage-type not found / already exists / referenced) - `inst-err-2`
   1. [ ] - `p1` - Map it to the corresponding typed `UsageCollectorPluginError` variant - `inst-err-2a`
3. [ ] - `p1` - **IF** the error is retryable (connection loss, deadlock victim, pool acquire timeout) - `inst-err-3`
   1. [ ] - `p1` - Classify as `Transient` (retryable) - `inst-err-3a`
4. [ ] - `p1` - **ELSE** - `inst-err-4`
   1. [ ] - `p1` - Classify as `Internal` (non-retryable); a malformed or unauthorized call reaching the SPI is a host-contract breach and surfaces here - `inst-err-4a`
5. [ ] - `p1` - **RETURN** the classified error so the host applies retry / fail-closed behavior without backend-specific parsing - `inst-err-5`

## 4. States (CDSL)

Not applicable — Foundation establishes the schema, adapter, and registration handshake but defines no entity lifecycle state machine. The `active → inactive` record lifecycle belongs to Feature 2 (`cpt-cf-uc-plugin-state-record-status`), and the backend-readiness gauge lifecycle to Feature 6.

## 5. Definitions of Done

### Implement the Plugin Module lifecycle

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-foundation-module`

The system **MUST** implement the `#[toolkit::gear]` `init` that loads and validates the typed configuration, builds the connection pool, invokes Schema Migrations, and performs GTS publication plus ClientHub scoped registration. The module **MUST NOT** decide whether it is the active backend and **MUST NOT** implement SPI methods directly.

**Implements**:

- `cpt-cf-uc-plugin-flow-foundation-bind-startup`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-module`
- Entities: `UsageCollectorPluginV1`, typed plugin configuration, connection-pool handle

### Implement the SPI Storage Adapter shell and error classification

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-foundation-adapter`

The system **MUST** implement the single `UsageCollectorPluginV1` adapter that routes record operations to the Record Store and catalog operations to the Catalog Store, holds no business logic or authorization, owns backend-error classification and keyset cursor encoding, and runs inside the host's ambient tracing span.

**Implements**:

- `cpt-cf-uc-plugin-algo-foundation-error-classification`

**Constraints**: `cpt-cf-uc-plugin-principle-spi-conformance`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-adapter`
- Interface: `cpt-cf-uc-plugin-interface-spi`, `cpt-cf-uc-plugin-interface-storage-spi`
- Entities: `UsageCollectorPluginError`

### Implement idempotent Schema Migrations

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-foundation-migrations`

The system **MUST** create, idempotently at `init`, the `timescaledb` extension, the `usage_type_catalog` table, the `usage_records` hypertable with its 4-tuple dedup uniqueness constraint and `ON DELETE RESTRICT` foreign key, the query-supporting indexes, and the retention-policy registration — all re-runnable as no-ops on restart.

**Implements**:

- `cpt-cf-uc-plugin-algo-foundation-schema-provisioning`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-migrations`
- DB: `cpt-cf-uc-plugin-db-schema`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`, `cpt-cf-uc-plugin-dbtable-usage-type-catalog`

### Implement GTS-scoped registration

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-foundation-registration`

The system **MUST** build the `PluginV1<UsageCollectorPluginSpecV1>` registration, publish it to `types-registry`, and register the adapter as a scoped `UsageCollectorPluginV1` client via ClientHub under the GTS instance scope, carrying the configured vendor and priority, so the host's plugin selection can discover and bind it.

**Implements**:

- `cpt-cf-uc-plugin-flow-foundation-bind-startup`

**Touches**:

- Contract: `cpt-cf-uc-plugin-contract-gts-registration`

### Enforce the TLS-required, secret-wrapped DSN

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-foundation-tls-dsn`

The system **MUST** refuse non-TLS database connections and **MUST** hold the DSN as a secret-wrapped value so the embedded Postgres password never reaches `Debug`, logs, or error output.

**Implements**:

- `cpt-cf-uc-plugin-flow-foundation-bind-startup`

**Constraints**: `cpt-cf-uc-plugin-nfr-transport-security`

**Touches**:

- Contract: `cpt-cf-uc-plugin-contract-timescaledb`

### Publish the consistency profile

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-foundation-consistency-profile`

The system **MUST** publish, per deployment topology, the backend's consistency profile: a single-node PostgreSQL/TimescaleDB deployment provides read-after-write visibility of a committed record, and a read-replica deployment documents its staleness bound.

**Constraints**: `cpt-cf-uc-plugin-nfr-consistency-profile`

**Touches**:

- Design: `cpt-cf-uc-plugin-design-timescaledb`

### Preserve vendor isolation

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-foundation-vendor-isolation`

The system **MUST** keep all TimescaleDB-specific SQL, schema, and client dependencies inside this crate, depending only on `usage-collector-sdk` and `types-registry-sdk` and never on the host `usage-collector` crate.

**Constraints**: `cpt-cf-uc-plugin-constraint-vendor-isolation`

**Touches**:

- Design: `cpt-cf-uc-plugin-tech-stack`

## 6. Acceptance Criteria

- [ ] The crate implements the full `UsageCollectorPluginV1` SPI and conforms to the SDK trait at build time, with no dependency on the host `usage-collector` crate.
- [ ] `init` loads config, builds the pool over TLS, provisions the schema, and registers under a GTS instance identifier carrying the configured vendor and priority; the plugin does not self-select as the active backend.
- [ ] Re-running `init` on restart re-provisions the schema and re-registers the retention policy as no-ops (idempotent), with no error and no duplicate objects.
- [ ] A non-TLS endpoint is refused, and the DSN and its embedded credentials never appear in logs, error messages, or debug output.
- [ ] Backend/SQL errors are surfaced as `UsageCollectorPluginError` classified `Transient` vs `Internal` plus the typed domain variants; a malformed or unauthorized call surfaces as `Internal`.
- [ ] Invalid config, an unreachable database, or a missing TimescaleDB extension fails `init` fast; the plugin does not register.
- [ ] The single-node consistency profile is published as read-after-write for a committed record.

## 7. Non-Applicable Concerns

- **Security — Authentication & Authorization (SEC-FDESIGN-001, SEC-FDESIGN-002)**: Not applicable — authentication and PDP authorization are enforced upstream by the gear core (`cpt-cf-uc-plugin-principle-pure-persistence`); every SPI call arrives already authorized. Foundation's only security obligations are transport security and credential non-disclosure (`cpt-cf-uc-plugin-dod-foundation-tls-dsn`).
- **Security — Audit Trail (SEC-FDESIGN-005)**: Not applicable — the plugin produces no auditable user actions; user attribution and audit are gear-core concerns.
- **Data Privacy (DATA-FDESIGN-005) / Compliance (COMPL)**: Not applicable — the plugin holds only opaque identifiers and opaque metadata passed through from the gear and performs no classification; privacy/regulatory obligations are gear-owned.
- **Usability (UX)**: Not applicable — no user interface; all interaction is programmatic via the in-process SPI.
- **Observability (OPS-FDESIGN-001)**: Addressed cross-cuttingly by Feature 6 (`cpt-cf-uc-plugin-feature-observability`); Foundation provides only the meter and pool the metric inventory instruments.
- **Performance (PERF)**: Not applicable — Foundation is startup wiring with no runtime hot path; ingestion-throughput and query-latency budgets are allocated to Features 2 and 3.
