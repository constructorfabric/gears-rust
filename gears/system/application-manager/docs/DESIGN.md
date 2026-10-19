---
description: "Technical design of the Application Manager gear: operations, whole-manifest validation, ordered dispatch to section plugins, and the append-only audit ledger."
---

<!-- Created: 2026-10-05 by Constructor Tech -->

# Technical Design — Application Manager

How the Application Manager runs each seed and uninstall as an operation: it makes quick synchronous checks, answers with an operation id, validates whole manifests, dispatches sections to its section plugins in priority order, and appends audit events to an append-only audit ledger. Written for gear implementers and section-plugin authors.

- [ ] `p3` - **ID**: `cpt-cf-application-manager-design-overview`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional context](#4-additional-context)
  - [4.1 Concurrent Requests for the Same Application](#41-concurrent-requests-for-the-same-application)
  - [4.2 Security Considerations](#42-security-considerations)
  - [4.3 Fault Tolerance](#43-fault-tolerance)
  - [4.4 Observability](#44-observability)
  - [4.5 Configuration](#45-configuration)
  - [4.6 Platform Deviations and Dependencies](#46-platform-deviations-and-dependencies)
  - [4.7 Areas Not Applicable](#47-areas-not-applicable)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The Application Manager is a thin orchestrating gear on the ToolKit gear model. It owns one manifest envelope, one operations table, one append-only audit ledger, and one fixed sequence of checks, audit event inserts, and plugin calls. All domain meaning lives in section plugins. Section plugins may provision data into systems outside the platform. The gear never talks to those systems.

Five decisions shape the design. The plugin set is fixed at startup ([ADR 0001](./ADR/0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)). Seed and uninstall run as operations: quick checks answer at once, and the rest runs in the background within a run window ([ADR 0003](./ADR/0003-cpt-cf-application-manager-adr-async-operations.md)). Every seed fetches live schemas and validates the whole manifest before it dispatches anything ([ADR 0002](./ADR/0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md)). Accepted and finished events are appended in the same transaction as the change they describe, and a refused event in its own transaction before the refusal answers ([ADR 0004](./ADR/0004-cpt-cf-application-manager-adr-audit-ledger.md)). Operation records are platform-wide and unrestricted, and the PDP decides platform-level permission with no tenant ([ADR 0005](./ADR/0005-cpt-cf-application-manager-adr-platform-level-access.md)).

### 1.2 Architecture Drivers

Requirements that significantly influence architecture decisions.

#### Functional Drivers

| Requirement | Design Response |
|---|---|
| `cpt-cf-application-manager-fr-manifest-envelope` | The schema validator owns the envelope schema and checks it before acceptance (`cpt-cf-application-manager-entity-manifest`). |
| `cpt-cf-application-manager-fr-application-id` | The id is checked against the `max_envelope_text_length` limit and the control-character rule, stored with a binary collation, compared as exact text, and never parsed (`cpt-cf-application-manager-entity-manifest`, `cpt-cf-application-manager-dbtable-operations`). |
| `cpt-cf-application-manager-fr-opaque-version` | The version is opaque text, passed unchanged to `apply`, and carried in the report and the audit events. |
| `cpt-cf-application-manager-fr-required-sections` | The schema validator requires exactly the key set and names each missing or unknown key (`cpt-cf-application-manager-component-schema-validator`). |
| `cpt-cf-application-manager-fr-unique-section-keys` | Startup fails on a repeated or malformed key, and the key set stays fixed until the next start (`cpt-cf-application-manager-component-startup-registration`). |
| `cpt-cf-application-manager-fr-section-priority` | The priority is the registered GTS instance priority, sorted lowest first, and startup fails on an equal priority. |
| `cpt-cf-application-manager-fr-startup-checks` | The startup component makes each check in its list once, with no retry, and a failed check stops the host (`cpt-cf-application-manager-component-startup-registration`). |
| `cpt-cf-application-manager-fr-validate-before-dispatch` | Each seed operation fetches every schema live and validates the whole manifest before any `apply` (`cpt-cf-application-manager-principle-validate-then-dispatch`). |
| `cpt-cf-application-manager-fr-plugin-served-schemas` | The `schema` call is the only source of section rules, and a compiled schema lives only for one operation or one read. |
| `cpt-cf-application-manager-fr-plugin-extensibility` | Section plugins are separate gears that register an instance of the section plugin GTS type, and the gear depends only on its SDK and the types registry. |
| `cpt-cf-application-manager-fr-async-operations` | The request orchestrator runs a synchronous phase with no plugin call, then a background operation (`cpt-cf-application-manager-component-request-orchestrator`, `cpt-cf-application-manager-entity-operation`). |
| `cpt-cf-application-manager-fr-one-operation-per-application` | A unique index allows one open operation per application id ([§4.1](#41-concurrent-requests-for-the-same-application)). |
| `cpt-cf-application-manager-fr-ordered-dispatch` | The section plugin router calls `apply` or `remove` one at a time in priority order and continues after every failure (`cpt-cf-application-manager-component-section-plugin-router`). |
| `cpt-cf-application-manager-fr-pass-through` | `apply` receives the envelope and its section as the parsed JSON value the caller sent. The gear keeps no per-section state and stores no section content. Audit ledger events record what happened, for a person who debugs a request later. The gear never reads them back, compares against them, or decides an action from them, so earlier records never drive a seed (`cpt-cf-application-manager-entity-section`, `cpt-cf-application-manager-entity-audit-event-content`). |
| `cpt-cf-application-manager-fr-plugin-owned-semantics` | The section plugin interface offers `key`, `schema`, `apply`, and `remove` only (`cpt-cf-application-manager-interface-section-plugin-api`). |
| `cpt-cf-application-manager-fr-outcome-categories` | The request orchestrator decides one category per request from the outcome table in [§3.3](#33-api-contracts). |
| `cpt-cf-application-manager-fr-outcome-precedence` | The outcome table in [§3.3](#33-api-contracts) fixes the precedence. |
| `cpt-cf-application-manager-fr-response-content` | Refusals are canonical Problems with the request id, and the operation read returns the outcome report (`cpt-cf-application-manager-entity-outcome-report`). The category determines what to fix, retry, or request, and the seeding documentation publishes that mapping from the Retriable column of the outcome table in [§3.3](#33-api-contracts). |
| `cpt-cf-application-manager-fr-bounded-plugin-calls` | The plugin call guard stops waiting at `plugin_call_timeout` and never cancels the call (`cpt-cf-application-manager-component-plugin-call-guard`). |
| `cpt-cf-application-manager-fr-dependency-failures` | Access control and operation storage are consulted before acceptance and before any plugin call, and the outcome table decides their failures. |
| `cpt-cf-application-manager-fr-uninstall` | An uninstall operation calls `remove` on every section plugin in priority order, with no check of whether the application is seeded (`cpt-cf-application-manager-seq-uninstall`). |
| `cpt-cf-application-manager-fr-discovery` | The read service lists keys from memory and serves live schemas with their schema versions (`cpt-cf-application-manager-component-read-service`). |
| `cpt-cf-application-manager-fr-operation-read` | The read service reads one operation record after a separate `read_operation` permit, and the operation sweeper purges final records after `operation_retention`. |
| `cpt-cf-application-manager-fr-authentication` | Every route is registered `.authenticated()`, and plugins receive a caller identity without the bearer token (`cpt-cf-application-manager-entity-caller-identity`). |
| `cpt-cf-application-manager-fr-authorization` | At gear level, PolicyEnforcer decides four separate actions that implement the four PRD permissions. At plugin level, a section plugin can refuse with `NotPermitted` after dispatch ([§3.3](#33-api-contracts)). |
| `cpt-cf-application-manager-fr-audit-event` | Every audit event is one insert into the append-only audit ledger, made only by the operation repository, and no other place keeps audit events (`cpt-cf-application-manager-dbtable-audit-ledger`, `cpt-cf-application-manager-component-operation-repository`). |
| `cpt-cf-application-manager-fr-audit-ordering` | The accepted event commits with the operation record before any dispatch, every final state commits its finished event with the report, and every refusal inserts its refused event before it answers (`cpt-cf-application-manager-principle-trace-before-change`). |
| `cpt-cf-application-manager-interface-seeding` | `cpt-cf-application-manager-interface-rest-api`: seed and both uninstall forms. |
| `cpt-cf-application-manager-interface-operations` | `cpt-cf-application-manager-interface-rest-api`: the operation read. |
| `cpt-cf-application-manager-interface-discovery` | `cpt-cf-application-manager-interface-rest-api`: the key list, one schema, and the combined schema. |
| `cpt-cf-application-manager-contract-section-plugin` | `cpt-cf-application-manager-interface-section-plugin-api`. |

#### NFR Allocation

This table maps non-functional requirements from PRD to specific design/architecture responses, demonstrating how quality attributes are realized.

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|---|---|---|---|---|
| `cpt-cf-application-manager-nfr-audit-completeness` | Every request has its audit events in the audit ledger | `cpt-cf-application-manager-component-request-orchestrator`, `cpt-cf-application-manager-component-operation-repository`, `cpt-cf-application-manager-dbtable-audit-ledger` | Every path ends in a refused event or in the acceptance transaction, and every final state inserts its finished event in the same transaction as the report, so no event exists for a change that did not commit. | Table-driven tests over every category and every storage failure point compare the ledger rows with the operations and refusals. |
| `cpt-cf-application-manager-nfr-platform-scope` | Platform-wide operation records behind access checks | `cpt-cf-application-manager-component-operation-repository`, PolicyEnforcer | Operation records and audit ledger rows have no tenant column and are unrestricted in the secure ORM. Operation records are reached only after a permit, and the ledger is written by the gear for every request and read by no route ([ADR 0005](./ADR/0005-cpt-cf-application-manager-adr-platform-level-access.md)). | Inspection of the entity declarations; tests that a refused decision reaches no operation record; tests that PDP decisions carry no context tenant; a test that a permit with any constraint ends as not permitted. |
| `cpt-cf-application-manager-nfr-authorized-before-plugin` | No section plugin call without a permit | `cpt-cf-application-manager-component-request-orchestrator`, `cpt-cf-application-manager-component-read-service` | The gear-level permission decision comes before the conflict read, the acceptance, and any plugin call, and the router is reachable only after a permit. | A recording test section plugin sees no call for a refused or unanswered decision. |
| `cpt-cf-application-manager-nfr-bounded-call-time` | Every plugin call ends within the configured limit | `cpt-cf-application-manager-component-plugin-call-guard` | Every plugin call goes through the guard, and `plugin_call_timeout` is required with no default ([§4.5](#45-configuration)). | Paused-clock tests with slow test plugins, and configuration tests. |
| `cpt-cf-application-manager-nfr-bounded-request-time` | Quick answer, and every operation final within its run window | `cpt-cf-application-manager-component-request-orchestrator`, `cpt-cf-application-manager-component-operation-sweeper` | The synchronous phase has a checked time budget, and the interruption step ends every operation at its run deadline ([§4.5](#45-configuration)). | Paused-clock tests with test plugins that always reach the limit. |
| `cpt-cf-application-manager-nfr-no-partial-on-invalid` | No dispatch for invalid manifest, temporarily unavailable, or conflict | `cpt-cf-application-manager-component-request-orchestrator` | Dispatch is reachable only after acceptance, usable schemas, and a valid manifest. | Counting tests assert no `apply` or `remove` call for those categories. |
| `cpt-cf-application-manager-nfr-availability` | Platform availability baseline; seeding pauses only while a restart runs | `cpt-cf-application-manager-component-startup-registration`, `cpt-cf-application-manager-topology-replicas` | Audit events are written to the platform database, and after startup readiness depends only on the published startup result. Seeding resumes once the restart completes. | Readiness tests before and after startup, and restart tests that seed again once the gear is ready. |
| `cpt-cf-application-manager-nfr-operational-signals` | Operator signals and counts | `cpt-cf-application-manager-component-operation-repository`, `cpt-cf-application-manager-component-plugin-call-guard`, `cpt-cf-application-manager-component-startup-registration` | Platform telemetry carries the counts and signal conditions of [§4.4](#44-observability). | Tests read the counts from an in-memory exporter and assert the log events. |

#### Key ADRs

| ADR ID | Decision Summary |
|---|---|
| `cpt-cf-application-manager-adr-section-plugin-fan-out` | Every section plugin takes part. Plugins are discovered once at startup into a key map and a priority order. |
| `cpt-cf-application-manager-adr-plugin-served-section-schemas` | Section plugins serve their own live schemas, fetched on every seed, and the whole manifest is validated before anything is dispatched. |
| `cpt-cf-application-manager-adr-async-operations` | Seed and uninstall run as background operations: a bounded synchronous phase answers with an operation id, the run ends within its run window, and plugin calls are abandoned at their time limit without cancellation. |
| `cpt-cf-application-manager-adr-audit-ledger` | Accepted and finished events are appended in the same transaction as the change they describe, and a refused event in its own transaction before the refusal answers. |
| `cpt-cf-application-manager-adr-platform-level-access` | The PDP decides platform-level permission with no tenant, and operation records and audit ledger rows are unrestricted. |

### 1.3 Architecture Layers

Solid arrows are runtime calls. Dotted arrows are compile-time relationships.

```mermaid
flowchart TB
    GW["API gateway"] --> API["api/rest"] --> DOM["domain"]
    INF["infra"] -.->|"implements domain ports"| DOM
    INF --> SP["Section plugins"]
    INF --> PDP["Platform access control"]
    INF --> DB[("Platform database: operations and audit ledger")]
    INF -->|"startup only"| TR["Types registry"]
    SP -. implements .-> SDK["application-manager-sdk"]
    DOM -. uses .-> SDK
```

- [ ] `p3` - **ID**: `cpt-cf-application-manager-tech-layers`

The gear uses the platform layer model ([gear layout and SDK pattern](../../../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md)). The SDK crate is the only crate that plugin gears depend on, and REST is the only consumer surface.

| Layer | Responsibility | Technology |
|---|---|---|
| API (`api/rest/`) | Routes, DTOs, Problem bodies for refusals, the not-ready gate | Axum handlers through OperationBuilder; canonical errors (RFC 9457) |
| Domain (`domain/`) | Orchestration, the operation lifecycle, reads, validation, dispatch order, outcome ranking, report and event assembly; owns the ports | `#[domain_model]` services; JSON Schema Draft 2020-12 through `jsonschema`; SHA-256 |
| Infrastructure (`infra/`) | Call guard and ClientHub adapters, policy adapter, operation repository with its audit ledger inserts, operation sweeper, startup, telemetry | ClientHub; types registry; PolicyEnforcer; SeaORM through SecureConn; OpenTelemetry |
| SDK (`application-manager-sdk`) | Section plugin interface, models, error kinds, GTS types | Rust traits; GTS |

## 2. Principles & Constraints

### 2.1 Design Principles

When principles conflict, the earlier one in this list wins: Trace Before Change; Honest, Deterministic Outcomes; Validate Everything, Then Dispatch; Bounded Waiting, No Cancellation; Pass Through, Never Interpret; Plugin Set Fixed at Startup; Minimal State.

#### Pass Through, Never Interpret

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-pass-through`

The gear checks shape against schemas it does not own and moves each section unchanged to its owner. It never gives meaning to versions, re-seeds, schema versions, or section content.

**ADRs**: `cpt-cf-application-manager-adr-plugin-served-section-schemas`

#### Plugin Set Fixed at Startup

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-startup-fixed-plugin-set`

The section plugin set, its keys, and its priority order are built once at startup and then never change. Key and priority conflicts show up at deploy time, and an operation never sees the plugin set change.

**ADRs**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

#### Validate Everything, Then Dispatch

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-validate-then-dispatch`

Dispatch starts only after every earlier step has passed: permission, storage, the conflict check, the envelope check, the committed operation with its accepted event, every live schema, and the whole-manifest validation. Sections then go out one at a time in priority order, dispatch continues after a section plugin fails, and rollback stays with the section plugins. Seed validation and the discovery combined schema use one composition rule, so a manifest built from discovery passes validation.

**ADRs**: `cpt-cf-application-manager-adr-plugin-served-section-schemas`

#### Trace Before Change

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-trace-before-change`

No section plugin receives anything until the operation record and its accepted event have committed together. Each final state commits with its finished event, and each refusal records its refused event before it answers.

**ADRs**: `cpt-cf-application-manager-adr-audit-ledger`

#### Honest, Deterministic Outcomes

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-honest-outcomes`

Every seed and uninstall ends in exactly one outcome category, chosen by the outcome table in §3.3. The gear fails closed, never claims not permitted or conflict when it could not check, and never reports a partial failure as success. Every write to an operation is guarded, so an operation has exactly one final state.

#### Bounded Waiting, No Cancellation

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-bounded-waiting`

The gear stops waiting for a plugin call at the time limit but never cancels or undoes work a plugin has started. Each seed and uninstall runs as a request task detached from the HTTP request, so a caller disconnect cuts no step short.

#### Minimal State

- [ ] `p2` - **ID**: `cpt-cf-application-manager-principle-minimal-state`

The gear keeps only operation records, purged after `operation_retention`, and the append-only audit ledger. Neither holds a manifest, section content, or a schema. The ledger is a record of what happened, kept for people who debug a request later, and the gear never reads it back, compares against it, or decides from it.

**ADRs**: `cpt-cf-application-manager-adr-audit-ledger`, `cpt-cf-application-manager-adr-platform-level-access`

### 2.2 Constraints

#### ToolKit Gear and Plugin Model

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-toolkit-plugin-model`

The gear follows the ToolKit SDK, domain, API, and infrastructure split. The section plugin type is a GTS type, instances are listed from the types registry, and clients are looked up in ClientHub by exact GTS id, because ClientHub cannot enumerate scoped clients ([ClientHub and plugins](../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md)).

**ADRs**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

#### Platform Authentication and Policy Enforcement

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-platform-auth`

Authentication belongs to the platform pipeline, and authorization uses PolicyEnforcer ([AuthN, AuthZ and secure ORM](../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md)). The gear holds no credentials and never forwards caller credentials to plugins.

**ADRs**: `cpt-cf-application-manager-adr-platform-level-access`

#### Secure ORM for Operations and the Audit Ledger

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-secure-orm`

All access to operations and to the audit ledger goes through SecureConn, with both entities declared unrestricted and no tenant column ([database patterns](../../../../docs/toolkit_unified_system/11_database_patterns.md)). Raw SQL appears only in migrations.

**ADRs**: `cpt-cf-application-manager-adr-platform-level-access`

#### Canonical Error Model

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-canonical-errors`

Every error response is a standard canonical Problem built from a canonical error category ([canonical error system](../../../../docs/arch/errors/DESIGN.md)). The gear adds no extension member, and the outcome of an accepted operation is data in its report, not an error.

#### No Stored Manifest Content

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-no-manifest-content`

Manifest content lives only in memory, while its request task or a detached plugin call holds it. No parsed manifest and no section body appears in operation records, audit ledger rows, logs, traces, metrics, or error text. Of the manifest, audit ledger rows carry only the envelope id and version within the bounds of §3.1, section keys of the key set, counts, and rule tokens, with no problem locations. Operation records and logs carry only the envelope id and version within the bounds of §3.1 and §4.4, section keys, and problem locations. Errors name locations and section keys, never values.

**ADRs**: `cpt-cf-application-manager-adr-audit-ledger`

#### Restart for Plugin Changes

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-restart-for-plugin-change`

Adding, removing, or re-prioritizing a section plugin takes effect only after a restart.

**ADRs**: `cpt-cf-application-manager-adr-section-plugin-fan-out`

#### Configured Time Limit Only

- [ ] `p2` - **ID**: `cpt-cf-application-manager-constraint-configured-time-limit`

The plugin-call time limit comes only from configuration, equals the configured value exactly, and is the same for every call kind.

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust types marked `#[domain_model]`; GTS types for the section plugin type, the authorization resource type, the outcome report, and the section schemas; JSON Schema Draft 2020-12.

**Location**: shared models in `application-manager-sdk/src/models.rs`, GTS definitions in `application-manager-sdk/src/gts.rs`, and domain services in `application-manager/src/domain/`.

**Core Entities**:

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-manifest`

**Manifest.** A JSON object with exactly `application`, the envelope, and `sections`, an object with one member per section key. The envelope holds exactly `id` and `version`. Each is non-empty text of at most `max_envelope_text_length` bytes with no control character (Unicode category Cc, which includes NUL). Uninstall applies the same rules to its id, with the same `max_envelope_text_length` limit. `APPLICATION_ID_MAX_BYTES` is only the column size and the upper bound of that setting.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-section`

**Section.** The JSON value under one key of `sections`, passed unchanged to the owning section plugin's `apply` together with the envelope.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-outcome-category`

**Outcome category and section outcome.** The request-level category is one of the seven PRD categories, as the outcome table in §3.3 assigns it. A section outcome is `succeeded`, `rejected`, `not_permitted`, or one of four internal failures: `internal_failure`; `timed_out`, whose effect is unknown because the call may still complete; `not_sent`, which had no effect because the in-flight cap refused the call; and `not_dispatched`, where the request task failed before the call. `not_sent` and `not_dispatched` are failures of the system itself, which the PRD's internal failure covers. `not_ready` and `at_capacity` are the gear's own temporary inability to take a request. This design maps them to the PRD's temporarily unavailable, because the caller can retry and nothing is dispatched.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-outcome-report`

**Outcome report.** The PRD's operation report. The gear writes it into the operation record as the operation runs, and the operation read returns it once the operation is final. It validates against the outcome report GTS type.

| Member | Content |
|---|---|
| `operation_id` | The operation id |
| `action` | `seed` or `uninstall` |
| `category` | The outcome category of a completed operation; absent while open and for an interrupted operation |
| `interrupted` | True when the operation ended at its run deadline |
| `application` | Seed: `id` and `version`. Uninstall: `id`. |
| `sections` | One item per section plugin with a recorded outcome, in priority order: `key`, `outcome`, `schema_version` for a validated seed section, and the plugin's `detail`, cut at `PLUGIN_DETAIL_MAX_BYTES`, and optional `location` for `rejected` and `not_permitted` |
| `problems` | Section validation problems: `location` (JSON Pointer), `rule` (a JSON Schema keyword, `missing_section`, or `unknown_section`), and `message` |
| `unavailable` | Unavailable items for a seed whose section schemas could not be used |

An unavailable item has a `dependency` (`access_control`, `operation_storage`, `section_schema`, or `application_manager`), a `key` for a section schema, and a `reason` (`unreachable`, `timed_out`, `unusable`, `not_sent`, `not_ready`, or `at_capacity`). Refused events use the same shape.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-operation`

**Operation record.** One per accepted seed or uninstall: the operation id, application id, action, status, category, report so far, accepted, started, and finished times, run deadline, and finished event id. It never holds manifest content or caller identity, and it has no tenant column.

```mermaid
stateDiagram-v2
    [*] --> Accepted: acceptance transaction with the accepted event
    Accepted --> Running: mark running
    Running --> Completed: final transaction with the finished event
    Accepted --> Interrupted: interruption step after the run deadline
    Running --> Interrupted: interruption step after the run deadline
    Completed --> [*]
    Interrupted --> [*]
```

The PRD names the operation states, and this diagram defines the transitions between them.

- **Run deadline.** Set by the acceptance transaction, anchored at the acceptance commit time and sized by the plugin count, as [§4.5](#45-configuration) states. No plugin call starts after it.
- **Status guard.** Every write to an open operation matches only an open record before its run deadline. The interruption step matches only an open record past it. So the first final write wins.
- **Interruption step.** One transaction sets status interrupted, keeps the section outcomes so far, and inserts the finished event with the stored event id. The sweeper, the operation read, and the acceptance transaction run it when they meet an expired open operation.
- **Supervisor completion.** When a request task fails after acceptance, the supervisor first marks the operation running if it is still accepted, then completes it as internal failure in the final transaction, with `not_dispatched` for plugins not yet called. So the operation goes Accepted, Running, Completed.
- **Retention.** The sweeper purges a final record once `operation_retention` has passed after its finish time.

A completed or interrupted operation frees its application.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-caller-identity`

**Caller identity.** The subject id, subject type, subject tenant id, and token scopes, copied from the request's `SecurityContext`, with no bearer token. Every section plugin call receives it, and accepted and refused events carry it. The SDK has a constructor that turns it back into a token-less `SecurityContext` for a plugin's own PolicyEnforcer check.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-audit-event-content`

**Audit event content.** A request has one refused event, or one accepted event followed by one finished event. The one exception is an acceptance that timed out after it committed: its request has the accepted event, the refused event, and later the finished event. Operations that end without dispatch also get a finished event. Each event id is a time-ordered UUID assigned at receive, and it is the primary key of the event's audit ledger row.

| Field | Accepted | Finished | Refused |
|---|---|---|---|
| Event id, kind, action, write time | Yes | Yes | Yes |
| Correlation id | `operation_id` | `operation_id` | `request_id` |
| Application (the envelope fields) | Seed: id and version. Uninstall: id. | As accepted | Id and version as sent, when within the limits. An id or version outside them appears only as its byte length and SHA-256 digest. |
| Section keys present (seed) | Yes | No; joined through the accepted event | Yes, when `sections` is an object |
| Caller identity and receive time | Yes | No; joined through the accepted event | Yes |
| Result | None | Category or interrupted marker; the problem count and rule tokens of the section problems; the section outcome per key of the key set, with schema versions; unavailable items (dependency, key-set key, reason) | Category, the problem count and rule tokens of the envelope problems, busy operation id, or unavailable items |

The section keys present are the keys of the manifest's `sections` that belong to the section key set, in priority order, plus the count of any other keys. A key outside the section key set appears in ledger rows only as a count. So no stored field of an event carries an id or version outside the limits.

For problems, an event carries only the problem count and the rule tokens, with no location and no message. A section outcome in an event carries the section outcome of one key of the key set, with no plugin `detail` and no `location`. The full problem list with locations and messages, and the plugin details and locations, stay in the outcome report, inside the operation record that the sweeper purges after `operation_retention`. So a ledger row carries no caller-supplied manifest content beyond the envelope id and version within the limits, or their byte length and digest outside them.

An event holds no parsed manifest and no section body. The audit ledger is a record of what happened, written for people who debug a request later. The gear never reads it back, compares against it, or decides an action from it, so earlier records never drive a seed (`cpt-cf-application-manager-fr-pass-through`).

One time-ordered UUID per request is its request id: the PRD's id that links a refusal or a report to its audit events. For an accepted request it is the operation id, and the correlation id of its accepted and finished events. For a refused request it is the correlation id of its refused event.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-entity-schema-version`

**Schema version.** Opaque text of 1 to `SCHEMA_VERSION_MAX_BYTES` bytes that a section plugin returns with each schema. The gear records it as sent, in the finished event, the report, and discovery, and never compares it. A missing, empty, or too long version makes the schema `unusable`.

| Entity | Description | Schema |
|---|---|---|
| Operation | Status, report, and times of one accepted request | `cpt-cf-application-manager-dbtable-operations` |
| Manifest | Envelope plus sections | Envelope schema owned by the gear; combined schema from discovery |
| Section | One section plugin's part of a manifest | Served live by its section plugin |
| OutcomeCategory, SectionOutcome | Request and per-section results | `application-manager-sdk/src/models.rs` |
| OutcomeReport | Result of an operation | GTS type `gts.cf.core.application_manager.outcome_report.v1~` |
| CallerIdentity | Identity passed to plugins | `application-manager-sdk/src/models.rs` |
| AuditEventContent | Accepted, finished, and refused events | `application-manager/src/domain/`, stored as the `content` of an audit ledger row (`cpt-cf-application-manager-dbtable-audit-ledger`) |
| SchemaVersion | Version of a section schema | Part of the section schema that `schema` returns |

**Relationships**:
- Manifest → Section: one section per section key.
- Operation → OutcomeReport: one report, final once the operation is final.
- Operation → AuditEventContent: accepted and finished events carry the operation id.
- OutcomeReport → SectionOutcome: one per section plugin with a recorded outcome.

### 3.2 Component Model

Domain components call ports, and infrastructure components implement them. Each component below states why it exists, what it owns, what it does not do, and its related components.

```mermaid
graph LR
    REST[REST API] --> ORC[Request orchestrator]
    REST --> RS[Read service]
    ORC --> VAL[Schema validator]
    RS --> VAL
    ORC --> SPR[Section plugin router]
    RS --> SPR
    ORC --> REP[Operation repository]
    RS --> REP
    SWP[Operation sweeper] --> REP
    SPR --> GUARD[Plugin call guard]
    GUARD --> SP[Section plugins]
    REST --> REP
    ORC --> PDP[Platform access control]
    RS --> PDP
    REP -->|"operations and audit ledger"| DB[(Platform database)]
    START[Startup and registration] -->|"publishes plugin set"| SPR
    START -->|"starts and stops"| SWP
    START -->|"lists instances"| TR[Types registry]
    START -->|"key calls"| GUARD
    REST -->|"reads readiness and stopping flag"| START
```

#### REST API

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-rest-api`

**Why**: It is the only caller-facing surface. It turns HTTP requests into domain calls and domain results into the answers of §3.3.

**Scope**: It registers the routes through OperationBuilder and reads the seed body as a parsed JSON value. It assigns the request id, the event ids, and the receive time of every seed and uninstall before the gate, so a gate refusal has them too. It owns the not-ready gate and the request-task cap check, which read the published startup result and the stopping flag: a seed or uninstall that arrives before startup ends, after `stop` begins, or with no free permit is refused with a refused event (reason `not_ready` or `at_capacity`), and a read is refused with no event.

**Boundaries**: It does not authenticate or authorize. The only category it decides is the rank 1 refusal at the gate, and it records that refusal's refused event through the operation store port. All other categories belong to the request orchestrator. Requests that the platform rejects, such as malformed JSON or throttled requests, never reach it.

**Related**: `cpt-cf-application-manager-component-request-orchestrator` (starts its request tasks), `cpt-cf-application-manager-component-read-service` (reads), `cpt-cf-application-manager-component-startup-registration` (startup result and stopping flag).

#### Request Orchestrator

- [ ] `p1` - **ID**: `cpt-cf-application-manager-component-request-orchestrator`

**Why**: Precedence, acceptance, audit ordering, and dispatch must run as one sequence on every path. One component owns it, so no branch can skip a step.

**Scope**: It runs each seed and uninstall as a supervised request task. A supervisor inside the orchestrator watches the task and, when it fails after acceptance, performs the supervisor completion of §3.1. It decides the category by the outcome table in §3.3. Its invariants:

- The synchronous phase makes no plugin call, and every refusal records its refused event before it answers.
- No `apply` or `remove` before a permit and a committed operation record with its accepted event.
- Every write to an operation is guarded, and no plugin call starts after the run deadline or after the operation is final.
- The id is taken once from the single parse of the body, and every use carries that value or none.
- The request task completes even when the HTTP request is dropped.

**Boundaries**: It holds no plugin set and no schema logic, and it writes no table itself. It never cancels, undoes, or resumes work, and it does not sweep or purge.

**Related**: `cpt-cf-application-manager-component-operation-repository` (operations and audit events), `cpt-cf-application-manager-component-schema-validator` (validation), `cpt-cf-application-manager-component-section-plugin-router` (schemas and dispatch).

#### Read Service

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-read-service`

**Why**: Discovery and operation reads record no audit event and decide no outcome category. Keeping them in their own component keeps the read paths free of audit and precedence logic.

**Scope**: After a `read_schema` or `read_operation` permit, it lists keys from memory, fetches one or every live schema with its schema version, or reads one operation record. When the record is open past its run deadline, it runs the interruption step first.

**Boundaries**: It records no audit event, caches no schema, and makes no plugin call for the key list or the operation read. It never returns the caller subject, a manifest, or section content, and it never reads the audit ledger.

**Related**: `cpt-cf-application-manager-component-operation-repository` (operation reads), `cpt-cf-application-manager-component-section-plugin-router` (live schemas), `cpt-cf-application-manager-component-schema-validator` (combined schema).

#### Schema Validator

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-schema-validator`

**Why**: Seed validation and the combined schema must follow one rule, so that discovery is a reliable contract.

**Scope**: It owns the envelope schema, composes the combined schema with each section schema under its key and the gear-owned `$id` `urn:cf:application-manager:section:<key>`, and collects every problem with its JSON Pointer location. It compiles served schemas as Draft 2020-12 off the async executor, with the linear-time `regex` engine and the limits `SCHEMA_REGEX_SIZE_LIMIT` and `SCHEMA_REGEX_DFA_SIZE_LIMIT`; another dialect, a reference outside the served document, a size above `SERVED_SCHEMA_MAX_BYTES`, a bad schema version, or a compile failure makes a schema `unusable`.

**Boundaries**: It resolves nothing outside the served document and keeps no compiled schema beyond one operation or read. Its messages come from schema-side constraints, never from manifest values.

**Related**: `cpt-cf-application-manager-component-request-orchestrator` and `cpt-cf-application-manager-component-read-service` (callers).

#### Section Plugin Router

- [ ] `p1` - **ID**: `cpt-cf-application-manager-component-section-plugin-router`

**Why**: Routing depends only on the startup-built section plugin set, so a manifest can never select or add a plugin.

**Scope**: It holds the plugin set in priority order, fetches all schemas concurrently, and calls `apply` or `remove` one at a time when the orchestrator asks. It turns each result into a section outcome and passes the caller identity and the operation id to every call.

**Boundaries**: It never consults the types registry after startup and never retries a call. It does not record outcomes or check the run deadline.

**Related**: `cpt-cf-application-manager-component-plugin-call-guard` (wraps every call), `cpt-cf-application-manager-component-startup-registration` (provides the plugin set).

#### Plugin Call Guard

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-plugin-call-guard`

**Why**: In an async runtime, dropping a timed-out call would cancel it. One guard makes "stop waiting, never cancel" the same for every call kind.

**Scope**: It runs each plugin call detached, waits up to `plugin_call_timeout`, and logs a late result by metadata only. It enforces `plugin_max_in_flight` per plugin instance, so a call over the cap is never sent (`not_sent`), and it turns panics and transport errors into internal errors.

**Boundaries**: It does not decide what a timeout means for its caller, and the limit it waits for equals the configured value exactly.

**Related**: `cpt-cf-application-manager-component-section-plugin-router` (section plugin calls), `cpt-cf-application-manager-component-startup-registration` (`key` calls).

#### Operation Repository

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-operation-repository`

**Why**: It is the only writer of operation records and of the audit ledger. It enforces the secure-ORM rules and owns the status guard.

**Scope**: It runs the storage step, the acceptance transaction, the guarded updates, the final transaction, the interruption step, reads, the sweep selection, the purge, and refused-event inserts, each bounded by `storage_call_timeout`. Every audit event is one insert into the audit ledger at the write points of §3.7. It reports "storage cannot answer", "index violated", and "operation final" as distinct results.

**Boundaries**: It decides no category and runs no schedule. It never reads, updates, or deletes an audit ledger row.

**Related**: `cpt-cf-application-manager-component-request-orchestrator` (main caller), `cpt-cf-application-manager-component-operation-sweeper` (interruptions and purges).

#### Operation Sweeper

- [ ] `p2` - **ID**: `cpt-cf-application-manager-component-operation-sweeper`

**Why**: A request task can stop mid-operation, yet every operation must reach a final state, and final records must not grow without bound.

**Scope**: On every replica, every `SWEEP_INTERVAL`, it runs the interruption step for up to `MAINTENANCE_BATCH_SIZE` expired open operations. Every `PURGE_INTERVAL` it deletes final records older than `operation_retention`, in batches.

**Boundaries**: It makes no plugin call, never resumes or completes an operation, never deletes an open record, and never purges the audit ledger.

**Related**: `cpt-cf-application-manager-component-operation-repository` (runs its steps).

#### Startup and Registration

- [ ] `p1` - **ID**: `cpt-cf-application-manager-component-startup-registration`

**Why**: Every configuration and plugin-set problem must stop the gear at deploy time.

**Scope**: In `init` it validates the settings and registers the section plugin GTS type. A hand-implemented `start` makes each check once, with no retry, and then publishes the startup result. Its `key` calls go through the plugin call guard, so a `key` call that does not answer within `plugin_call_timeout` fails startup with `section_key_unavailable`. A failed check emits a startup failure event that names one of these reasons, and the host exits with a non-zero status. Each reason belongs to one problem in `cpt-cf-application-manager-fr-startup-checks`:

- the plugin registration facility cannot answer: `types_registry_unreachable`, when the instance listing fails or does not answer in time; it is never read as empty;
- a section plugin's key or priority cannot be read: `priority_unreadable`, `plugin_client_unresolved` (no client to ask for the key), or `section_key_unavailable` (a `key` call fails);
- a key is invalid: `invalid_section_key`, when a key breaks the SDK token pattern;
- two section plugins share a key: `duplicate_section_key`;
- two section plugins share a priority: `equal_priority`;
- a setting is missing or invalid: `invalid_setting` (in `init`), which also covers a broken time budget or margin floor of §4.5.

An answering types registry that lists no section plugins gives an empty key set.

**Boundaries**: It never runs again after startup and never selects plugin instances by vendor.

**Related**: `cpt-cf-application-manager-component-section-plugin-router` (receives the plugin set), `cpt-cf-application-manager-component-operation-sweeper` (started and stopped by it).

#### Ports and Composition

| Port | Operations | Implemented by |
|---|---|---|
| Policy decision | Decide an action: permit, refuse, or cannot answer | Policy adapter over PolicyEnforcer |
| Operation store | Storage step, acceptance, guarded writes, interruption, reads, sweep, purge, refused-event insert; every audit ledger insert | Operation repository |
| Section plugin | `key`, `schema`, `apply`, `remove` | Call guard and ClientHub adapter |
| Startup | Publish the startup result once; set the stopping flag | Startup component |
| Clock and id source | Times and time-ordered ids | System clock and UUID generator |

`gear.rs` is the composition root: it returns the migrations, builds the domain services in `init`, and lets the startup component start and stop the background work. Tests replace every port with a double.

### 3.3 API Contracts

This section realizes `cpt-cf-application-manager-interface-seeding`, `cpt-cf-application-manager-interface-operations`, `cpt-cf-application-manager-interface-discovery`, and `cpt-cf-application-manager-contract-section-plugin`.

- [ ] `p2` - **ID**: `cpt-cf-application-manager-interface-rest-api`

- **Contracts**: `cpt-cf-application-manager-interface-seeding`, `cpt-cf-application-manager-interface-operations`, `cpt-cf-application-manager-interface-discovery`
- **Technology**: REST over HTTP with JSON bodies, canonical Problem errors (RFC 9457), and OpenAPI generated from OperationBuilder ([REST OperationBuilder](../../../../docs/toolkit_unified_system/04_rest_operation_builder.md))
- **Location**: the platform's generated OpenAPI document

**Endpoints Overview**:

| Method | Path | Description | Stability |
|---|---|---|---|
| `POST` | `/application-manager/v1/applications` | Seed: submit a manifest. Permission `seed`. | unstable |
| `DELETE` | `/application-manager/v1/applications/{application_id}` | Uninstall by id in one percent-encoded path segment. Permission `uninstall`. | unstable |
| `POST` | `/application-manager/v1/applications:uninstall` | Uninstall with `application_id` in the body, for ids a path segment cannot carry. Permission `uninstall`. | unstable |
| `GET` | `/application-manager/v1/operations/{operation_id}` | Read one operation. Permission `read_operation`. | unstable |
| `GET` | `/application-manager/v1/section-keys` | List section keys in priority order. Permission `read_schema`. | unstable |
| `GET` | `/application-manager/v1/section-keys/{section_key}/schema` | Read one live schema and its schema version. Permission `read_schema`. | unstable |
| `GET` | `/application-manager/v1/manifest-schema` | Read the live combined schema with every schema version. Permission `read_schema`. | unstable |

**Operation registration.** Each route is registered through OperationBuilder with an `application_manager.*` operation id, `.authenticated()`, `.no_license_required()`, its schemas, a tag, and its statuses. Seed, both uninstall forms, the operation read, and the combined-schema read are bound to gateway rate-limit and in-flight zones. The seeding documentation asks callers to keep secrets out of manifests, because the gear does not detect or remove them, and tells callers to read the operation for the result. The category of a refusal or a report determines what the caller fixes, retries, or requests. The seeding documentation publishes that mapping from the Retriable column of the outcome table, which, with the request id, the section outcomes, and the problems, gives the content `cpt-cf-application-manager-fr-response-content` requires.

**Answers.** An accepted request gets 202 with `operation_id`, `status` `accepted`, and a `Location` header with the operation read path. The operation read gets 200 with the id, action, application id and version, status, times, run deadline, and, once final, the outcome report.

**Outcome table.** This is the one normative statement of outcome precedence, placement, and HTTP mapping. The first matching rank decides.

| Rank | Outcome category | Decided in | Canonical error category | HTTP | Retriable |
|---|---|---|---|---|---|
| 1 | Temporarily unavailable: not ready, request-task cap, access control cannot answer, or operation storage cannot answer for a permitted caller | Synchronous refusal | `service_unavailable` | 503 | Yes, after the retry delay |
| 2 | Not permitted by platform access control | Synchronous refusal | `permission_denied` | 403 | No; obtain the permission |
| 3 | Conflict: an open operation of the application before its run deadline | Synchronous refusal | `aborted` | 409 | Yes, once the named operation is final |
| 4 | Invalid manifest: an envelope problem, or an uninstall id outside the limits | Synchronous refusal | `invalid_argument` | 400 | No; fix the request |
| Acceptance | Temporarily unavailable: the operation and its accepted event cannot commit, or the winner of a lost index race cannot be read | Synchronous refusal | `service_unavailable` | 503 | Yes, after the retry delay |
| Gear defect | Internal failure: the request task fails before it answers | Synchronous refusal | `internal` | 500 | Yes, seed or uninstall again |
| 5 | Temporarily unavailable: a section schema is not served or is unusable (seed) | Operation report | — | — | Yes, with a new request; for `unusable`, after the plugin is fixed |
| 6 | Not permitted by a section plugin's `schema` (seed) | Operation report | — | — | No; obtain the plugin's permission |
| 7 | Invalid manifest: a section problem (seed) | Operation report | — | — | No; fix the named sections |
| After dispatch 1 | Not permitted by a section plugin | Operation report | — | — | No; obtain the plugin's permission |
| After dispatch 2 | Plugin rejected | Operation report | — | — | No; fix the named sections |
| After dispatch 3 | Internal failure: a section plugin fails or times out, a call is `not_sent`, or the supervisor completes the operation after a task failure (`not_dispatched`) | Operation report | — | — | Yes, seed or uninstall again |
| — | Succeeded | Operation report | — | — | Not applicable |

Storage is consulted only for a permitted caller, so a refused caller is not permitted even while storage is down. A seed with envelope and section problems ends at rank 4, because sections are checked only inside the operation. An interrupted operation has no category, and requests the platform rejects get none. Both internal failure rows are the PRD's internal failure: a section plugin failed or exceeded the time limit, or the system itself failed. A gear defect stops the request where it happens, so it is not ranked against the other rows: the caller gets the platform's generic 500 `internal`, and the refused event records internal failure.

**Refusal bodies.** Every synchronous refusal is a standard canonical Problem with no extension member:

- the `detail` states the request id, which links the refusal to its refused event, and the platform fills `trace_id`;
- temporarily unavailable carries the retry delay `REFUSAL_RETRY_AFTER_SECONDS`; the dependency and reason go only to the refused event, logs, and metrics;
- not permitted names the refused action;
- conflict has the `aborted` reason `application_busy`, and the `detail` names the open operation's id;
- invalid manifest lists each problem as a field violation: location, rule token (a JSON Schema keyword, `max_length`, or `control_character`), and message.

Reads end as not permitted, not found, or temporarily unavailable, and the seed and uninstall outcome categories do not apply to them: 403 `permission_denied`, 404 `not_found` for an unknown key or an unknown, malformed, or purged operation id, and 503 with the retry delay.

**Safe wire errors.** Plugin `Internal` text, PDP internals, and storage errors are logged under the trace id and never returned. A plugin's `Rejected` and `NotPermitted` detail is returned cut at `PLUGIN_DETAIL_MAX_BYTES`, and validation messages never contain manifest values.

**Authorization.** Two levels decide whether a caller may act.

- **Gear level.** PolicyEnforcer evaluates the resource type `gts.cf.core.application_manager.application.v1~` for four actions, which implement the four PRD permissions. The permission is platform-level, and every decision is requested with no tenant ([ADR 0005](./ADR/0005-cpt-cf-application-manager-adr-platform-level-access.md)), so policy authors grant each action as a platform-level permission. Seed and uninstall send the application id as the resource property `application_id` when it is within the limits. Each decision is bounded by `pdp_call_timeout`. `EvaluationFailed` means access control cannot answer. `Denied`, `CompileFailed`, and a permit with any constraint mean not permitted (rank 2).
- **Plugin level.** A section plugin that needs an extra permission of its own decides from the caller identity and refuses with `NotPermitted`. From `apply` or `remove`, that gives the `not_permitted` section outcome and the rank After dispatch 1. From `schema`, it gives rank 6 in a seed, with nothing dispatched, and 403 `permission_denied` in a read.

`cpt-cf-application-manager-nfr-authorized-before-plugin` concerns the gear level only.

| Action | PRD permission | Operations | What a permit allows | Intended holders |
|---|---|---|---|---|
| `seed` | Seed | Seed | Have every section plugin apply its section | Each application's deployment pipeline, granted narrowly |
| `uninstall` | Uninstall | Both uninstall forms | Have every section plugin remove an application | Operators, granted apart from `seed` |
| `read_operation` | Read operations | Read an operation | Read the status and report of any operation | Seeding callers and operators |
| `read_schema` | Read schemas | All three discovery reads | Read keys, live schemas, and schema versions | Seeding callers and application teams |

- [ ] `p2` - **ID**: `cpt-cf-application-manager-interface-section-plugin-api`

- **Contracts**: `cpt-cf-application-manager-contract-section-plugin`
- **Technology**: ToolKit plugin model; GTS type `gts.cf.toolkit.plugins.plugin.v1~cf.core.application_manager_section.plugin.v1~`; scoped ClientHub client
- **Location**: `application-manager-sdk/src/section_plugin_api.rs`

| Operation | Called | Receives | Returns | On timeout |
|---|---|---|---|---|
| `key` | Once, at `start` | Nothing | One key in the SDK token pattern: lower-case letters, digits, `-`, `_`, `.` | Startup fails |
| `schema` | In each seed operation and each schema read | Caller identity | A self-contained Draft 2020-12 schema of the plugin's GTS type, with its schema version | Seed: rank 5. Read: 503. |
| `apply` | In a seed operation, in priority order | Caller identity, operation id, envelope, its section | Success or an error kind | `timed_out`; dispatch continues |
| `remove` | In an uninstall operation, in priority order | Caller identity, operation id, application id | Success or an error kind | `timed_out`; dispatch continues |

The priority is the registered GTS instance priority, read at startup. A call refused by the in-flight cap gives `not_sent`.

| Error kind | Carries | Section outcome |
|---|---|---|
| `Rejected` | Caller-facing detail and an optional JSON Pointer location | `rejected` |
| `NotPermitted` | Caller-facing detail | `not_permitted` |
| `Internal` | Diagnostic detail, logged only | `internal_failure` |

**Schema call results.** The `schema` call is classified apart from dispatch:

- `NotPermitted` gives not permitted (plugin-level authorization): rank 6 in a seed, and 403 `permission_denied` in a read.
- Any other error kind, or a transport error, gives the unavailable reason `unreachable`.
- A call that does not answer in time gives the reason `timed_out`.
- A schema that cannot be used gives the reason `unusable`, as the schema validator states.
- A call refused by the in-flight cap gives the reason `not_sent`.

A schema read uses the seed's precedence: when one plugin's call gives an unavailable reason and another's gives `NotPermitted`, the read answers 503, as the seed ends at rank 5, so unavailable wins over not permitted (403).

**Contract evolution.** Additive changes within `v1` are compatible. Any other change is a new GTS type version, and a plugin moves to it only after the gear supports it.

**Plugin obligations.** The SDK crate is the normative plugin contract. Its main obligations:

- Section plugins register their instance, priority, and scoped client in their own `init`, and answer `key` once `init` has finished.
- Section plugins change the schema version whenever the schema content changes, keep `schema` cheap and free of side effects, and tolerate repeated `apply` and `remove`.
- Section plugins put no manifest values or secrets in any detail, use their own service credentials, make their own authorization decision from the caller identity, and clean up their own data when they are taken out of a deployment.
- Each section plugin ships a conformance test for its side of the contract.

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|---|---|---|
| types-registry | `TypesRegistryClient` | Register the section plugin GTS type in `init`, and list plugin instances once at `start` |
| authz-resolver | PolicyEnforcer | Decisions for `seed`, `uninstall`, `read_operation`, and `read_schema` |
| api-gateway | Platform REST host | Request protection, authentication, `SecurityContext`, readiness, and route hosting |
| Section plugin gears | `cpt-cf-application-manager-interface-section-plugin-api` | Keys, schemas, `apply`, and `remove` |

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use sdk modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

Plugins receive `CallerIdentity` with no bearer token, which is the one deliberate narrowing of the propagation rule, and background work has no caller. The gear declares `deps = [types_registry, authz_resolver]` and the capabilities `db`, `rest`, and `stateful`, and never depends on plugin crates.

### 3.5 External Dependencies

#### Platform Relational Database

- **Contract**: platform storage baseline through SecureConn.

| Dependency | Interface Used | Purpose |
|---|---|---|
| Platform database | SeaORM through SecureConn, migrations | Holds the operations table and the audit ledger table |

#### Platform Observability Backend

- **Contract**: platform telemetry baseline.

| Dependency | Interface Used | Purpose |
|---|---|---|
| Platform telemetry | ToolKit tracing, metrics, and structured logging | Logs, metrics, traces, and operator signals |

The dependency rules of §3.4 apply unchanged.

### 3.6 Interactions & Sequences

#### Startup and Plugin Discovery

**ID**: `cpt-cf-application-manager-seq-startup`

At `start`, after every gear's `init`, the startup component lists the plugin instances once, resolves their clients, reads every key concurrently, starts the sweeper, and publishes the startup result. Any failed check stops the host with a named reason (`cpt-cf-application-manager-component-startup-registration`).

#### Synchronous Checks and Acceptance

**ID**: `cpt-cf-application-manager-seq-pre-dispatch-gate`

**Use cases**: `cpt-cf-application-manager-usecase-first-seed`, `cpt-cf-application-manager-usecase-reseed`, `cpt-cf-application-manager-usecase-uninstall`, `cpt-cf-application-manager-usecase-seed-failed`

**Actors**: `cpt-cf-application-manager-actor-seeding-caller`, `cpt-cf-application-manager-actor-operator`, `cpt-cf-application-manager-actor-platform-access-control`

The diagram draws the repository and the audit ledger as one participant. Ranks refer to the outcome table in §3.3.

```mermaid
sequenceDiagram
    participant C as Caller
    participant API as REST API
    participant O as Request orchestrator
    participant PDP as Platform access control
    participant R as Repository and audit ledger
    C ->> API: seed or uninstall (authenticated)
    Note over API: request id, event ids, receive time
    alt not ready, stopping, or no free request-task permit
        API ->> R: refused event (own transaction)
        API -->> C: refusal (rank 1)
    else permit taken
        API ->> O: start the detached request task
        Note over O: envelope or id checks
        O ->> PDP: decide seed or uninstall (bounded)
        opt permitted
            O ->> R: storage step: open operation of the application (bounded)
        end
        alt a rank 1 to 4 category applies
            O ->> R: refused event (own transaction)
            O -->> API: refusal with the request id
        else nothing decided
            O ->> R: one transaction: interrupt an expired open operation, insert the operation and its accepted event
            alt committed
                O -->> API: accepted, with the operation id
            else a concurrent request won the index
                O ->> R: one transaction: read the winner and insert the refused event
                alt committed
                    O -->> API: conflict naming the winner
                else fails or times out
                    Note over O: missing refused event logged and signalled
                    O -->> API: refusal (acceptance rank)
                end
            else fails or times out
                O ->> R: refused event (own transaction)
                O -->> API: refusal (acceptance rank)
            end
        end
        API -->> C: 202 with the operation id, or the refusal Problem
    end
```

**Description**: Each request takes exactly one path, and no path makes a plugin call. When a refused event cannot be recorded, the refusal keeps its category and the missing event is logged and signalled. On a lost index race, the winner is read in the same transaction that inserts the refused event, so when that transaction fails, the request is refused at the acceptance rank, the missing event is logged and signalled, and nothing more is inserted. A seed with no usable id reads the empty id in the storage step, so the step still proves that storage can answer.

#### Seed Operation

**ID**: `cpt-cf-application-manager-seq-seed`

**Use cases**: `cpt-cf-application-manager-usecase-first-seed`, `cpt-cf-application-manager-usecase-reseed`, `cpt-cf-application-manager-usecase-seed-failed`

**Actors**: `cpt-cf-application-manager-actor-seeding-caller`, `cpt-cf-application-manager-actor-section-plugin`

Every write is guarded (§3.1), and every plugin call goes through the router and the call guard.

```mermaid
sequenceDiagram
    participant O as Request orchestrator
    participant V as Schema validator
    participant P as Section plugins
    participant R as Repository and audit ledger
    O ->> R: mark running
    par every section plugin
        O ->> P: schema
    end
    alt a schema not served or unusable
        O ->> R: final transaction: report (rank 5), finished event
    else a schema call not permitted
        O ->> R: final transaction: report (rank 6), finished event
    else all usable
        O ->> V: validate the sections
        alt invalid
            O ->> R: final transaction: report (rank 7), finished event
        else valid
            loop each section plugin in priority order, before the run deadline
                O ->> P: apply(identity, operation id, envelope, section)
                O ->> R: record the section outcome
            end
            O ->> R: final transaction: report, completed, finished event
        end
    end
```

**Description**: A re-seed follows the same path, and each section plugin decides what the repeat means. A section outcome update that fails keeps the outcome in memory, and the final transaction writes the full report.

#### Uninstall Operation

**ID**: `cpt-cf-application-manager-seq-uninstall`

**Use cases**: `cpt-cf-application-manager-usecase-uninstall`

**Actors**: `cpt-cf-application-manager-actor-operator`, `cpt-cf-application-manager-actor-seeding-caller`, `cpt-cf-application-manager-actor-section-plugin`

```mermaid
sequenceDiagram
    participant O as Request orchestrator
    participant P as Section plugins
    participant R as Repository and audit ledger
    O ->> R: mark running
    loop each section plugin in priority order, before the run deadline
        O ->> P: remove(identity, operation id, application id)
        O ->> R: record the section outcome
    end
    O ->> R: final transaction: report, completed, finished event
```

**Description**: Every section plugin receives `remove`, whether or not the application has ever been seeded, and dispatch continues after failures. An empty key set gives a succeeded uninstall with no plugin call.

#### Read an Operation

**ID**: `cpt-cf-application-manager-seq-operation-read`

**Actors**: `cpt-cf-application-manager-actor-seeding-caller`, `cpt-cf-application-manager-actor-operator`, `cpt-cf-application-manager-actor-platform-access-control`

After a `read_operation` permit, the read service reads the record, records an expired open operation as interrupted first, and returns the status and, once final, the report. It makes no plugin call and records no audit event.

#### Interrupted Sweep and Purge

**ID**: `cpt-cf-application-manager-seq-operation-sweep`

On every replica, the sweeper interrupts expired open operations and purges final records past `operation_retention`, in guarded batches (`cpt-cf-application-manager-component-operation-sweeper`).

#### Discovery of Keys and Schemas

**ID**: `cpt-cf-application-manager-seq-discovery`

**Use cases**: `cpt-cf-application-manager-usecase-discovery`

**Actors**: `cpt-cf-application-manager-actor-seeding-caller`, `cpt-cf-application-manager-actor-operator`, `cpt-cf-application-manager-actor-platform-access-control`, `cpt-cf-application-manager-actor-section-plugin`

After a `read_schema` permit, the read service returns the key list from memory, or fetches one or every schema live and returns each with its schema version. The combined read fails when any section plugin cannot answer, with the precedence of the schema call results in [§3.3](#33-api-contracts).

### 3.7 Database schemas & tables

- [ ] `p3` - **ID**: `cpt-cf-application-manager-db-operations`

The gear owns the operations table and the audit ledger table, created by its migrations. Supported backends are PostgreSQL, MySQL, and SQLite.

#### Table: operations

**ID**: `cpt-cf-application-manager-dbtable-operations`

**Schema**:

| Column | Type | Description |
|---|---|---|
| `operation_id` | UUID | Time-ordered operation id |
| `application_id` | text, at most `APPLICATION_ID_MAX_BYTES` | Exact application id |
| `action` | enumerated text | `seed` or `uninstall` |
| `status` | enumerated text | `accepted`, `running`, `completed`, or `interrupted` |
| `category` | enumerated text, nullable | Category of a completed operation |
| `report` | JSON | The outcome report so far, with no manifest or section content |
| `accepted_at` | timestamp | Receive time |
| `started_at` | timestamp, nullable | Set by mark running |
| `finished_at` | timestamp, nullable | Set with a final status; retention counts from it |
| `run_deadline` | timestamp | End of the run window |
| `finished_event_id` | UUID | Event id of the finished event, used by every writer of that event |

**PK**: `operation_id`

**Constraints**: all columns NOT NULL apart from `category`, `started_at`, and `finished_at`. `category` is set exactly when the status is `completed`, and `finished_at` exactly when the status is final. Text columns use a binary, no-pad collation, so ids that differ in case, trailing spaces, or normalization stay distinct.

**Additional info**: a unique index on `application_id` over rows with status `accepted` or `running` allows one open operation per application. It is a partial index on PostgreSQL and SQLite, and a unique index on a stored generated column on MySQL. Indexes on (`status`, `run_deadline`) and on `finished_at` serve the sweep and the purge. The table grows by one row per accepted request and shrinks with the purge.

**Example**:

| operation_id | application_id | action | status | category | finished_at |
|---|---|---|---|---|---|
| `01929a3c-5e1f-7b2a-9c4d-3f8e6a1b2c7d` | `billing` | `seed` | `completed` | `plugin_rejected` | `2026-10-15T09:12:51Z` |

#### Table: audit_ledger

**ID**: `cpt-cf-application-manager-dbtable-audit-ledger`

**Schema**:

| Column | Type | Description |
|---|---|---|
| `event_id` | UUID | Time-ordered event id |
| `kind` | enumerated text | `accepted`, `finished`, or `refused` |
| `correlation_id` | UUID | The request id: the operation id for accepted and finished events, the request id for a refused event |
| `action` | enumerated text | `seed` or `uninstall` |
| `written_at` | timestamp | Write time |
| `content` | JSON | The event content of `cpt-cf-application-manager-entity-audit-event-content`: the envelope fields within the limits, the keys of the key set with the count of other keys, the section outcome per key of the key set, problem counts and rule tokens, unavailable items, and the caller identity and times. It holds no parsed manifest, no section body, no problem location or message, and no plugin detail. |

**PK**: `event_id`

**Constraints**: all columns NOT NULL. The table is insert-only: the gear never updates or deletes a row. It has no tenant column, and the entity is declared unrestricted.

**Additional info**: an index on `correlation_id` joins the events of one request. The operation repository is the only writer, and it inserts at three write points:

- the accepted event, in the acceptance transaction with the operation record;
- the finished event, in the final transaction with the report, which includes the interruption step and the supervisor completion;
- a refused event, in its own transaction before the refusal answers.

The ledger is a record of what happened, for people who debug a request later. The gear never reads it back, compares against it, or decides from it. The gear keeps ledger rows without a purge, because the PRD leaves retention of audit data out of scope. The table grows by two rows per accepted request (accepted and finished) and one per refusal. An acceptance that timed out but committed counts as both. No gear route reads it.

**Example**:

| event_id | kind | correlation_id | action | written_at |
|---|---|---|---|---|
| `01929a3c-5e20-7c11-8a2e-6b4d9f0e1a35` | `finished` | `01929a3c-5e1f-7b2a-9c4d-3f8e6a1b2c7d` | `seed` | `2026-10-15T09:12:51Z` |

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-application-manager-topology-replicas`

The gear runs in the platform host process on each replica, with its section plugin gears in the same process. Replicas are stateless apart from the shared database. Each runs its own sweeper, and the status guard keeps them safe together. An operation runs on the replica that accepted it, with that replica's plugin set, and any replica can read or interrupt it.

```mermaid
flowchart LR
    LB["Platform gateway"] --> R1["Replica: gear and in-process plugins"]
    LB --> R2["Replica: gear and in-process plugins"]
    R1 --> DB[("Platform database: operations and audit ledger")]
    R2 --> DB
```

The healthcheck reports not ready until the startup result is published, and after that it probes no dependency. On shutdown, `stop` sets the stopping flag, drains the request tasks, stops the sweeper, and drains the detached plugin calls until the stop deadline. Operations left open are interrupted after their run deadline.

## 4. Additional context

### 4.1 Concurrent Requests for the Same Application

The unique index on open operations admits one open operation per exact application id, across replicas, so a second request is refused as conflict naming the open one. An open operation past its run deadline does not block: the next acceptance interrupts it in the same transaction.

### 4.2 Security Considerations

- Callers are authenticated by the platform, and the gear is supported only with gateway authentication enabled, because `auth_disabled` gives every request the same subject.
- Plugins are operator-deployed gears in the same process, looked up by exact GTS id, and trusted with the data they receive. The gear does not sandbox them.
- A manifest cannot select a plugin, because routing uses only the startup plugin set, and an unknown key makes the manifest invalid.
- The permission decision comes before the conflict read, the acceptance, and any plugin call, and the gear fails closed when the PDP cannot answer.
- `seed` and `uninstall` are granted apart and narrowly, and each section plugin makes its own decision from the caller identity, which limits confused-deputy use of plugin credentials.
- Served schemas are untrusted input: no external reference, a linear-time regex engine, a size cap, and compilation off the async executor.
- Ids and versions are escaped and bounded in logs and spans, which prevents log injection.
- The caller identity is the only personal data the gear handles, and it is stored only in audit ledger rows. Encryption in transit and at rest follows the platform baselines.
- The gear never stores section content, so a secret inside a section is never stored by the gear and reaches only the owning section plugin. Of the caller's manifest text, ledger rows carry only the envelope id and version within their limits. Operation records also carry problem locations, which can name keys and properties the caller chose, and the sweeper purges those records after `operation_retention`. Logs carry the envelope id and version within their limits. So keeping secrets out of the envelope and out of key and property names, as out of the whole manifest, is the seeding callers' part, as the PRD assumes.
- The manifest-derived part of an audit ledger row is the envelope fields within their limits, keys of the section key set, counts, and rule tokens, so its size is bounded by the envelope limits and the section key set alone. The ledger has no purge, because the PRD leaves retention of audit data out of scope. Operators size database storage for its growth: two rows per accepted request and one per refusal, including refusals of callers that are not permitted.

### 4.3 Fault Tolerance

| Failure | Behaviour | Recovery |
|---|---|---|
| PDP or operation storage cannot answer | Refused, with no operation and no plugin call | Retry after the delay |
| Not ready, stopping, or request-task cap full | Refused at the gate, logged and counted | Retry after the delay |
| Acceptance fails or times out | Refused. A transaction that timed out but committed leaves an operation with no task, which is interrupted at its run deadline; its accepted event and the refused event share the correlation id. | Retry; a retry that meets it gets conflict naming it |
| A refused event cannot be recorded | The refusal keeps its category, and the missing event is logged and signalled. On a lost index race, where the same transaction reads the winner, the refusal is at the acceptance rank. | Operators restore storage |
| A section schema is not served or unusable | Nothing dispatched; the unusable-schema signal fires | Retry after the plugin recovers or is fixed |
| A section plugin fails, times out, or is at its cap | `internal_failure`, `timed_out`, or `not_sent`; dispatch continues | Seed or uninstall again |
| The final transaction cannot be recorded | Logged and signalled; the operation is interrupted at its run deadline | Seed or uninstall again |
| A replica stops, or a task is abandoned at the stop deadline | Not resumed; any replica interrupts it after the run deadline | Seed or uninstall again |
| The request task panics | Before the answer: 500 `internal`, and the refused event records internal failure. After acceptance: the supervisor marks the operation running, then completes it as internal failure, with `not_dispatched` for plugins not yet called. | Seed or uninstall again |
| A sweep or purge step fails | Logged; the next interval tries again | Operators restore storage |
| A startup check fails | Startup failure event; the host exits | Fix the cause, then restart |

The gear never resumes an operation, and callers retry by sending a new request. The per-plugin in-flight cap is the bulkhead that bounds the load on one plugin.

### 4.4 Observability

**Logs.** Structured logs carry the correlation id, trace id, action, application id and version, category, section key, and call kind as relevant. They never carry section content, manifest bodies, or schema bodies. Ids, versions, and schema versions are written with every control character escaped and cut at `LOG_TEXT_MAX_BYTES`, with the full length and SHA-256 digest added. Span attributes follow the same rule.

**Metrics.** Synchronous answers by action and result; operations accepted, completed by category, and interrupted; operation durations; plugin call durations, timeouts, cap refusals, and calls still running after their timeout; request tasks in flight; audit events recorded and not recorded; dependency-unavailable counts; unusable schemas; sweep and purge results; startup duration and failures by reason. The section key is a safe label. Application ids and operation ids are never labels.

**Signals.** Operators set thresholds for: audit events not recorded; rising interrupted operations; repeated timeouts or cap refusals for one section key; an unusable schema; an unavailable dependency; failing sweep or purge steps; request tasks at the cap; a run window margin warning; and a startup failure.

**Traces.** Each request has a span for its synchronous phase, linked to a span for its operation, with child spans for plugin calls and guarded writes. Each sweep and purge has its own span.

### 4.5 Configuration

Settings live under `gears.application-manager.config`. Each is required with no default, and a missing or invalid value fails `init` with `invalid_setting`.

| Setting | Meaning | Validation |
|---|---|---|
| `plugin_call_timeout` | The one time limit for every plugin call, and the unit of the run window | Positive duration |
| `run_window_margin` | Added to each operation's plugin-call time to give its run deadline | Positive duration, at least 2 × `storage_call_timeout` |
| `operation_retention` | How long a final operation record is kept | Positive duration |
| `plugin_max_in_flight` | Most calls in flight per plugin instance, counting detached calls | Positive integer |
| `max_request_tasks` | Most seed and uninstall request tasks in flight per replica | Positive integer |
| `max_envelope_text_length` | Maximum byte length of the application id, for seed and uninstall, and of the version | Positive integer, at most `APPLICATION_ID_MAX_BYTES` |
| `pdp_call_timeout` | Deadline of one PDP decision | Positive duration |
| `storage_call_timeout` | Bound of every operation access and audit ledger insert | Positive duration |

The gear constants are `APPLICATION_ID_MAX_BYTES`, `SCHEMA_VERSION_MAX_BYTES`, `SERVED_SCHEMA_MAX_BYTES`, `SCHEMA_REGEX_SIZE_LIMIT`, `SCHEMA_REGEX_DFA_SIZE_LIMIT`, `LOG_TEXT_MAX_BYTES`, `PLUGIN_DETAIL_MAX_BYTES`, `REFUSAL_RETRY_AFTER_SECONDS`, `SWEEP_INTERVAL`, `PURGE_INTERVAL`, `MAINTENANCE_BATCH_SIZE`, and `GATEWAY_REQUEST_DEADLINE`. The stop deadline is the host's shutdown deadline and is not a gear setting.

**Synchronous time budget.** The synchronous phase takes at most `pdp_call_timeout` + 3 × `storage_call_timeout`: the storage step, the acceptance, and the refusal after a failed acceptance, which on a lost index race is one transaction that reads the winner and inserts the refused event. When that transaction fails, the request is refused at the acceptance rank, the missing refused event is logged and signalled, and nothing more is inserted, so the bound holds on every path. `init` fails when this exceeds `GATEWAY_REQUEST_DEADLINE`, the gateway's fixed 30s.

**Run window.** This sizes the PRD's run window. The run deadline is the acceptance commit time plus (N + 1) × `plugin_call_timeout` for a seed, or N × `plugin_call_timeout` for an uninstall, plus `run_window_margin`, where N is the number of section plugins. The synchronous phase ends with that commit, so the whole run window belongs to the operation. After its acceptance commits, an operation makes at most N + 2 storage steps, so `start` logs a margin warning when the margin is below (N + 2) × `storage_call_timeout`.

**Sizing.** Size `max_request_tasks` for the operations expected at once during an environment-wide re-seed. Size `plugin_max_in_flight` above `max_request_tasks`, so a plugin meets its cap only when it hangs. Operators set the database pool acquire timeout below `storage_call_timeout`.

### 4.6 Platform Deviations and Dependencies

- **Eager plugin discovery.** The gear discovers every plugin instance once at `start` and does not resolve lazily or use `choose_plugin_instance`, so conflicts fail at deploy time ([ADR 0001](./ADR/0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)). Review owner: platform plugin model owners.
- **Platform-level access.** Operation records and audit ledger rows are unrestricted secure-ORM entities with no tenant column, and every PDP decision is requested with no tenant, which departs from the tenant-scoped platform default. The gear manages applications installed into the whole environment, so its records and permissions are platform-level ([ADR 0005](./ADR/0005-cpt-cf-application-manager-adr-platform-level-access.md)). Review owner: authz-resolver and secure ORM owners.
- **Detached work.** Guarded plugin calls and request tasks do not take the shutdown token, because `cpt-cf-application-manager-fr-bounded-plugin-calls` forbids cancelling plugin work. The drains abandon them at the stop deadline with a log. Review owner: ToolKit lifecycle owners.
- **Hand-implemented lifecycle.** The gear implements `RunnableCapability` itself, because `WithLifecycle` only logs a task error and a failed startup check must fail the start phase. Review owner: ToolKit lifecycle owners.
- **In-process plugins only.** Out-of-process plugins are not supported until the platform guarantees that they register before dependants start. Owner: platform plugin model owners.
- **Gateway deadline.** The gateway does not expose its 30s request deadline, so the gear holds it as `GATEWAY_REQUEST_DEADLINE` with a pin test. Owner: api-gateway owners.
- **Error format.** The platform error format keeps no gear-added members, so refusals carry the request id and the busy operation id as text in `detail`. Owner: canonical error system owners.
- **Staged problem listing.** Section problems are listed only once the envelope is valid, because schemas are fetched only inside an accepted operation. Owner: PRD owner.

### 4.7 Areas Not Applicable

- **Capacity and cost budgets**: not applicable beyond the sizing rules of [§4.5](#45-configuration), because seeds are infrequent deploy-time requests (PRD §6.2).
- **Testing strategy**: unit tests cover the domain with port doubles. Integration tests cover the repository on each supported backend and the REST surface. Each section plugin ships a contract conformance test. Performance and end-to-end suites are not applicable, because load is low.
- **MFA, SSO, sessions, CORS, network segmentation, and DMZ**: not applicable, because authentication and request protection belong to the platform gateway and the interfaces are machine-facing.
- **Compliance and privacy architecture**: platform baselines apply. The caller identity in ledger rows is the only personal data. Ledger rows, including the caller identity, are kept without a purge because audit retention is outside PRD scope, and data-subject requests are a platform-level matter (PRD §6.2).
- **Tamper evidence**: not provided beyond append-only use by the operation repository.
- **Performance targets, infrastructure code, SLO targets, and user-facing architecture**: not applicable (PRD §6.2).

## 5. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **ADRs**: [ADR/](./ADR/)
  - `cpt-cf-application-manager-adr-section-plugin-fan-out` — [0001](./ADR/0001-cpt-cf-application-manager-adr-section-plugin-fan-out.md)
  - `cpt-cf-application-manager-adr-plugin-served-section-schemas` — [0002](./ADR/0002-cpt-cf-application-manager-adr-plugin-served-section-schemas.md)
  - `cpt-cf-application-manager-adr-async-operations` — [0003](./ADR/0003-cpt-cf-application-manager-adr-async-operations.md)
  - `cpt-cf-application-manager-adr-audit-ledger` — [0004](./ADR/0004-cpt-cf-application-manager-adr-audit-ledger.md)
  - `cpt-cf-application-manager-adr-platform-level-access` — [0005](./ADR/0005-cpt-cf-application-manager-adr-platform-level-access.md)
- **Features**: not applicable — this gear's document set has no separate feature specifications.
- **Platform baselines**: [Architecture Manifest](../../../../docs/ARCHITECTURE_MANIFEST.md), [security guidelines](../../../../guidelines/SECURITY.md), [ClientHub and plugins](../../../../docs/toolkit_unified_system/03_clienthub_and_plugins.md), [plugin guide](../../../../docs/TOOLKIT_PLUGINS.md), [lifecycle](../../../../docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md), [canonical error system](../../../../docs/arch/errors/DESIGN.md), [AuthN, AuthZ and secure ORM](../../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md)
