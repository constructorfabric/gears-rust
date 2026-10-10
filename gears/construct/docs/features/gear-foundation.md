# Feature: Gear Foundation


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Start the Gear](#start-the-gear)
  - [Map Domain Errors to Problem Errors](#map-domain-errors-to-problem-errors)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Gear Anatomy and Startup](#gear-anatomy-and-startup)
  - [RFC 9457 Error Behavior](#rfc-9457-error-behavior)
  - [Placeholder Note Table](#placeholder-note-table)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-construct-featstatus-gear-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-construct-feature-gear-foundation`

## 1. Feature Context

### 1.1 Overview

The Gear Foundation is the backend shell every other Construct feature is built on: the two crates, the startup of the gear, tenant-scoped storage through the secure ORM, a permission check per operation, and the mapping of errors to RFC 9457 Problems. It first carried a placeholder, a "foundation note" with one route, two client operations and one table. Record Intake (`cpt-cf-construct-feature-record-intake`) retired that placeholder: its route, client operations, service, repository, entity and model are gone, and the migration `m003_record_ids` drops the note table. This document describes what remains.

### 1.2 Purpose

Construct needs a base that is a real platform gear: an SDK crate, an implementation crate, a permission check for each operation, and storage limited to one tenant. The foundation proved this pattern once, so later features copy it. The decision to build Construct as a gear is in `cpt-cf-construct-adr-construct-is-a-gear`. The standard gear layout is described in `cpt-cf-construct-tech-rust-gear`.

**Out of scope** (as in DECOMPOSITION entry 2.1): any DESIGN table, entity, component or sequence; routes for records, profiles and subjects; the full Rust SDK interface `cpt-cf-construct-interface-rust-sdk`, which grows with each later feature.

**Requirements**: `cpt-cf-construct-fr-tenant-isolation`, `cpt-cf-construct-fr-access-control`

**Principles**: None. The shell applies no DESIGN principle.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-construct-actor-platform-auth` | Authenticates the caller before a request reaches Construct, supplies the caller and tenant in the security context, and decides through the AuthZ resolver whether the caller may run an operation. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md), entry 2.1
- **ADR**: [ADR-0001](../ADR/0001-cpt-cf-construct-adr-construct-is-a-gear.md)
- **Dependencies**: None. Every other Construct feature depends on this one.

## 2. Actor Flows (CDSL)

**Use cases**: None. The foundation has no route of its own any more; the routes and client operations belong to the features that add them, starting with Record Intake.

## 3. Processes / Business Logic (CDSL)

### Start the Gear

- [x] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-start-gear`

**Input**: The gear configuration (optional), the database provider and the AuthZ resolver client, both supplied by the runtime.

**Output**: A running gear with its services built and its client registered in ClientHub. The migration set and the REST routes are handed over by two separate capability hooks (see the second list below the steps).

The gear declares the dependencies `authz_resolver` and `types_registry` and the capabilities `rest` and `db`. It holds no raw database connection of its own. The runtime supplies the database provider, and all statements go through the secure ORM.

**Steps** (the order of `init` in `gear.rs`):
1. [x] - `p1` - Read the config, or use defaults when none is set. The keys are `personalization_default` (Subject Settings) and `connectors_off` (Record Intake) - `inst-start-config`
   1. [x] - `p1` - **IF** the config has an unknown key, or `connectors_off` holds a value that is not a UUID, fail `init` - `inst-start-config-fail`
2. [x] - `p1` - Ask the runtime for the database provider. Fail `init` if the gear has none - `inst-start-db`
3. [x] - `p1` - Resolve the AuthZ resolver client from ClientHub and build the policy enforcer. Fail `init` if it is missing - `inst-start-authz`
4. [x] - `p1` - Build the services: Subject Settings with its tenant default, and Record Intake with its record types, connector switch and hand-off. The settings service and the types registry are resolved from ClientHub on each request, not here. Store the intake service once. **IF** the gear is initialized a second time, fail - `inst-start-service`
5. [x] - `p1` - Register `ConstructClientV1`, backed by the intake service, in ClientHub - `inst-start-client`
6. [x] - `p1` - **RETURN** success - `inst-start-return`

**Capability hooks, called separately by the runtime**:
1. [x] - `p1` - The database capability hook returns the migration set in order: `initial_001`, `m002_subject_settings`, `m003_record_ids`. - `inst-start-migrations`
2. [x] - `p1` - The REST capability hook registers the routes on the router. It fails if the service is not initialized. - `inst-start-rest`

### Map Domain Errors to Problem Errors

- [x] `p1` - **ID**: `cpt-cf-construct-algo-gear-foundation-map-errors`

**Input**: A domain error.

**Output**: A canonical error. On a REST route it is written as an RFC 9457 Problem response.

**Steps**:
1. [x] - `p1` - Scope errors from the secure ORM become domain errors (`scope_error.rs`): denied and tenant-not-in-scope become forbidden, an invalid scope becomes internal, and a database error stays a database error. An unknown scope-error variant is logged at error level and then becomes internal - `inst-map-scope`
2. [x] - `p1` - **IF** the policy enforcer denies the request, or the constraints it returns cannot be compiled into a scope (`domain/error.rs`) - `inst-map-enforcer-denied`
   1. [x] - `p1` - **RETURN** a forbidden domain error. The domain mapping does not log it; the REST mapping logs it once (step 4) - `inst-map-enforcer-denied-return`
3. [x] - `p1` - **IF** the evaluation itself fails - `inst-map-enforcer-failed`
   1. [x] - `p1` - **RETURN** an unavailable domain error when the failure is retryable, that is when it answers with one of the platform's retryable HTTP statuses, 429, 503 or 504 (the policy enforcer reports its own timeout as service unavailable), and an internal domain error for any other failure. Its message keeps the real cause, which the REST mapping logs once (steps 5 and 6) - `inst-map-enforcer-failed-return`
4. [x] - `p1` - **IF** the error is forbidden - `inst-map-forbidden`
   1. [x] - `p1` - Log a warning. **RETURN** permission denied (403) with the reason `ACCESS_DENIED`. The internal message is not sent to the caller - `inst-map-forbidden-return`
5. [x] - `p1` - **IF** the error is unavailable - `inst-map-unavailable`
   1. [x] - `p1` - Log a warning. **RETURN** service unavailable (503), without the internal message - `inst-map-unavailable-return`
6. [x] - `p1` - **IF** the error is internal or from the database - `inst-map-internal`
   1. [x] - `p1` - Log at error level. **RETURN** internal (500), without the internal text in `detail` - `inst-map-internal-return`

A refused record is mapped by Record Intake (`cpt-cf-construct-flow-record-intake-submit`, step `inst-submit-refused`).

## 4. States (CDSL)

None.

## 5. Definitions of Done

### Gear Anatomy and Startup

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-anatomy`

The system **MUST** be built as two crates with separated layers: the SDK crate `cf-gears-construct-sdk` (client trait, models and the record types, with no infrastructure types) and the gear crate `cf-gears-construct` (API, domain and infrastructure layers). The gear **MUST** declare its dependencies and the capabilities `rest` and `db`, and **MUST** hold no raw database connection. It **MUST** start as in `cpt-cf-construct-algo-gear-foundation-start-gear`.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-start-gear`

**Verified by**: the config unit tests `an_empty_config_takes_the_defaults`, `both_keys_are_read`, `unknown_keys_are_rejected`, `a_connector_that_is_not_a_uuid_is_rejected` and `a_connector_in_another_spelling_is_the_same_connector`. `init` is not run in tests. The startup order itself, the declared dependencies, the capabilities, the two-crate layout and the absence of a raw database connection are verified by review and compilation only.

### RFC 9457 Error Behavior

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-error-mapping`

The system **MUST** answer every error as an RFC 9457 Problem with the status in `cpt-cf-construct-algo-gear-foundation-map-errors` and **MUST NOT** leak internal detail in 403, 503 or 500 answers.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-map-errors`

**Verified by**: `forbidden_maps_to_403_without_the_internal_message`, `unavailable_maps_to_503_without_leaking_detail`, `internal_maps_to_500_without_leaking_detail`, `database_maps_to_500_without_leaking_detail` (error mapping), and `map_scope_error_maps_each_variant`, `unknown_scope_error_message_is_neutral` (scope errors). No test checks the log output.

### Placeholder Note Table

- [x] `p1` - **ID**: `cpt-cf-construct-dod-gear-foundation-storage`

The migration `initial_001` **MUST** stay unchanged in the migration chain: it created the placeholder table `construct__foundation_notes`, which `m003_record_ids` drops. A database that ran `initial_001` before the placeholder was retired so migrates forward without an edited migration.

**Implements**:
- `cpt-cf-construct-algo-gear-foundation-start-gear`

**Touches**:
- DB Table: `construct__foundation_notes` (created by `initial_001`, dropped by `m003_record_ids`)

**Verified by**: `mysql_ddl_uses_binary_uuid_columns`, `postgres_and_sqlite_ddl_use_namespaced_names`, `identifiers_are_namespaced_and_within_63_bytes`, `postgres_and_sqlite_ddl_are_unchanged` (migration `initial_001`), and `the_placeholder_note_table_is_dropped` (migration `m003_record_ids`). Every test runs the whole chain on SQLite.

## 6. Acceptance Criteria

- [ ] The config rejects an unknown key and a connector that is not a UUID (`unknown_keys_are_rejected`, `a_connector_that_is_not_a_uuid_is_rejected`). `init` is not run in tests.
- [ ] The `detail` of a 500 answer does not contain the internal text, for an internal error and for a database error (`internal_maps_to_500_without_leaking_detail`, `database_maps_to_500_without_leaking_detail`).
- [ ] A 403 and a 503 answer carry no internal text (`forbidden_maps_to_403_without_the_internal_message`, `unavailable_maps_to_503_without_leaking_detail`).
- [ ] The tests named in this document pass.

## 7. Non-Functional Considerations

- **Security**: The caller comes only from the security context, and each operation has its own permission check. The code sends no internal message in a 403, 503 or 500 answer.
- **Reliability**: A bad config, a missing database provider or a missing AuthZ resolver makes `init` fail, so errors show early. A retryable failure of the AuthZ evaluation is a 503, any other a 500, and neither is an open door.
- **Rollback**: Each migration has a `down` step; none is tested.
- **Deviations from platform baselines**: None.
- **History**: The placeholder (one route, `POST /construct/v1/foundation-notes`, the client operations `create_note` and `get_note`, the note model, its text validation and its table) was retired by Record Intake, Story 5.7.2.
