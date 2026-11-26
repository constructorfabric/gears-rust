<!-- Created: 2026-04-25 by Constructor Tech -->

# PRD — Quota Enforcement

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
  - [2.3 Actor Permissions](#23-actor-permissions)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Subject Model & Attribution](#31-subject-model--attribution)
  - [3.2 Metric Identity](#32-metric-identity)
  - [3.3 Integration Boundary with Usage Collector](#33-integration-boundary-with-usage-collector)
  - [3.4 Decision Contract for Calling Services](#34-decision-contract-for-calling-services)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Metric Types & Subject Attribution](#51-metric-types--subject-attribution)
  - [5.2 Quota Lifecycle](#52-quota-lifecycle)
  - [5.3 Quota Type Semantics](#53-quota-type-semantics)
  - [5.4 Time Period & Reset Semantics](#54-time-period--reset-semantics)
  - [5.5 Quota Operations: Debit, Credit, Rollback, Return, Preview](#55-quota-operations-debit-credit-rollback-return-preview)
  - [5.6 Lease & Two-Phase Operations](#56-lease--two-phase-operations)
  - [5.7 Bulk Operations](#57-bulk-operations)
  - [5.8 Idempotency](#58-idempotency)
  - [5.9 Multi-Quota Evaluation, Quota Resolution Policy & Pluggable Engine](#59-multi-quota-evaluation-quota-resolution-policy--pluggable-engine)
  - [5.10 Quota Snapshot Read API](#510-quota-snapshot-read-api)
  - [5.11 Quota Enforcement Mode](#511-quota-enforcement-mode)
  - [5.12 Tenant Isolation](#512-tenant-isolation)
  - [5.13 Authorization](#513-authorization)
  - [5.14 Pluggable Storage Backend](#514-pluggable-storage-backend)
  - [5.15 Notification Plugin Contract](#515-notification-plugin-contract)
  - [5.16 Operational Telemetry](#516-operational-telemetry)
  - [5.17 Quota Resolver Plugin](#517-quota-resolver-plugin)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 Data Governance](#62-data-governance)
  - [6.3 NFR Exclusions](#63-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

The Quota Enforcement gear is the platform's authoritative engine for declaring resource consumption limits ("quotas")
and evaluating whether individual operations are permitted under those limits. It supports debit, credit, rollback,
return, and two-phase lease primitives. Modules reach it through the **Quota Resolver**, the platform gear for
charging and reading quotas, whose backend plugin QE implements; QE's own API serves management and callers that use
QE directly. Subjects are Quota Resolver subject types, so quotas apply to tenants, users, and any operator-defined
subject kind without changing the evaluation engine.

Quota Enforcement is consumption-counter infrastructure only. It does not collect raw usage events, does not compute
pricing, does not own license-pack catalogs, plan templates, redistribution UIs, or increase-request workflows, and does
not decide enforcement responses on behalf of callers — it returns deterministic decisions (`Allowed` / `Denied`) or a
platform-canonical error that the calling service applies. Commercial overage and overage pricing (a tenant exceeds an
entitled allowance and is charged premium rates) are explicitly out of scope at the QE admission layer; that
responsibility belongs to the billing service downstream of QE, composed from Usage Collector observations and Quota
records. Integration with the Usage Collector (which is the source of consumption events) is performed by a separate
wrapper component, and quota management/provisioning concerns (catalog, plans, redistribution, increase-requests,
end-user and tenant-admin UIs) are owned by **Quota Manager** — both out of scope for this PRD; see §4.2.

### 1.2 Background / Problem Statement

Core CF/Gears (LLM Gateway, file-storage, compute, API gateway, etc.) increasingly need to enforce
resource budgets against a variety of subjects — tenants and individual users today, with richer organizational
structures (cost centers, applications, departments) anticipated as the platform's organizational model evolves. Typical
examples already in demand include: maximum AI tokens per day, maximum compute hours per month, maximum storage
allocation. Today each consuming service either implements ad-hoc counters in its own database (no consistency across
services, no operator visibility, no idempotency) or the budget is not enforced at all (costly overruns, no fairness
between tenants).

Without a centralized quota engine, every consuming team duplicates the same counter, ledger, period-rollover,
idempotency-key, and quota-resolution code — at varying levels of correctness. Cross-service quota policies (a single "
AI tokens per tenant" budget that spans multiple AI-using services) are impossible to express because each service holds
its own counter. Operator workflows for raising or lowering caps require a per-service migration. The Quota Enforcement
gear addresses these problems by providing a single, shared, multi-tenant counter/ledger backend with explicit Quotas,
idempotent operations, and a uniform evaluation contract.

### 1.3 Goals (Business Outcomes)

- **Single source of truth for resource budgets**: All consuming platform services evaluate budgets against Quota
  Enforcement rather than maintaining their own counters; one operator change to a quota cap takes effect across every
  service that consults it.
- **Idempotent, race-safe accounting**: Every debit, credit, rollback, return, and lease operation is replay-safe
  under at-least-once delivery semantics, so retried network calls or duplicate background-job invocations do not
  corrupt counters.
- **Operator self-service for new quota types**: Platform operators can define new metric quotas at runtime and assign
  them to tenants or users without code changes, redeployment, or per-service rollout.
- **Predictable enforcement decisions for consumers**: Consumers receive deterministic decisions (`Allowed` / `Denied`)
  within a bounded latency budget, with explicit reason codes, so they can integrate quota checks into hot paths without
  bespoke fallback logic. Operational failures (Engine timeout, mid-flight infrastructure unavailability, invariant
  violations) surface as platform-canonical errors with the same fail-closed discipline.

**Success Metrics** (measured at initial production release):

| Goal                              | Measurable Success Criterion                                                                                                                                                         | Target                                |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------- |
| Single source of truth            | All in-scope platform services that previously held local quota counters integrated with Quota Enforcement; zero per-service quota counters remaining at first production deployment | 100% migration of in-scope services   |
| Idempotent accounting             | Zero double-counting incidents observed in soak testing under simulated retry storms (10× normal RPS with 5% retry rate) over a 7-day period                                         | 0 double-count events over 7-day soak |
| Operator self-service             | Time from API call defining a new quota to first evaluable operation against it                                                                                                      | ≤ 1 minute end-to-end                 |
| Predictable enforcement decisions | p95 latency of `debit` operation under target load                                                                                                                                   | ≤ 100ms p95 at 10 000 ops/sec         |

### 1.4 Glossary

| Term                      | Definition                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
|---------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Quota Resolver            | The platform gear through which modules charge and read quotas ([PRD](../../quota-resolver/docs/PRD.md)). It has two planes: the **platform plane**, where a platform service names the tenant and the subjects, and the **tenant plane**, where an authenticated end user reads its own quota state. It calls QE in process through the resolver plugin that QE implements, and its SDK owns the quota subject base, the tenant subject type, and the metric base, the GTS type from which every metric derives. |
| Subject                   | A concrete identity to which a Quota is assigned: `{type, id}`, stored as `(subject_type, subject_id)`. `type` is a quota subject type, a GTS type derived from the Quota Resolver base `gts.cf.core.quota.subj.v1~`; `id` is opaque. The tenant subject type `gts.cf.core.quota.subj.v1~cf.core.quota.tenant.v1~` names the tenant itself, its id is the tenant id; other types (user, device, agent, service account) are registered by the operator or by the module that charges that kind of subject. |
| Subject Tier              | The rank of a subject type for the Engine: `Tenant` for the tenant subject type, `Subject` for every other type. |
| Metric                    | What a Quota counts: a GTS type derived from the Quota Resolver metric base `gts.cf.core.quota.metric.v1~`, registered in `types-registry` by the Gear that owns the metric and named under that Gear (a **metric type**; ADR-0013). Its traits are `kind` (`counter` or `gauge`), `admitted_subjects` (the subject types the metric's quotas may count; §3.1), and `enforcement` (`quota_gated` by default, or `direct`; §3.2). Its schema refines the `metadata` of the metric object. Another major version of a metric type is another metric with its own Quotas. |
| Metric Object             | How a request names its metric: `{type, id?, metadata}`. `type` is the metric type. `id` is optional and names the one thing the amount is counted for, for example a model or a virtual machine; it is not part of the P1 counter key. `metadata` holds the attributes of the operation that Policies read. QE validates the whole object against the resolved schema of the metric type. |
| Metric Catalogue          | QE's process-local record of the metric types it has read from `types-registry`: per metric, the resolved schema and the traits. An entry is **served** or **unavailable** (ADR-0012). |
| Subject Attribution       | The caller-supplied `tenant_id` plus `subjects: [{type, id}]`. The **attribution** of an operation is this tuple plus the metric, with its type and optional id. On QE's own API the PDP authorizes it against the authenticated service principal; on the resolver plugin path no PDP check runs (`cpt-cf-quota-enforcement-fr-resolver-plugin`). |
| Access Scope              | What admission returns for an operation and storage binds to every query: the tenant, and on QE's own API the PDP constraints, that the operation may reach. |
| Trusted Admission         | The admission mode of the resolver plugin path: it checks the target's shape, calls no PDP, and returns an access scope that names the request's tenant only (ADR-0009). |
| Quota                     | A declarative limit assigned to a single subject for a single metric. Carries the subject reference, metric reference, quota type, period (consumption types only), enforcement mode, cap, optional notification thresholds, optional validity window, and optional failure-mode hint. Identified by a stable quota ID. Quotas are first-class stored entities — there is no separate "template" or "binding" concept; an operator who wants the same shape on many subjects creates one Quota per subject (typically driven by the subject manager at subject-creation time).                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Quota Snapshot            | A point-in-time per-Quota view returned by the snapshot read APIs (§5.10): for each applicable Quota — `quota_id`, `cap` (numeric or `null` for unbounded Quotas), current consumed, `remaining` (numeric or `null`), period boundary, validity window, metadata. Engine-agnostic — no aggregate "headline" cap/balance is computed, since under cascade, split, or attribute-gated Engines no single number is universally meaningful. Authoritative admission for a specific operation is given by the Engine's `Decision` (or its read-only `Preview`, `cpt-cf-quota-enforcement-fr-evaluate-preview`).                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Quota Resolution Policy   | An operator-managed entity that binds a Quota Resolution Engine and its config to a scope. P1 supports two scope levels: platform default (`global`) and per-metric. The most-specific-scope Policy applies at evaluation time. The scope ladder is intentionally extensible — narrower scopes (e.g., per-subject) are deferred and tracked in §13 Open Questions. See `cpt-cf-quota-enforcement-fr-quota-resolution-policy` for versioning, scope precedence, and seeded defaults.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| Quota Resolution Engine   | The pluggable component that implements multi-Quota arbitration logic. Receives the applicable-Quotas set, current usage, the request, and an opaque engine-specific config; returns a Decision (result + debit plan + diagnostics). P1 ships two built-ins: `most-restrictive-wins` (hardcoded; fastest path; produces a single-entry Debit Plan against the binding Quota — see §5.9) and `cel` (sandboxed CEL evaluator; customizable). Multi-Quota debit patterns (cascade, attribute-weighted splits) are expressed via a `cel` Policy or future Engines. Additional engines (Starlark, Lua, Wasm-loaded) plug in via the same trait without changes to evaluation core.                                                                                                                                                                                                                                                                                                                                                                                                         |
| Debit Plan                | A list of **chains** produced by the active Engine alongside the result verdict. A chain is an ordered list of entries `{quota_id, amount}`, primary pool first, whose amounts add up to exactly the requested amount; several chains are an AND, each counting the whole operation. A Quota appears at most once in a plan. The system mutates counters strictly per the plan; Quotas not named in it are not touched. **Unwinding** takes an amount X back from a charge: in every chain, X is taken from the entries in reverse order, each entry at most what it still holds (ADR-0010). |
| Quota Cascade / Spillover | An arbitration pattern where one Quota acts as primary pool and another as fallback; debit is routed to the primary first and falls through to the fallback only when the primary's remaining capacity is insufficient. The default `most-restrictive-wins` Engine cascades from the `Subject` tier to the `Tenant` tier with one-entry Debit Plans; the `cel` Engine produces chains of several entries for splits (primary takes X, fallback takes `amount − X`), intra-tier cascade between Quotas identified by subject type or id, and weighted splits. |
| Quota Metadata            | An operator-supplied JSON object on a Quota, at most 4 KB: the Quota's **conditions** over the Request Metadata. Each key names a scalar property of the metric type's `metadata`, and its value is one value or a list of values of that property. A Quota applies to a request only when the request's metadata satisfies every condition. |
| Request Metadata          | The `metadata` of the request's metric object: caller-supplied attributes of the operation, validated against the schema of the metric type and given to the Engine as `request`. |
| Allocation Quota          | A quota type that bounds in-flight reservable capacity (e.g., maximum concurrent jobs). No periodic reset.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Consumption Quota         | A quota type that bounds cumulative consumption within a recurring time period. Counter resets at the period boundary.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| Rate Quota                | A quota type that bounds the rate of operations over a sliding or windowed interval. P3 type; not implemented in P1.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Enforcement Mode          | The Quota's behavior at the cap boundary.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| Period                    | The recurring time window for consumption-type Quotas. UTC, calendar-aligned by default.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Debit                     | An idempotent operation that decreases a Quota's remaining capacity (allocation: increment in-flight; consumption: increase used-amount in current period).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Credit                    | An idempotent operation that increases a Quota's remaining capacity (allocation: decrement in-flight; consumption: decrease used-amount).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Replay Window             | The time QE keeps idempotency records, 24 hours by default. |
| Charge                    | An allowed debit or a lease commit, recorded in the **charge ledger** and named by its tenant, operation (debit or commit), and idempotency key (ADR-0011). It is the idempotency record of that debit or commit for as long as it lives. Its **return window** is the replay window, counted from its creation. |
| Rollback                  | An operation that returns everything that remains of a charge. It names the charge — tenant, operation, and key — with no amount and no idempotency key of its own. On QE's own API it also carries the attribution of the charged operation, which QE authorizes again. Equivalent to refund-on-cancel. |
| Return                    | An idempotent operation that gives back part or all of a charge by unwinding it over the Quotas the charge reached. QE records the key of every applied return with its charge and amount. |
| Lease                     | A two-phase operation that holds capacity for a bounded TTL without committing. Resolved by `commit` (converts the lease to a debit), `release` (returns capacity to the Quota), or auto-release on TTL expiry.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| CEL                       | Common Expression Language. Used to express custom Quota Resolution Policies.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Idempotency Key           | A client-supplied identifier ensuring that duplicate submissions of the same operation produce a single counter effect. Unique per `(tenant, operation, key)`. Required on every debit, credit, return, reserve, commit, and release call, on every batch envelope, and on every batch item.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Notification Plugin       | An in-process Rust trait implemented by deployment-specific event sinks. P1 emission mechanism; P2 will additionally route through the platform EventBus.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Fail-Closed               | Failure mode in which authorization or evaluation errors result in operation denial. Quota Enforcement itself is fail-closed on internal errors; the consuming service decides its own behavior on Quota Enforcement unavailability.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| PDP                       | Policy Decision Point — the platform authorization service (`authz-resolver`) that evaluates access control policies and returns permit/deny decisions, optionally with constraint filters that narrow the scope of permitted operations.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| SecurityContext           | A platform-provided server-side structure carrying the authenticated caller principal: a service principal on the consumer S2S surface and a user/operator principal on management surfaces. Derived from authentication, never from request payloads; explicit target attribution is a separate PDP-authorized input.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |

## 2. Actors

### 2.1 Human Actors

Quota Enforcement is a server-side engine. Only one human actor — the Platform Operator — interacts with QE directly.
Tenant administrators and end users interact with the platform via **Quota Manager** (a separate platform component, out
of scope for this PRD); Quota Manager translates their workflows into QE API calls.

#### Platform Operator

**ID**: `cpt-cf-quota-enforcement-actor-platform-operator`

- **Role**: Defines quota types, configures storage backend, sets default quota-resolution
  policy, registers notification sinks, and monitors overall Quota Enforcement health. The operator is responsible for
  every Policy change (`cpt-cf-quota-enforcement-fr-quota-resolution-policy`).
- **Needs**: APIs for declarative quota lifecycle management; visibility into per-quota utilization and
  rejection rates; ability to onboard new metric quotas without redeployment.

> Tenant Administrator and End User are recognized human roles at the platform level but are **not direct actors** of
> Quota Enforcement. Tenant-administrator workflows (assigning caps within a tenant, requesting cap increases,
> redistributing tenant top-ups) are mediated by Quota Manager (`cpt-cf-quota-enforcement-actor-quota-manager`), which
> calls QE's APIs as a Quota Manager actor. An end user reads their own quota state through the Quota Resolver
> (`cpt-cf-quota-enforcement-actor-quota-resolver`).

### 2.2 System Actors

#### Quota Consumer

**ID**: `cpt-cf-quota-enforcement-actor-quota-consumer`

- **Role**: Any authenticated platform service that performs operations subject to budget enforcement (e.g., LLM
  Gateway invoking a model, compute service starting a job, storage service allocating bytes). It charges through the
  Quota Resolver, or calls QE's own API with an explicit target tenant, subjects, and metric object; on QE's API
  the PDP authorizes that attribution against the service principal. New consumers use the Quota Resolver. End
  users never call QE directly.

#### Quota Resolver

**ID**: `cpt-cf-quota-enforcement-actor-quota-resolver`

- **Role**: The Quota Resolver gateway. It calls QE's resolver plugin in process for debit, lease, return, and quota
  state, with a fixed tenant and quota subjects, and its caller's platform identity for the record only.

#### Metric Owner

**ID**: `cpt-cf-quota-enforcement-actor-metric-owner`

- **Role**: The single Gear that owns a metric. It registers the metric type in `types-registry`, named under the
  Gear, with the traits `kind`, `admitted_subjects`, and `enforcement` and the schema of the metric object's
  `metadata`. The schema must be general enough for every service that debits the shared metric. The owner does not
  proxy calls to QE; any authorized caller may debit directly. No registration by another Gear changes how the metric
  is served.

#### Quota Reader

**ID**: `cpt-cf-quota-enforcement-actor-quota-reader`

- **Role**: Any system that queries Quota Snapshot data without modifying counters (e.g., dashboards, billing systems,
  throttling proxies). Consumes the read API only.

#### Quota Manager

**ID**: `cpt-cf-quota-enforcement-actor-quota-manager`

- **Role**: Platform component that owns license-pack catalog, plan templates, provisioning, redistribution,
  increase-request workflow, and the human-facing surfaces for tenant administrators and end users. Calls Quota
  Enforcement's management CRUD/read APIs on behalf of tenant administrators under user-plane PDP scope. It creates
  each Quota for a subject type that the metric admits.
- **Needs**: Stable Quota CRUD APIs (create, update, deactivate, read), bulk Quota Snapshot reads with PDP scoping,
  Quota Snapshot reads scoped to a single user/tenant, credit primitive for redistribution and compensatory adjustments.

#### Subject Manager

**ID**: `cpt-cf-quota-enforcement-actor-subject-manager`

- **Role**: Platform service that owns the lifecycle of subject identities (e.g., `account-management` for
  tenants and users). Subject managers signal subject lifecycle events (created, removed, identity changes) to **Quota
  Manager**, which decides whether to materialize plan templates into concrete Quotas, deactivate Quotas on subject
  removal, or perform any other reaction; Quota Manager translates those decisions into Quota Enforcement CRUD calls.
  **In P1 there is no direct Subject Manager → Quota Enforcement channel** — the subject-manager-issued lifecycle stream
  reaches QE solely via Quota Manager-mediated CRUD. Neither subject managers nor Quota Enforcement owns the
  template/catalog concept (see §4.2 umbrella delegation).

#### Storage Backend

**ID**: `cpt-cf-quota-enforcement-actor-storage-backend`

- **Role**: The persistent store backing Quota Enforcement's Quotas, counters, leases, Quota Resolution Policies, and
  operation log. P1: `toolkit-db`-based plugin; alternative backends are viable under the same plugin contract
  (`cpt-cf-quota-enforcement-contract-storage-plugin`).

#### Notification Sink

**ID**: `cpt-cf-quota-enforcement-actor-notification-sink`

- **Role**: A deployment-specific implementation of the Quota Enforcement notification plugin contract. Receives events
  on threshold crossings, period rollovers, lease auto-release, lease-resolved-by-deactivation, and definition changes.
  P1: in-process plugin trait. P2: standardized routing through the platform EventBus.

#### Types Registry

**ID**: `cpt-cf-quota-enforcement-actor-types-registry`

- **Role**: The platform `types-registry` gear. Hosts the metric types, with their traits and schemas, and the quota
  subject types. Quota Enforcement reads a metric type by its id, registers no metric type and no subject type, and
  does not duplicate these schemas in a table or registration API.

#### AuthZ Resolver

**ID**: `cpt-cf-quota-enforcement-actor-authz-resolver`

- **Role**: Platform PDP that authorizes every operation of QE's own API, optionally returning constraint filters that
  scope read results. The resolver plugin path makes no PDP call (`cpt-cf-quota-enforcement-fr-resolver-plugin`).

#### Monitoring System

**ID**: `cpt-cf-quota-enforcement-actor-monitoring-system`

- **Role**: Consumes Quota Enforcement telemetry (metrics, health endpoints, structured logs) for dashboards, alerting,
  and operational visibility.

### 2.3 Actor Permissions

| Actor                                              | Permitted Operations                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Denied by Default                                                                                                                                                                                                                         |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-quota-enforcement-actor-platform-operator` | Create and update Quotas; manage Quota Resolution Policies; view utilization across authorized tenants; register notification sinks                                                                                                                                    | Bypassing tenant scope without explicit PDP grant; deleting the seeded global Policy; registering metric types through QE                                            |
| `cpt-cf-quota-enforcement-actor-metric-owner`      | Register the metric type, with its traits and the schema of the metric object's `metadata`, through `types-registry`; change it compatibly | Registering a metric type per caller for its shared metric; using the traits of the metric type as a substitute for PDP permission |
| `cpt-cf-quota-enforcement-actor-quota-manager`     | Acting on behalf of tenant administrators: create, update, and deactivate Quotas within the tenant's scope (subject to operator-imposed bounds and PDP scope); invoke credit and preview; read authorized snapshots | Modifying quotas of other tenants; registering metric types; managing Quota Resolution Policies; bypassing PDP scope of the original caller |
| `cpt-cf-quota-enforcement-actor-quota-consumer`    | Invoke debit, rollback, reserve, commit, release, batch debit, consumer `evaluate_preview`, and consumer snapshot reads for a caller-supplied attribution tuple authorized by PDP; a rollback is authorized for the attribution of the charge it names                                                                                                                                                                                                                                                                                                                 | Invoking management-only operations; operating outside the PDP-authorized tenant, subject, or metric scope                                                                                                                       |
| `cpt-cf-quota-enforcement-actor-quota-resolver`    | Through the resolver plugin, in process: debit, lease acquire, commit, release, return, and quota snapshot reads, each for one tenant, without a PDP check | Policy management; operations outside the request's tenant |
| `cpt-cf-quota-enforcement-actor-quota-reader`      | Perform manager/operator snapshot reads and previews with an explicit target, within PDP-returned scope                                                                                                                                                                                                                                                                                                                                                                                                 | Modifying counters; reading raw operation logs unless explicitly authorized                                                                                                                                                               |
| `cpt-cf-quota-enforcement-actor-subject-manager`   | Signal subject lifecycle events (created, removed, identity changes) to Quota Manager, which translates them into Quota Enforcement CRUD calls. P1: no direct Subject Manager → Quota Enforcement channel — see `cpt-cf-quota-enforcement-contract-subject-manager` (informational) and §4.2 umbrella delegation                                                                                                                                                                                          | Calling Quota Enforcement directly in P1; mutating quota counters or definitions; reading Quota Enforcement state outside Quota Manager-mediated read APIs                                                                                |
| `cpt-cf-quota-enforcement-actor-storage-backend`   | Receive and persist Quotas, counters, Quota Resolution Policies, leases, charges, idempotency records, and operation logs forwarded by the Quota Enforcement gateway plugin                                                                                                                                                                                                                                                                                                                                        | Direct access from any actor other than the authorized storage plugin instance                                                                                                                                                            |
| `cpt-cf-quota-enforcement-actor-notification-sink` | Receive structured events emitted by Quota Enforcement on threshold crossings, period rollovers, lease auto-release, lease resolution by quota deactivation, and definition changes                                                                                                                                                                                                                                                                                                                       | Initiating operations against Quota Enforcement; reading quota state directly                                                                                                                                                             |
| `cpt-cf-quota-enforcement-actor-types-registry`    | Respond to reads of metric types and subject types initiated by the Quota Enforcement gateway                                                                                                                                                                                                                                                                                                                                                                                                             | N/A — passive service; does not initiate operations on Quota Enforcement                                                                                                                                                                  |
| `cpt-cf-quota-enforcement-actor-authz-resolver`    | Respond to PDP authorization queries initiated by the Quota Enforcement gateway                                                                                                                                                                                                                                                                                                                                                                                                                           | N/A — passive PDP service                                                                                                                                                                                                                 |
| `cpt-cf-quota-enforcement-actor-monitoring-system` | Read observability endpoints (`/health/*`, metrics scrape)                                                                                                                                                                                                                                                                                                                                                                                                                                                | Modifying counters or definitions                                                                                                                                                                                                         |

Authorization is enforced via the platform PDP (`authz-resolver`) on every operation of QE's own API; the resolver
plugin path is the only exception (`cpt-cf-quota-enforcement-fr-resolver-plugin`). Unauthenticated requests are
rejected before any authorization check. Authenticated requests have their public request/target shape checked before
PDP; subject admission and validation of the metric object follow authorization. Failures result in immediate
rejection with no partial operation (fail-closed; see §3.4 — "Quota Enforcement itself fails closed on internal
errors").

## 3. Operational Concept & Environment

### 3.1 Subject Model & Attribution

Every Quota is bound to a concrete subject `{type, id}`, stored as `(subject_type, subject_id)`. The type is a Quota
Resolver subject type derived from `gts.cf.core.quota.subj.v1~` (ADR-0008). The metric type lists in its
`admitted_subjects` trait the subject types the metric's quotas may count; a type derived from an admitted type is
admitted, and the tenant subject type is always admitted, so the empty default admits tenant quotas only. An owner
narrows the list when it knows which kinds of subject its metric counts, so that QE rejects a Quota or a request for
another kind. An owner that does not know them, because the operator defines the subject types, admits the base
`gts.cf.core.quota.subj.v1~`, which admits every subject type. A counter is keyed by
`(subject_type, subject_id, metric)`, where `metric` is the metric type id; the `id` of the request's metric object
is not part of the key in P1.

A consumer request supplies `tenant_id` and `subjects: [{type, id}]`, at least one and at most 16, no two of one type;
more is `InvalidArgument` (`SUBJECTS_TOO_MANY`). QE adds the
tenant subject `(tenant subject type, tenant_id)` to every request; a tenant subject named in `subjects` must name
`tenant_id` and is that same subject, and a request that charges only the tenant names it. Callers of one metric
must name the same subject types, so that the applicable-Quota key does not vary by caller and a counter is shared
across services. QE does not enforce this; on the resolver path the Quota Resolver's principal mapping gives it. An operation by user `U` in tenant `T` presents every active Quota of `(user subject type, U)` and
`(tenant subject type, T)` to the Engine, which preserves the user→tenant cascade.

This design has the following consequences that callers and operators must understand:

1. **Tenant Quotas apply to every subject in the tenant.** A Quota whose subject is `(tenant subject type, T)`
   constrains every operation performed within tenant `T`, whichever subjects it names. Each operation's Decision
   arbitrates tenant and subject Quotas via the active Quota Resolution Policy.
2. **Other subject Quotas constrain one subject within its tenant.** A Quota whose subject is `(user subject type, U)`
   constrains only operations that name `U`, and never crosses the tenant boundary that owns it. Under the default
   `most-restrictive-wins` Engine, the `Subject` tier ranks before the `Tenant` tier in binding-Quota selection
   (see §5.9).
3. **Attribution is explicit.** The caller supplies tenant and subjects. On QE's own API they remain untrusted until
   PDP authorizes the complete tuple against the authenticated service principal; on the resolver plugin path the
   Quota Resolver names them and storage confines the operation to the named tenant
   (`cpt-cf-quota-enforcement-fr-resolver-plugin`).
4. **Arbitration logic lives in the Engine, not in Quota records.** The relationship between Quotas of different
   subjects — independence, cascade, split, or any other rule — is determined by the active Quota
   Resolution Engine and its config, not by attributes on the Quota records themselves. Quota records remain pure
   declarative caps; multi-Quota arbitration is a separate concern owned by the Policy/Engine layer.

#### Phase Roadmap for Subject Types

| Phase | Subject types | Resolution mechanism |
|-------|---------------|----------------------|
| P1 | The tenant subject type plus any registered subject type the metric admits (user, device, agent, service account) | Caller names `{type, id}`; QE checks `admitted_subjects` |
| P3 | Subjects backed by resource-group hierarchy, such as `cost-center` or `department` | The Quota Resolver or the charging module derives the subject; QE core is unchanged |

A new subject type needs only a registered type and an admitted entry. The evaluation engine continues to receive an
applicable-Quota set and does not implement registry traversal.

### 3.2 Metric Identity

A metric is a GTS type derived from the Quota Resolver metric base `gts.cf.core.quota.metric.v1~` (ADR-0013). The
Gear that owns the metric registers the type in the platform `types-registry` gear, named under that Gear — e.g.,
`gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_tokens_input.v1~`,
`gts.cf.core.quota.metric.v1~cf.genai.llm_gateway.ai_requests.v1~`. Quota Enforcement mints no metric name and owns no
GTS type for subjects or metrics. The GTS types that describe a Quota stay QE's own, with their instances: the quota
type (§5.3), the period type (§5.4), the enforcement mode type (§5.11), and the source type (§5.2). QE registers only
those.

> **Notation.** Throughout this document, metrics are referenced by short names (`ai-tokens-input`, `vCPU-hours`,
> `storage-bytes`, etc.) in examples and use-case payloads for readability. API requests, storage rows, and outbox
> events use the full GTS type id shown above.

A metric type holds everything QE needs to serve the metric:

- the trait `kind` (`counter` or `gauge`);
- the trait `admitted_subjects` (§3.1);
- the trait `enforcement` (`quota_gated` by default, or `direct`; see "Gated vs Non-Gated Metrics" below);
- a schema that refines the `metadata` of the metric object.

A request names its metric as a metric object `{type, id?, metadata}`. `type` is the metric type. `id` is optional and
names the one thing the amount is counted for, for example a model or a virtual machine. `metadata` holds the
attributes of the operation that Policies read. QE validates the whole metric object against the resolved schema of
the metric type and gives the Engine the metric type and the `metadata`. A Quota names the metric type only.

QE reads a metric type from `types-registry` by its id and keeps what it has read in the metric catalogue
(`cpt-cf-quota-enforcement-fr-subject-type-registry`, ADR-0012). No other registered object decides how a metric is
served. A metric type that the registry does not know is rejected with canonical `InvalidArgument`, and a registry
that cannot be read fails the request rather than skipping the check
(`cpt-cf-quota-enforcement-fr-metric-identity-validation`). The format of the type id is governed by `types-registry`;
QE adds no naming rule of its own. Another major version of a metric type is another type id, so it is another metric
with its own Quotas.

The metric's `kind` (`counter` vs `gauge`) states which quota types the metric takes:

- a counter-kind metric takes **consumption** Quotas (cumulative within period) and **rate** Quotas,
- a gauge-kind metric takes **allocation** Quotas (in-flight reservable capacity).

Creating a Quota of a type that the metric's kind does not take **MUST** be rejected with canonical
`InvalidArgument`.

#### Gated vs Non-Gated Metrics

The `enforcement` trait of the metric type classifies the metric as
`quota_gated` (the default; written `QuotaGated` below) or `direct` (`Direct`) (architectural recommendation from the
Usage Collector review):

- `QuotaGated` — usage of this metric **MUST** flow through Quota Enforcement before any Usage Collector record is
  emitted; the Usage Collector SDK rejects direct `emit()` calls for gated metrics. The integration wrapper performs the
  `preflight_reserve` → work → `settle` flow against Quota Enforcement; only on a successful settlement does the wrapper
  emit the corresponding Usage Collector record.
- `Direct` (non-gated) — usage of this metric is emitted directly via the Usage Collector with PDP-only authorization;
  Quota Enforcement is not consulted. Suitable for monitoring, audit, and analytics metrics that have no enforceable
  cap.

Quota Enforcement stores Quotas for whatever metric an operator declares. The `enforcement` trait determines **the
path** by which usage records reach the Usage Collector, not whether Quota Enforcement participates. In practice:

- Operators typically create Quotas only for `QuotaGated` metrics — Quotas on `Direct` metrics cannot constrain
  consumption.
- **Quotas on `Direct` metrics are inert.** Callers don't traverse QE for `Direct` metrics, so the counter would never
  reliably reflect consumption. Every operation that takes or checks capacity (`debit`, `reserve`, batch-debit,
  `evaluate-preview`) targeting such a Quota **MUST** be rejected with `METRIC_NOT_QUOTA_GATED` — no counter
  mutation, no idempotency / operation-log / lease record. An operation that gives capacity back or names an existing
  Quota, lease, or charge (`credit`, `rollback`, `return`, `commit`, `release`) **MUST NOT** be rejected for this
  reason: a metric type can change to `Direct` while leases and charges exist (ADR-0012). Quota Snapshot reads
  still succeed so operators can clean up. Create is permitted (the Quota stays inert); while such a Quota stays
  active it is counted by the `quota_for_direct_metric_total` gauge.
- **A `QuotaGated` metric with no matching Quota for the operation's subject is rejected.** Quota Enforcement returns
  `Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` — `QuotaGated` means every consumption requires explicit
  quota authorization, so absence of an applicable Quota is treated as absence of permission, not as unconstrained
  freedom. Operators are expected to provision Quotas (typically via subject-manager hooks at tenant/user creation)
  before any consumption can occur.

The detailed `preflight_reserve` → `settle` flow and the SDK rules that enforce the gating contract are **not** part of
this PRD and are not part of the Usage Collector specification either. They will live in a separate integration
component — either a dedicated QE↔UC wrapper authored alongside Usage Collector, or, in a later major release that
consolidates QE and UC responsibilities, as part of the merged surface. This PRD acknowledges the boundary so that QE's
primitives and vocabulary stay aligned with whichever shape the integration eventually takes.

### 3.3 Integration Boundary with Usage Collector

Quota Enforcement and the Usage Collector are intentionally decoupled:

- Quota Enforcement owns its own internal counters and ledger; it does not query Usage Collector at evaluation time.
- The Usage Collector owns the canonical record of consumption events; it does not know about quota caps or denials.

The integration component (out of scope for this PRD) is responsible for the `QuotaGated` flow
described in §3.2: it calls Quota Enforcement's `reserve`/`commit`/`release` (or `debit`/`rollback`) primitives, then —
on successful settlement — emits the corresponding Usage Collector record carrying a settlement proof that the Usage
Collector SDK validates before persisting the usage. For `Direct` metrics, the wrapper is not involved: callers emit
directly to the Usage Collector with PDP-only authorization. Operators who deploy Quota Enforcement without the wrapper
still receive a fully functional standalone counter/ledger system — calling services invoke Quota Enforcement's
primitives directly without Usage Collector involvement.

### 3.4 Decision Contract for Calling Services

Every Quota Enforcement evaluation operation returns one of two outcomes:

1. A deterministic **`Decision`** (a verdict produced by the active Quota Resolution Engine, §5.9), or
1. A platform-canonical **error** (when the system could not produce a Decision at all — Engine timeout, mid-flight
   infrastructure unavailability, malformed Engine output, invariant violation).

The `Decision` shape carries:

- `result` — one of:

  - `Allowed` — operation is permitted within every Quota's cap.
  - `Denied(violated_quota_ids, reason)` — at least one Quota would be exceeded; counters are not modified. Every
    violating Quota is named (no short-circuit).

- `debit_plan` — a list of chains naming exactly which Quotas to mutate and how (§1.4 Debit Plan). Each entry carries
  `quota_id` and `amount` (≥ 0), the counter mutation for that Quota. Future per-entry fields MAY be added without
  breaking the top-level Decision shape. Counters are mutated strictly per `debit_plan` when `result = Allowed`.
  `Denied` produces an empty `debit_plan` and no counter mutations.

- `diagnostics` — Engine-supplied per-Quota detail (current consumed, cap, contribution to the decision, Engine-specific
  notes) so callers can render breakdowns without recomputing Engine logic.

Built-in `most-restrictive-wins` Engine produces a `debit_plan` of one chain with one entry against the binding Quota
at `amount = request.amount`; Quotas not in the plan remain unchanged. A Quota is satisfiable if remaining ≥
`request.amount` (unbounded Quotas are trivially satisfiable). The binding Quota is selected from the satisfiable set by, in
priority order:

1. **Subject tier** — the `Subject` tier before the `Tenant` tier.
2. **Bounded > unbounded** within the chosen tier — operator's explicit cap is enforced first; unbounded acts as
   overflow when no bounded Quota in the tier is satisfiable.
3. **Smallest remaining** within bounded satisfiable Quotas of the chosen tier (literal "most restrictive"); ties broken
   by ascending `quota_id`. Among unbounded Quotas of the chosen tier (selected only when no bounded satisfiable exists
   in that tier), ascending `quota_id` is the sole tiebreaker.

`Denied` is returned when no Quota is satisfiable — every applicable bounded Quota has remaining capacity below
`request.amount` and no applicable unbounded Quota exists; `violated_quota_ids` enumerates every such bounded Quota.

Custom Engines (`cel` in P1; `starlark`/Wasm/etc. in P2 or later) MAY produce chains of several entries for cascade
splits and weighted distributions, and several chains for AND across Quotas; the Debit-Plan invariants apply uniformly
at the Engine boundary — see `cpt-cf-quota-enforcement-fr-quota-resolution-engine`.

**Failure surface.** When the Engine cannot produce a valid Decision — Engine timeout, Engine cost-cap exhaustion,
Engine internal failure, malformed Debit Plan, Debit-Plan invariant violation, mid-flight PDP / storage failure — the
operation **MUST NOT** mutate counters and **MUST** surface a platform-canonical error (per the AIP-193 categories
implemented by `cf-toolkit-canonical-errors`; specific category mapping is a DESIGN concern). These are operational
failures, not verdicts, and never appear as a `Decision` arm. A failure-shaped response is mutually exclusive with the
`Decision` shape — every evaluation operation returns either a `Decision` (`Allowed` / `Denied`) **or** a canonical
error, never both.

**Forward compatibility.** The two-arm `result` is a strict subset of any future expanded shape. A future P3
introduction of `AllowedWithClamp(quota_id, admitted_magnitude)`, intended for batch-style workloads, would be additive:
P1 consumers continue to handle only `Allowed` / `Denied`, and the would-be-clamped case is conservatively reported as
`Denied` until clamp mode is adopted.

**Trust boundary — Decision is server-derived, response-only.** The Decision (`result`, `debit_plan`, `diagnostics`) is
computed entirely server-side by the active Engine and returned to the caller for application of the verdict and for
diagnostic rendering. The Decision and every field within it **MUST NOT** be supplied, modified, or echoed back by the
caller — neither on the originating request nor on any subsequent request. Request DTOs do not carry Decision-shaped
fields by type design; if such fields appear in a request body (e.g., an over-eager client echoing back the response
shape), the server silently ignores them. This mirrors the authenticated-principal and PDP-authorized-attribution
discipline in §5.13. Replay safety is achieved exclusively via the client-supplied idempotency key (§5.8); on replay, the original
outcome (Decision or error) is the canonical response — never anything the client resubmits. This boundary prevents a
misbehaving or malicious caller from skipping a Quota's mutation, redirecting it to a different Quota, replaying a stale
plan against a now-different applicable-Quotas set, or otherwise bypassing Engine arbitration. Tokens that *do*
round-trip between client and server (idempotency keys, lease tokens, operation IDs) are opaque server-issued or
client-supplied identifiers — never structured Decision fields.

The calling service interprets the outcome according to its own policy: `Denied` is propagated to the user as
`429 Too Many Requests` or domain-specific equivalent; a canonical error triggers the calling service's fallback
behavior, which may be fail-open or fail-closed at the calling service's discretion (the canonical category — e.g.,
`DeadlineExceeded` vs `Internal` vs `ServiceUnavailable` — informs that choice). Quota Enforcement itself fails closed
on internal errors (surfaces a canonical error, never silently allows). **Commercial overage pricing** (a tenant exceeds
an entitled allowance and is charged premium rates for the overage) is **not modeled in the Decision contract**; per
§4.2, this is the billing service's responsibility, composed downstream of QE from Usage Collector observations and
Quota records.

## 4. Scope

### 4.1 In Scope

- Quota lifecycle (create, update, deactivate, read) — single first-class entity bound to a subject at creation time
- Metric types read from `types-registry`; Quota Resolver subject types admitted per metric by its metric type;
  validation of the request's metric object; the check of Quota metadata against the metric type; fail-closed handling of a wrong
  metric type
- Caller-named subjects, PDP-authorized on QE's own API
- Allocation and consumption quota types (rate type declared as future)
- Calendar-aligned UTC time periods with deterministic rollover semantics
- Idempotent debit, credit, rollback, and return operations over a charge ledger
- Two-phase lease primitive (reserve/commit/release with TTL auto-release)
- Bulk / batch debit with envelope idempotency: atomic all-or-nothing for multi-metric admission (P1); partial-success
  for bulk-independent operations (P2)
- Multi-quota evaluation against the applicable-Quotas set
- Quota Resolution Policy as a separate operator-managed entity selecting a pluggable Quota Resolution Engine; default
  platform-wide policy uses the built-in `most-restrictive-wins` Engine; operator-configurable narrower-scope Policies
  may use any registered Engine
- Quota Resolution Engine plugin contract with P1 built-ins: `most-restrictive-wins` (hardcoded; fastest path) and `cel`
  (sandboxed; customizable). Cascade/spillover is expressible via the customizable Engine in P1, and attribute-gated
  caps through the conditions of Quota metadata under every Engine; additional Engines (Starlark, Wasm-loaded, …) plug in via the same contract in later phases without
  changes to the evaluation core
- Quota Snapshot read APIs (single subject, bulk subjects, end-user self-service through the Quota Resolver)
- Quota enforcement mode; in P1 supports only strict rejection at the cap boundary; future modes are added as new GTS
  instances per `cpt-cf-quota-enforcement-fr-enforcement-mode`
- Tenant isolation on every read and write
- PDP-gated authorization on every operation of QE's own API
- Quota Resolver backend plugin (`cpt-cf-quota-enforcement-fr-resolver-plugin`)
- Pluggable storage backend (P1: `toolkit-db`-based plugin; alternative backends viable under the same plugin contract)
- Notification plugin contract emitting the event catalog defined in `cpt-cf-quota-enforcement-fr-notification-plugin`
- Operational telemetry (gear-specific counters, histograms, and gauges; baseline observability via the platform
  framework)
- P1 ships a closed `source` enum on every Quota with values `licensing` (default; quotas materialized from the
  licensing layer) and `operator` (manual caps — incident response, compliance carve-outs, soft-launch placeholders).
  Both follow the same mutation rules: operator-level or Quota-Manager PDP scope. Future capability for
  tenant-administrator-imposed (`tenant_admin`) and end-user-self (`user_self`) Quotas with per-source mutation rules
  and cap-inheritance — together with subscriber-customizable notification thresholds — is a P2 enhancement; see §13.

### 4.2 Out of Scope

**Quota management and provisioning — umbrella delegation to Quota Manager.** Every concern in this group is **out of
scope for Quota Enforcement** and **owned by Quota Manager** (a separate platform component,
`cpt-cf-quota-enforcement-actor-quota-manager` in §2.2; its own PRD is authored independently). Quota Enforcement
exposes the primitives (Quota CRUD, debit/credit/rollback, lease, Quota Snapshot read, notifications) that Quota Manager
composes into management workflows. The boundary is normative: any new management/provisioning capability the platform
decides to ship belongs in Quota Manager unless and until a future PRD revision explicitly relocates it.

The umbrella covers:

- **License-pack catalog and plan templates** — operator definitions of named bundles ("Free", "Pro", "Enterprise"; "10
  GPU-hours / month / user"; "1 TB egress / tenant / month"; etc.) and the rules for materializing concrete Quotas from
  them. Quota Enforcement provides only the per-Quota CRUD primitive; bundling, templating, and plan-driven
  instantiation are Quota Manager's concern.
- **Subject-creation provisioning** — automatically attaching a plan to a freshly-created subject (tenant, user,
  cost-center, …) so that Quotas appear without operator hand-roll. Subject managers (e.g., `account-management`) signal
  subject lifecycle to Quota Manager; Quota Manager calls Quota Enforcement's CRUD on the resulting Quotas.
- **Redistribution / reallocation** — moving cap from one user to another within a tenant, splitting a tenant top-up
  across users, draining a deactivated user's allocation back into the tenant pool, etc. Composed in Quota Manager from
  existing Quota Enforcement update/credit primitives; Quota Enforcement does not introduce a "redistribute" verb.
- **Quota increase-request workflow** — self-service raise + operator approval lifecycle, including the request entity,
  the approval state machine, the cap-bump audit trail, and the human-facing surfaces. Owned by Quota Manager (replaces
  the earlier "deferred / wrapper component or workflow gear" framing — see §13).
- **License purchase / renewal / expiry / revocation hooks** — translating commercial events into Quota lifecycle
  changes (create, update cap, deactivate, reactivate). Owned by Quota Manager; Quota Enforcement merely receives the
  resulting CRUD calls.
- **Tenant-administrator UI surfaces** — assigning caps within a tenant, reviewing utilization, reacting to threshold
  notifications, and configuring per-user caps within operator-imposed bounds. Owned by Quota Manager.
- **End-user UI surfaces** — self-service Quota Snapshot rendering, "explain why this was denied" explanations,
  request-to-raise affordances. Owned by Quota Manager; Quota Enforcement provides only the underlying snapshot read
  (`cpt-cf-quota-enforcement-fr-end-user-quota-snapshot-read`).
- **Pricing, rating, billing rules, invoice generation** — quota counters are dimensionless integer/decimal values, not
  currency amounts. Owned by the billing layer (separate from Quota Manager and from Quota Enforcement).
- **Cost-based quotas in monetary units** — P1 Quotas count metric units (tokens, bytes, hours), not cost.
  Cost-translated caps, if needed, are computed in Quota Manager / billing and materialized as metric-unit Quotas in
  Quota Enforcement.
- **Commercial overage and overage pricing** — when a tenant's premium product permits consumption above an entitled
  allowance and is charged premium rates for the overage, this is the **billing service's** responsibility, composed
  downstream of QE from Usage Collector observations and Quota records. QE itself does not admit operations above cap;
  operators wanting "smooth" premium-tier UX express it by raising the cap (or setting `cap = null` for an unbounded
  Quota per `cpt-cf-quota-enforcement-fr-quota-lifecycle`) on premium tenants and configuring the billing service to
  apply per-unit pricing above an entitled allowance. This division keeps the QE admission contract simple and
  concentrates pricing logic in the billing layer where catalog, currency, taxation, and tier definitions already live.
- **Operator approval workflows** — multi-step approvals for cap raises, plan migrations, redistribution between
  subjects, etc. Owned by Quota Manager.
- **Plan migration / customer-tier change** — moving a tenant from "Free" to "Pro" and rewriting its Quotas accordingly.
  Owned by Quota Manager.

The remaining out-of-scope items below are not management/provisioning but distinct exclusions retained for
completeness:

- **Usage Collector integration** — handled by a separate wrapper component (out of scope).
- **Cross-tenant quotas** — every quota is tenant-scoped; cross-tenant arrangements are explicitly excluded.
- **User-facing UI** — only REST and SDK surfaces are exposed by Quota Enforcement itself; all UIs (operator, admin,
  end-user) are the responsibility of consuming products and Quota Manager.
- **Audit logging** (consumption events and definition changes) — deferred to P2 when the platform audit infrastructure
  is available.
- **Subject hierarchy via resource-group** — P3 only; P1 counts each subject type directly without traversal.
- **Migration between versions of a metric** — another major version of a metric type is another metric with its own
  Quotas (`cpt-cf-quota-enforcement-fr-quota-lifecycle`); P1 provides no alias or Quota/counter migration operation.
- **Grace policies for hard caps** (e.g., +N% for T minutes during a campaign or incident before resuming the strict
  cap) — deferred to P2.
- **Rate quota implementation** (token bucket / sliding window) — declared as a future quota type; P1 implements only
  allocation and consumption.
- **EventBus integration** — P1 emits events through the in-process notification plugin; standardized EventBus routing
  is P2.
- **Multi-region replication** — single-region deployment; multi-region deferred (target resolution: production demand).
- **Per-resource counter axis** — the `id` of the metric object names the one thing the amount is counted for and
  reaches the PDP, the Engine, and the charge's attribution, but not the P1 counter key; §13 records the decision criterion for adding that
  axis.
- **Principal-to-subject mapping** — done by the Quota Resolver; QE receives only `{type, id}` subjects.
- **Business-semantic property validation** — a metric type validates the shape of the metric object; acceptable
  business values and their meaning remain with the Engine/Policy layer.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code
> coverage unless otherwise specified.

### 5.1 Metric Types & Subject Attribution

#### Metric Types

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-projection-contracts`

A metric **MUST** be a GTS type derived from the Quota Resolver metric base `gts.cf.core.quota.metric.v1~`, registered
through `types-registry` by the Gear that owns the metric (ADR-0013). QE **MUST** use the Quota Resolver SDK's quota
subject base `gts.cf.core.quota.subj.v1~`, tenant subject type, and metric base (ADR-0008). QE **MUST NOT** define or
register a GTS type for subjects or metrics; it registers only its own types that describe a Quota (§3.2). This
requirement adds no QE endpoint.

A metric type **MUST** set the trait `kind` and **MAY** set the traits `admitted_subjects` and `enforcement`:

- `kind` is `counter` or `gauge` (§3.2);
- `admitted_subjects` lists the subject types the metric's quotas may count. A type derived from an admitted type is
  admitted. The tenant subject type is always admitted, so the empty default admits tenant quotas only. A metric type
  **MAY** admit the base `gts.cf.core.quota.subj.v1~`, which admits every subject type, so an owner never needs to
  name a subject type that the operator defines;
- `enforcement` is `quota_gated` (the default) or `direct` (§3.2).

The schema of the metric type refines the `metadata` of the metric object. A request **MUST** name its metric as one
metric object `{type, id?, metadata}`; the object has the same form for all callers and subject types. A Quota
**MUST** name a subject and a metric type. The counter key **MUST** be `(subject_type, subject_id, metric)`, `metric`
being the metric type id. The `id` of the metric object **MUST NOT** enter the counter key in P1. The PDP and the
Engine receive it, so a Policy can allow, deny, or choose Quotas by it, and the attribution of a charge includes it. A caller **MUST NOT** register a metric type of its own for a metric that it shares with
other callers. Another major version of a metric type is another metric with its own Quotas.

- **Rationale**: One owner-registered type per metric and one subject vocabulary shared with the Quota Resolver
  preserve one cross-service counter. A request and a Quota name every object that QE reads, so a registration by
  another module cannot change how a metric is served, and the registry is a reviewable inventory of metered metrics,
  their properties, and their admitted subject types.
- **Actors**: `cpt-cf-quota-enforcement-actor-metric-owner`,
  `cpt-cf-quota-enforcement-actor-quota-consumer`, `cpt-cf-quota-enforcement-actor-types-registry`

#### Request Validation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-contract-validation`

At ingress of every subject-based consumer evaluation and snapshot operation, including each batch item, QE **MUST**
validate `tenant_id`, every `subjects[{type,id}]` entry, and the metric before Engine dispatch. An evaluation
operation carries a metric object, and QE **MUST** validate the whole object against the complete resolved schema of
its metric type, the rules of the metric base included. A snapshot read names metric types only. An empty subject
list, two subjects of one type, a tenant subject whose id is not `tenant_id`, a subject type that is not registered or
not admitted, a metric type that is not registered, absent `metadata`, or a schema violation **MUST** receive a
canonical `InvalidArgument` error. The one exception is the snapshot read on the resolver plugin path: for a metric
that does not admit the subject's type it returns only the tenant's Quotas, not an error
(`cpt-cf-quota-enforcement-fr-quota-snapshot-read`). QE **MUST NOT** default absent metadata to `{}` and **MUST NOT**
encode validation failure as `Decision::Denied`.

At Quota create and at a change of its metadata, QE core **MUST** check the Quota metadata against the schema of the
metric type (`cpt-cf-quota-enforcement-fr-quota-metadata`). QE **MUST NOT** check stored Quota metadata again on the
evaluation path.

Evaluation **MUST NOT** call `types-registry` for a metric whose catalogue entry QE already holds
(`cpt-cf-quota-enforcement-fr-subject-type-registry`), except to read the metric type again when the request's metric
object does not fit the schema of that entry. A metric type validates shape only and must not contain
secrets. `types-registry` admits a change of a registered metric type under the same id only when the new schema
accepts every instance that the previous one accepted; adding an optional property is such a change. A change that
the registry does not admit needs another major version, which is another metric. The registry does not apply this
rule to a type whose own version is major 0, so a metric type **MUST** have a version of major 1 or higher. A change
of a trait takes effect within one read interval, on new writes and new requests; no change of a metric type makes a
metric unavailable because of a stored Quota or Policy (`cpt-cf-quota-enforcement-fr-subject-type-registry`). ADR-0013 defines the metric object, ADR-0012 the reads of a
metric type, and ADR-0008 the subject admission.

- **Rationale**: A validated metric object eliminates silent attribute misses while keeping registry latency and
  availability outside the evaluation budget.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`,
  `cpt-cf-quota-enforcement-actor-platform-operator`

#### Metric Catalogue

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-subject-type-registry`

The **metric catalogue** is QE's process-local record of the metric types it has read from `types-registry`: one entry
per metric type, with the resolved schema and the traits (ADR-0012). QE has no subject-type table or resolver trait.
QE **MUST** reject Quota creation for a subject type that is not registered, not derived from
`gts.cf.core.quota.subj.v1~`, or not admitted by the metric type.

An entry is **served**, and QE evaluates requests for its metric, or **unavailable**. An entry **MUST** be unavailable
when its type is not a valid metric type: it is abstract, it does not derive from the metric base, its own version is
major 0, or a trait is wrong, for example an admitted subject type that is not a quota subject type.

QE **MUST** build an entry from the registry content only, and **MUST NOT** make a metric unavailable because of a
stored Quota or a stored Policy. A change of a metric type that stops admitting a
subject type leaves the Quotas of that type stored: a request that names the type receives `InvalidArgument`, so
those Quotas match no request, and nothing is allowed without a limit. They can be deactivated, and they match
requests again when the type is admitted again. When a changed metric type and an active Policy do not fit, the
Engine reports an error at evaluation, and QE **MUST** reject the request with `POLICY_EVALUATION_FAILED` (canonical
`FailedPrecondition`), change no counter, and count the rejection in telemetry. A caller **MUST NOT** treat this
answer as an outage of QE: a retry does not pass until the operator changes the Policy
(`cpt-cf-quota-enforcement-fr-quota-resolution-policy`); the operator then writes a Policy that fits.

QE **MUST** read a metric type from the registry by its id. It **MUST NOT** depend on a listing of registered types or
on any registered object that the request, the Quota, or the Policy does not name. QE **MUST** read at start the
metric types of the active Quotas and Policies. It **MUST** read any other metric type when a request or a Quota or
Policy write names it for the first time, and that read **MUST** have a time limit. A metric type that the registry
does not know **MUST** receive canonical `InvalidArgument`; QE remembers that answer for a short time, so repeated
requests for an unknown metric do not call the registry each time. An id that by its form is not derived from the
metric base or from the subject base **MUST** receive `InvalidArgument` with no registry call, and QE's memory of
unknown ids **MUST** have a size limit, so that a caller that names many ids cannot grow QE's memory or its registry
reads without limit. A read that fails or reaches its time limit
**MUST** receive canonical `ServiceUnavailable`, which a caller may repeat.

An owner can change a registered metric type under the same id, or delete it. QE **MUST** therefore ask the registry
on a bounded schedule whether each metric type that the catalogue holds, and each registered subject type that QE has
cached, changed. A scheduled read **MUST** build again each entry whose type changed,
remove the entry of a deleted metric type, and drop a deleted subject type; it **MUST NOT** read storage. A scheduled read that fails or reaches its time limit **MUST** change no entry: a
served entry stays served. A change in the registry **MUST** reach
every replica within one read interval of a scheduled read that succeeds, and a running replica **MUST** then hold the same entry as a replica started
after the change.

A request for a served metric uses the entry and **MUST NOT** call the registry for it, except to read the metric
type again when the request's metric object does not fit the entry's schema. A request for an unavailable
metric **MUST** receive canonical `FailedPrecondition` and change nothing; it is never an `Allowed` or a `Denied`
Decision. This stops debit, lease acquisition, batch debit, preview, and the snapshot read; a snapshot read that names
several metrics fails when one of them is unavailable. Credit, lease commit, lease release, return, and rollback name
an existing Quota, lease, or charge and **MUST** run while the metric is unavailable, so held capacity can always be
given back. After a metric type is deleted, a new request for its metric receives `InvalidArgument`, and credit,
commit, release, return, and rollback still run.

Quota and Policy writes use the same entry. A Quota create or change and a Policy create or change for a metric with
no served entry **MUST** be rejected: with `InvalidArgument` when the metric type is not registered, and with
`PROJECTION_NOT_RESOLVABLE` (canonical `FailedPrecondition`) when the entry is unavailable. A Quota deactivation, a
Policy rollback, and a Policy delete make no catalogue check and **MUST** always run.

A wrong metric type **MUST NOT** fail bootstrap or change readiness; bootstrap fails on QE's own state and
configuration (storage, schema version, and Engines). While `types-registry` does not answer the catalogue read at
start, QE is not ready and repeats bootstrap until it answers. QE **MUST** report each unavailable metric with its
reason in a
log entry and in telemetry.

- **Rationale**: Registered metric types provide discoverable identity and shape, and a validated local catalogue
  keeps the registry off the evaluation path. Modules start in any order, so a metric registered after QE started is
  served without a restart, and a wrong metric type of one module stops only the requests that name its metric.
- **Actors**: `cpt-cf-quota-enforcement-actor-metric-owner`

#### Subject Attribution at Evaluation Time

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-subject-resolution`

Every subject-based consumer evaluation request **MUST** carry `tenant_id`, `subjects: [{type, id}]` with at least one
entry, and one metric object. A subject `id` is an opaque non-empty string. A tenant subject **MUST** name
`tenant_id`; it is the same subject as the implicit tenant, which QE adds to every request. On QE's own API, QE
**MUST** first reject malformed public request shape: missing required fields, wrong public container types, empty
ids, an empty subject list, two subjects of one type, or a tenant subject of another tenant. It **MUST** then ask PDP
to authorize the complete caller-supplied attribution — the tenant, the subjects, and the metric with its type and
optional id — against the authenticated service principal. Only after authorization may QE check subject admission,
read a metric type, or validate the metric object. The resolver
plugin path runs the same checks without the PDP step (`cpt-cf-quota-enforcement-fr-resolver-plugin`).

QE **MUST** fetch every active Quota of the metric for the resulting `(subject_type, subject_id)` set, implicit tenant
included, and forward the complete set to multi-quota evaluation. A subject type that the metric does not admit fails
before evaluation. An operation with no matching Quotas is evaluated against an empty set and returns
`Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` per §3.2.

- **Rationale**: On QE's API, PDP authorization prevents a caller from charging another tenant or subject. On the
  plugin path the Quota Resolver names the tenant and storage confines the operation to it.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`, `cpt-cf-quota-enforcement-actor-quota-resolver`

### 5.2 Quota Lifecycle

#### Quota Lifecycle: Create, Update, Deactivate, Read

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-lifecycle`

The system **MUST** support a declarative lifecycle for Quotas: create, update, deactivate, and read. A Quota is a
single first-class entity that is bound to its subject at creation time; there is no separate definition-template or
assignment-binding concept. Each Quota **MUST** carry:

- a unique stable quota ID (server-assigned),
- a subject reference `(subject_type, subject_id)` — a quota subject type plus opaque subject id, bound at creation
  and immutable for the lifetime of the Quota,
- a reference to a metric type registered in `types-registry` (validated as described in
  `cpt-cf-quota-enforcement-fr-metric-identity-validation`),
- a quota type (`allocation` or `consumption`; `rate` is reserved for future use and is rejected at creation time in P1
  per `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection`),
- a period specification: consumption types **MUST** carry an explicit valid period, the non-recurring `one_time`
  included, and a missing or null period is rejected (`PERIOD_REQUIRED`); allocation types **MUST** reject any
  period field, a null period included (`PERIOD_NOT_ALLOWED`),
- an `enforcement_mode` value (the Quota's behavior at the cap boundary); in P1 only strict rejection at the cap
  boundary is supported; future modes are added as new GTS instances per `cpt-cf-quota-enforcement-fr-enforcement-mode`,
- a cap (units defined by the metric; either a non-negative integer or `null` for an **unbounded** Quota; negative caps
  are rejected at create/update with `CAP_MUST_BE_NON_NEGATIVE`; `cap = 0` and `cap = null` are both explicitly valid —
  see "Cap value semantics" below). All P1 metric values are integer; metric authors registering inherently fractional
  concepts (e.g., currency, fractional CPU-hours) **MUST** denormalize to a sufficiently small integer unit (cents,
  milliCPU-seconds) before registration in `types-registry`,
- an optional set of notification thresholds expressed as percentages of cap (e.g., `[50, 80, 100]`); thresholds are
  rejected at create/update on unbounded Quotas (`THRESHOLDS_REQUIRE_BOUNDED_CAP`) — percentages of `null` are
  meaningless,
- an optional validity window (start and end timestamps; when absent, the Quota has no time bounds and remains evaluable
  until explicitly deactivated),
- an optional failure-mode hint (`fail-closed` default, `fail-open` opt-in) — informational metadata for callers,
- an optional `metadata` JSON object — the Quota's conditions over the request's metadata (region, tier,
  environment, …), checked against the metric type when written (`cpt-cf-quota-enforcement-fr-quota-metadata`),
- a `source` value identifying who imposed the Quota; see **Source value semantics** below.

The system **MUST** reject creation of a Quota whose subject type is not registered, not derived from
`gts.cf.core.quota.subj.v1~`, or not admitted by the metric type, and **MUST** reject a tenant subject whose id is not
the Quota's tenant.

**Source value semantics.** `source` instances live under base `gts.cf.core.qe.source_type.v1~`. P1 seeds two:
`gts.cf.core.qe.source_type.v1~cf.core.qe.licensing.v1` (default; caps materialized from the licensing layer) and
`gts.cf.core.qe.source_type.v1~cf.core.qe.operator.v1` (caps an operator created manually outside the licensing
flow — incident response, compliance carve-outs, soft-launch placeholders). A stored Quota's `source` value never
changes silently. Mutation rules in P1 are uniform across both source values (operator-level or Quota-Manager PDP
scope; see the mutation paragraph below); P2 source-kind extensions (`tenant_admin`, `user_self`, …) and per-source
cap-inheritance and mutation rules land with the P2 Source Registry FRs tracked in §13.

> **Notation.** Throughout this document, source values are referenced by their short instance names (`licensing`,
> `operator`) in examples and use-case payloads for readability. API requests, storage rows, and outbox events
> use the full GTS URI form shown above.

**Multiple Quotas per `(subject, metric)` are permitted.** The system **MUST NOT** reject Quota creation on the basis
that another active Quota already exists for the same `(subject, metric)` pair. When several Quotas share the same
`(subject, metric)`, all of them enter the applicable-Quotas set at evaluation time and are resolved through multi-quota
evaluation (`cpt-cf-quota-enforcement-fr-multi-quota-evaluation`) under the active Quota Resolution Policy — by default
`most-restrictive-wins` (per `cpt-cf-quota-enforcement-fr-quota-resolution-engine`). Typical reasons operators
legitimately create more than one Quota per `(subject, metric)`:

- attribute-gated caps (per-region, per-tier, per-environment caps differentiated by the conditions of their Quota
  Metadata, per `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`),
- promotional or campaign-window caps living alongside steady-state caps (resolved through validity windows and Engine
  logic),
- license composition where multiple entitlements contribute caps to the same metric (operator-managed: see §4.2 —
  composition into a final cap is the licensing/billing layer's concern; QE simply hosts the resulting Quota records).

Operators (and Quota Manager flows acting on their behalf) are responsible for ensuring the declared set of Quotas
matches the intended arbitration shape — accidentally duplicate or contradictory Quotas are not rejected by QE; their
effect is observable through evaluation outcomes and the per-Quota detail surfaced in Decision diagnostics.

Operators who need the same shape on many subjects create one Quota per subject (typically driven by the subject manager
at subject-creation time); bulk template creation is out of scope (see §4.2).

Quota deactivation **MUST** retain the Quota record for read access but **MUST** stop accepting new debits or leases
against it; a deactivated Quota still accepts returns and rollbacks (`cpt-cf-quota-enforcement-fr-return`). Active
leases against the deactivated Quota **MUST** be marked as * *resolved-by-deactivation*\* atomically with the
deactivation transaction; subsequent `commit` or `release` calls against those leases **MUST** return
`LEASE_NOT_ACTIVE`. The deactivation timestamp serves as the implicit lease-resolve event for telemetry (no held
capacity remains against the deactivated Quota's counter); a `lease-resolved-by-deactivation` notification event (one
of the catalog kinds in `cpt-cf-quota-enforcement-fr-notification-plugin`) **MUST** be emitted for each affected lease
with the lease ID, the tenant, held amount, and the deactivated `quota_id`; the subject is the deactivated Quota's own. Updates **MUST** preserve the
quota ID and the subject reference; breaking changes (changing metric, type, period, enforcement mode, or subject)
**MUST** be performed by deactivating the original Quota and creating a new one.

Another major version of a metric type is another metric with its own Quotas and counters. P1 provides no alias and
no Quota/counter migration operation between two metrics. A Quota or Policy write for a metric with no served
catalogue entry **MUST** be rejected, with `PROJECTION_NOT_RESOLVABLE` when the entry is unavailable
(`cpt-cf-quota-enforcement-fr-subject-type-registry`).

**Cap reductions guard.** A Quota update that would reduce `cap` strictly below the Quota's current consumed amount (
consumption-type Quotas, within the active period) or current in-flight count (allocation-type Quotas) **MUST** be
rejected with an actionable `CAP_BELOW_CONSUMED` error. Operators wanting to reduce a cap below current usage **MUST**
first issue credits (`cpt-cf-quota-enforcement-fr-credit`) to bring consumption to or below the desired cap, then reduce
the cap. Cap raises (including any update from numeric → `null`) and updates where the new cap is greater than or equal
to current usage are not affected by this guard. Updates from `null` → numeric are subject to the same guard against the
current consumed amount. The cap-vs-consumed comparison **MUST** be evaluated at the moment the update transaction
commits, not at request-receipt time, to avoid TOCTOU races with concurrent debits.

**Cap value semantics.** Two boundary cases are explicitly valid:

- `cap = 0`: a `hard` Quota with `cap = 0` denies every debit against it (any non-zero amount exceeds remaining = 0).
  Useful as an explicit "deny everything for this `(subject, metric)`" Quota — placeholder created at subject
  provisioning before the operator decides on a real cap, or a deliberate cutoff during an incident response.
- `cap = null` (**unbounded**): the Quota imposes no upper bound. It is **always satisfiable** — every applicable-set
  evaluation that would otherwise debit against it produces a non-violating contribution and the operation proceeds.
  Counters still increment on debit/commit (so usage telemetry, audit trail, and a later migration to a real numeric cap
  all work without backfill); `remaining` is reported as `null` in Quota Snapshots, indicating "no upper bound". For
  consumption-type unbounded Quotas, period rollover (`cpt-cf-quota-enforcement-fr-period-rollover`) applies uniformly
  with bounded Quotas: `consumed` resets to zero at the period boundary and a `period-rollover` event is emitted
  carrying the closing-period `consumed` (closing-period `cap` is `null`); `remaining` stays `null` across the boundary.
  Long-term cumulative telemetry is reconstructed by downstream consumers from the rollover events, not from the live
  counter. Useful for premium-tenant "no commercial cap", internal/platform subjects, soft-launching a `QuotaGated`
  metric before per-tenant caps are configured, and bridging cap-bump approval workflows. Notification thresholds are
  not allowed on unbounded Quotas (per the field constraints above) — percentages of `null` are meaningless.

The system **MUST** surface the telemetry gauges `quota_cap_zero_total` and `quota_cap_unbounded_total` counting the
Quotas with lifecycle status `active` whose `cap = 0` and `cap = null` respectively, whatever their validity window. The system **MUST NOT** auto-reject either value — operators
are responsible for the choice.

**Validity-window semantics — Engine-driven, with a default exclusion.** A Quota's `validity_window` is a structural
field exposed in the Engine EvaluationContext (`cpt-cf-quota-enforcement-fr-quota-resolution-engine`); the Quota
Enforcement core does **NOT** auto-deactivate a Quota when `now() > validity_end`. Lifecycle (active vs deactivated) and
validity-window bounds are independent dimensions:

- The Quota record persists with its `validity_window` until explicitly deactivated (per §5.2 deactivation rule;
  hard-delete of deactivated Quotas is a P2 candidate per §6.2 / §13).
- At evaluation time, the active Engine sees the Quota's `validity_window` alongside `time`; the **default** behavior of
  the built-in `most-restrictive-wins` Engine is to **exclude** Quotas whose `time` falls outside
  `[validity_start, validity_end]` (i.e., not-yet-valid or expired Quotas are skipped from the Debit Plan).
- Operator-authored Policies (via the `cel` Engine in P1) **MAY** override this default — for example, to implement
  tier-conditional cutoffs or expected-window matching against `request.expected_window` (the canonical
  client-supplied attribute for "I'm operating as-of this time window; only consider Quotas valid for it"). The PRD does
  not mandate any single behavior — the choice is Engine-side.
- Active leases acquired within a Quota's valid window remain honorable across the validity boundary; see
  `cpt-cf-quota-enforcement-fr-lease-commit` for cross-boundary commit semantics.
- Quota Snapshot reads of QE's own API (`cpt-cf-quota-enforcement-fr-quota-snapshot-read`) **MUST** surface
  `validity_window` and a server-computed boolean `currently_within_window` so callers can render expiry state without
  recomputing the comparison.

This treatment keeps validity-window flexible: operators who want hard cutoff get it for free (the default exclusion),
operators who want grace or conditional behavior author a Policy, and callers who need window-aware evaluation pass
`expected_window` in request metadata.

In P1, Quota mutation requires operator-level or Quota-Manager PDP scope, regardless of `source` value. Both
`source = licensing` (the common case, materialized by the licensing layer) and `source = operator` (manual caps —
incident response, compliance carve-outs, soft-launch placeholders) follow the same mutation rules in P1; the `source`
field is recorded for forward-compatibility with the P2 Source Registry rather than for differential authorization.
Quota Manager (acting on behalf of tenant administrators or the licensing layer) **MAY** drive Quota CRUD only within
the original caller's tenant scope (PDP-enforced via the propagated SecurityContext); cross-tenant operations **MUST**
be rejected. P2 introduces additional source kinds (`tenant_admin`, `user_self`) with per-source mutation rules and
cap-inheritance constraints; see §13 Open Questions. Threshold notifications in P1 use the Quota's
`notification_thresholds` field directly per `cpt-cf-quota-enforcement-fr-notification-plugin`; P2 introduces
subscriber-customizable thresholds via an auto-created Subscription overlay (see §13) so subscribers can disable noisy
default events or add their own thresholds without modifying the Quota.

- **Rationale**: A single first-class Quota entity removes the conceptual ambiguity of a template-plus-binding split
  when templates are explicitly delegated to subject managers — operators reason about one entity bound to a subject at
  creation time instead of two interlocking records. Validity windows enable scheduled cap changes (e.g., promotional
  limits during a campaign) without ad-hoc rewrites.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-manager`

#### Metric Identity Validation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-metric-identity-validation`

The system **MUST** validate at Quota creation and update time that the referenced metric is a metric type registered
in the platform `types-registry` and has a served catalogue entry
(`cpt-cf-quota-enforcement-fr-subject-type-registry`). An unknown metric **MUST** be reported as an actionable
creation-time error (canonical `InvalidArgument`). If the registry is unreachable, Quota creation/update for a metric that the
catalogue does not hold **MUST** fail with an actionable error (canonical `ServiceUnavailable`); a metric that the catalogue holds needs no registry call. Persisted Quotas whose metric type is later removed from `types-registry` **MUST** be
flagged via operational telemetry but **MUST NOT** be auto-deactivated.

- **Rationale**: Catching typos and stale references at creation time prevents silently inert Quotas. Failing fast when
  the registry is unreachable avoids silently accepting unverifiable metric references.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-types-registry`

#### Quota Metadata

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-metadata`

The system **MUST** support an operator-authored `metadata` object on every Quota, attached at create or update time.
A Quota without metadata has the empty object. Quota Metadata holds the Quota's **conditions** over the `metadata` of
the request's metric object (Request Metadata):

- each key names a property of the metric type's `metadata` whose type is a string, an integer, or a boolean;
- each value is a value of that type, or a non-empty array of such values;
- a condition holds when the Request Metadata has the property and its value equals the condition's value or one of
  its values. A condition on a property the request lacks does not hold.

Equality and membership in a list are the only conditions QE core supports. A selection that needs more, for example
a range, a negation, or a priority among Quotas, belongs to a Policy, which reads the Request Metadata.

QE **MUST** bring the Request Metadata to one **canonical form** when it admits a request: a number with no
fractional part is an integer, however the caller wrote it (`5.0` is `5`). The conditions, the Policy, and the
request fingerprint (`cpt-cf-quota-enforcement-fr-idempotency`) all use this form.

A Quota applies to a request only when all its conditions hold (`cpt-cf-quota-enforcement-fr-multi-quota-evaluation`).
The empty object has no conditions, so such a Quota applies to every request of its metric.

When a Quota is created or its metadata changes, QE core **MUST** check the metadata against the schema of the metric
type. On a property that the metric type does not declare, a property that is not a string, an integer, or a boolean,
or a value of another type, QE **MUST** write nothing and answer canonical `InvalidArgument` with the token
`QUOTA_METADATA_REJECTED` and a list of field violations, each naming the place in the metadata and the reason.

The owner of a metric type can revise it after a Quota is written, for example widen the type of a property. QE
**MUST** check the conditions of the active Quotas against the current metric type on a schedule and report the
Quotas whose conditions no longer fit through the `quota_conditions_unfit_total` gauge
(`cpt-cf-quota-enforcement-fr-telemetry`) and a log entry that names the metric and the reason. Such a Quota stays
active; evaluation does not check stored conditions against the schema. A change of a Policy does not touch Quota
Metadata. QE **MUST NOT** index metadata for direct query.

The system **MUST** enforce a single operator-configurable size limit and reject Quota create/update at validation time
when violated:

- maximum total serialized metadata size (canonical JSON): **4 KB** per Quota (default).

Metadata is mutable as part of standard Quota updates; metadata changes **MUST NOT** invalidate the Quota's identity (
the quota_id is stable across metadata updates) but **MUST** emit a `quota-changed` notification event so sinks can
react. The full metadata object **MUST** be returned by every Quota read of QE's own API (single-Quota and bulk —
subject to PDP scoping) so callers can inspect the operator's gating intent; the resolver plugin leaves it out
(`cpt-cf-quota-enforcement-fr-quota-snapshot-read`).

Quota Metadata **MUST NOT** carry PII or other regulated data — it is classified as Platform Operational Data per §6.2,
visible to platform operators and (subject to PDP scope) Quota Manager acting on behalf of tenant administrators.
Operators are responsible for ensuring metadata content respects this classification; the system **MUST** surface
telemetry on metadata size distribution so operators can detect drift.

- **Rationale**: Attribute-based Quota selection (region-gated, tier-gated, environment-gated caps) is a
  routinely-requested pattern. Typed values (e.g., `regions: ["us-east-1", "us-west-2"]`, `enabled: true`) let
  operators express it without new structural Quota fields. The 4 KB byte-size limit is
  operator-configurable so deployments can tighten the bound when threat models demand. Conditions use the schema the
  metric type already gives the request, so a Quota needs no schema of its own, a mistake is found when the Quota is
  written, and no Policy can fail on a Quota it cannot read. The closed form of conditions keeps QE core from becoming
  an attribute-query engine; if direct metadata-indexed queries are ever needed (P2 or later), they will be added
  explicitly.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-manager`

#### Bulk Quota CRUD (P2)

- [ ] `p2` - **ID**: `cpt-cf-quota-enforcement-fr-bulk-quota-crud`

The system **MUST** provide transactional bulk Quota CRUD endpoints for Quota Manager workflows that materialize
multiple Quotas from a single logical event (license-pack provisioning, plan migration, redistribution batches, tenant
offboarding):

- `bulk_create_quotas([Q1, Q2, …])` — atomically create every listed Quota, or none. Partial failure rolls back the
  entire batch.
- `bulk_update_quotas([{id, patch}, …])` — atomically apply every patch, or none.
- `bulk_deactivate_quotas([id, id, …])` — atomically deactivate every listed Quota, or none. Each affected Quota's
  active leases are resolved-by-deactivation per `cpt-cf-quota-enforcement-fr-quota-lifecycle` deactivation rules; the
  entire batch's lease-resolution events are emitted atomically with the deactivation transaction.

Each bulk operation **MUST** carry a single envelope idempotency key; replay returns the original outcome without
re-applying. Per-item idempotency keys MAY also be supplied for individual identification. The system **MUST** enforce a
configurable maximum batch size (default: **50** items per batch) and reject oversized batches with an actionable
`BULK_TOO_LARGE` error. Failures **MUST** identify the offending item(s) by index and reason so the caller can retry
with corrections; partial-success is not a permitted outcome — the entire batch either commits or rolls back. Bulk
operations **MUST** be subject to the same PDP authorization, tenant-isolation, and trust-boundary rules as their
single-item counterparts (per `cpt-cf-quota-enforcement-fr-authorization`,
`cpt-cf-quota-enforcement-fr-tenant-isolation`, §3.4 trust boundary).

- **Rationale**: Quota Manager workflows currently must compose per-Quota CRUD calls and carry their own compensation
  logic for partial failures (license-pack create halfway through, plan migration halfway through). Pushing
  transactional atomicity into Quota Enforcement, where the storage plugin already exposes the transactional primitives
  (per `cpt-cf-quota-enforcement-contract-storage-plugin`), simplifies Quota Manager and eliminates half-applied batches
  under failure. P2 priority because P1 license-pack flows can be served by per-Quota CRUD with operator-side
  compensation; transactional atomicity is a quality-of-life and reliability improvement, not a P1 blocker. Maximum
  batch size 50 is conservative — smaller batches simplify error attribution and reduce the blast radius of a
  misconfigured caller.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-manager`, `cpt-cf-quota-enforcement-actor-platform-operator`

### 5.3 Quota Type Semantics

Each Quota carries a `quota_type` identifying its accounting model. P1 reserves three GTS instances under base
`gts.cf.core.qe.quota_type.v1~`:

- `gts.cf.core.qe.quota_type.v1~cf.core.qe.allocation.v1` — in-flight reservable capacity (no period reset).
- `gts.cf.core.qe.quota_type.v1~cf.core.qe.consumption.v1` — per-period cumulative consumption (resets at the period
  boundary).
- `gts.cf.core.qe.quota_type.v1~cf.core.qe.rate.v1` — P3 type; Quota creation is rejected in P1 with
  `NOT_YET_IMPLEMENTED` per `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection`.

> **Notation.** Throughout this document, quota types are referenced by their short instance names (`allocation`,
> `consumption`, `rate`) in examples and use-case payloads for readability. API requests, storage rows, and
> outbox events use the full GTS URI form shown above.

#### Allocation Quota Semantics

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-type-allocation`

The system **MUST** support allocation-type Quotas that bound in-flight reservable capacity. An allocation Quota
maintains a single counter representing currently-in-use capacity for that Quota. The counter increments on debit and
on lease acquire (the lease reserves capacity for its TTL); it decrements on credit, on lease release / TTL
auto-release, on return and rollback, and on lease commit by the unused `reserved − actual` portion. Allocation Quotas
**MUST NOT** carry a period; the counter persists across calendar boundaries until explicitly modified by an
operation.

- **Rationale**: Allocation quotas model concurrent-use limits (e.g., maximum running VMs per tenant, maximum open file
  descriptors per user) where the cap is a point-in-time invariant rather than a per-period budget.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Consumption Quota Semantics

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-type-consumption`

The system **MUST** support consumption-type Quotas that bound cumulative consumption within a recurring time period. A
consumption Quota maintains a per-period counter representing consumed amount within the current period. The counter
increases on debit and on lease acquire (the lease reserves capacity for its TTL, attributed to the acquisition period
per `cpt-cf-quota-enforcement-fr-lease-commit` cross-period rules); it decreases on credit, on lease release / TTL
auto-release, on return and rollback, and on lease commit by the unused `reserved − actual` portion. The counter
**MUST** reset to zero at the period boundary per `cpt-cf-quota-enforcement-fr-period-rollover`.

- **Rationale**: Consumption quotas model per-period budgets (e.g., 10 000 AI tokens per day, 1 TB egress per month)
  where the cap is the maximum amount consumable within a recurring window.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Rate Quota Type — P1 Rejection

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection`

The system **MUST** declare `rate` as a reserved quota-type identifier and **MUST** reject Quota creation requests with
`type=rate` in P1 with an actionable "not yet implemented" error. The data model and API surface **MUST** leave room for
adding rate semantics in P3 without breaking changes to existing Quotas; persisted Quotas with
`type ∈ {allocation, consumption}` **MUST NOT** require migration when the `rate` type is later activated.

- **Rationale**: Reserving the identifier and the data-model slot in P1 prevents naming collisions and
  forwards-compatibility breaks when rate-style smoothing is introduced. The reject behaviour is itself a P1 normative
  obligation and lives separately from the P3 implementation contract
  (`cpt-cf-quota-enforcement-fr-quota-type-rate-declared`).
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`

#### Rate Quota Type — P3 Implementation Contract

- [ ] `p3` - **ID**: `cpt-cf-quota-enforcement-fr-quota-type-rate-declared`

**P3 contract — token-bucket admission with burst.** P3 implementation will model rate Quotas with token-bucket
admission semantics carrying:

- `rate` — the sustained admission rate (e.g., 100 ops / minute);
- `burst_capacity` — the bucket size; up to this many operations are admitted without backoff if the bucket is full at
  request time;
- `smoothing_window` — the period over which the bucket refills at `rate` (typically equal to or smaller than the rate's
  denominator).

Admission against an exhausted rate Quota **MUST** return `Denied(reason = "RATE_WINDOW_EXHAUSTED")` with a
`Retry-After` floor (advisory; clients SHOULD add randomized jitter). Cap-clamp does not apply to rate Quotas — the only
valid admission outcomes for `quota_type = rate` remain `Allowed` / `Denied` (with operational failures surfaced as
canonical errors). Burst implementation choices ( sliding-window vs token-bucket vs fixed-window; per-tenant vs
per-region smoothing axis) are tracked in §13 Open Questions and resolved when P3 implementation begins. The P1 reject
contract from `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection` is superseded once this FR is implemented; until
then, `type=rate` creation requests continue to be rejected per the P1 FR.

- **Rationale**: Enumerating the future contract shape now prevents P3 implementation from re-deriving cap and bucket
  semantics from scratch.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`

### 5.4 Time Period & Reset Semantics

#### Period Specification & Calendar Alignment

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-period-semantics`

The system **MUST** support consumption-quota periods drawn from a fixed set of GTS instances under base
`gts.cf.core.qe.period_type.v1~`. P1 reserves five instances:
`gts.cf.core.qe.period_type.v1~cf.core.qe.day.v1`,
`gts.cf.core.qe.period_type.v1~cf.core.qe.week.v1`,
`gts.cf.core.qe.period_type.v1~cf.core.qe.month.v1`,
`gts.cf.core.qe.period_type.v1~cf.core.qe.year.v1`, and
`gts.cf.core.qe.period_type.v1~cf.core.qe.one_time.v1`. All periods **MUST** be UTC and calendar-aligned by
default: a `day` period begins at 00:00 UTC and ends at 24:00 UTC; a `month` period begins at 00:00 UTC on the first
day of the calendar month; a `week` period begins at 00:00 UTC on Monday. The `one-time` period denotes a non-recurring
quota (typically used for promotional or single-event budgets) — it has no automatic reset and **MUST** be deactivated
explicitly when exhausted or expired.

> **Notation.** Throughout this document, periods are referenced by their short instance names (`day`, `week`, `month`,
> `year`, `one-time`) in examples and use-case payloads for readability. API requests, storage rows, and
> outbox events use the full GTS URI form shown above (with `one_time` underscore form for the `one-time` instance).

The system **MUST** persist the current period boundary timestamp with each consumption Quota's counter to support
deterministic period-rollover detection.

- **Rationale**: Calendar alignment matches operator and user mental models (billing-cycle alignment is a separate
  concern handled by the billing layer); UTC eliminates timezone-related drift across multi-region deployments.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Period Rollover Semantics

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-period-rollover`

At every period boundary the system **MUST** reset the consumed counter to zero; any unused capacity is forfeited.
Period-rollover transitions **MUST** be atomic with respect to in-flight operations and **MUST** follow these explicit
ordering rules to make period attribution and event semantics deterministic:

- **Boundary attribution (direct debit)**: a single-shot `debit` whose mutation commits at exactly `boundary_at` is
  accounted to period `P+1` (the boundary is inclusive on the new side). Operations whose mutation transaction commits
  at timestamp `t` are accounted to the period whose half-open interval `[start, end)` contains `t`.
- **Boundary attribution (lease)**: a lease is attributed to the period containing its **acquisition** timestamp;
  commit/release/auto-release that fire after a period boundary apply against the acquisition period's counter, not the
  new period's. See `cpt-cf-quota-enforcement-fr-lease-commit` "Cross-period-boundary commit" for the normative rule.
- **`period-rollover` event ordering**: the `period-rollover` event for period `P` is emitted strictly after the last
  commit attributed to `P`.

Period rollovers **MUST** emit a `period-rollover` notification event including the closing-period consumed amount, the
closing-period cap, and the new period boundary. Downstream consumers (Quota Manager) **MAY** derive unused-capacity
figures from this payload for subscription-tier accounting external to QE.

**Event timing — calendar boundary vs event emission.** Because leases attributed to period `P` MAY commit up to
`max_lease_ttl` (default: 1 hour per `cpt-cf-quota-enforcement-fr-lease-acquire`) past `period_end`, the
`period-rollover` event for `P` MAY be delayed by up to `max_lease_ttl` after the calendar boundary. The event therefore
signals **settlement completion** (closing-period `consumed` is final, no further P-attributed mutations will arrive) —
not the calendar transition itself. The calendar boundary is a deterministic function of `time` and `period_spec`;
consumers needing a calendar-time signal compute it directly without observing any event.

Credit and return/rollback (`cpt-cf-quota-enforcement-fr-credit`, `cpt-cf-quota-enforcement-fr-return`,
`cpt-cf-quota-enforcement-fr-rollback`) are intentionally asymmetric on this axis: credit closes at the calendar
boundary (new intent is rejected immediately at `time >= period_end`), while a return or rollback decreases the
period's counter, a return only inside the return window, until `period-rollover` event emission (so cross-period lease
commits remain reversible during the
settlement window).

- **Rationale**: "Use it or lose it" matches the licensing service P1 PRD — neither layer models period-end carry-over
  inside the enforcement layer. Subscription-tier semantics that need to roll unused capacity forward (telco-style
  "rollover minutes", LLM-token credit grants) belong in the licensing layer, which subscribes to `period-rollover` and
  pushes new caps via `update_quota`. Keeping QE rollover atomic and policy-free preserves a small, predictable
  enforcement surface; future re-introduction is tracked in §13 Open Questions.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`

### 5.5 Quota Operations: Debit, Credit, Rollback, Return, Preview

#### Debit Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-debit`

The system **MUST** provide an idempotent debit operation. Debit invokes multi-quota evaluation (
`cpt-cf-quota-enforcement-fr-multi-quota-evaluation`) under the active Quota Resolution Engine (
`cpt-cf-quota-enforcement-fr-quota-resolution-engine`); the Engine returns a Decision whose `debit_plan` names exactly
which Quotas to mutate and by how much. The system **MUST** then atomically apply that plan: for every Quota in
`debit_plan`, decrease that Quota's remaining capacity by `entry.amount` (allocation Quotas: increment the in-flight
counter; consumption Quotas: increase the current-period consumed amount). Quotas not named in `debit_plan` **MUST NOT**
be mutated. An allowed debit writes a charge to the charge ledger (`cpt-cf-quota-enforcement-fr-return`) in the same
transaction. Debit **MUST** require a client-supplied idempotency key; replay of the same key with the same
payload (subjects, metric, amount) **MUST** return the original Decision without modifying any counter a second
time.

The request **MUST** carry a positive integer `amount > 0`. Requests with `amount ≤ 0` (zero or negative) **MUST** be
rejected with an actionable `INVALID_AMOUNT` error **before** idempotency lookup, subject resolution, or any other
pipeline step — no idempotency record is persisted, no operation log entry is written, no counter is mutated.
Zero-amount debits are rejected because they would consume idempotency / operation-log storage without changing any
counter and would muddy threshold-crossed and replay semantics; callers needing a "would this be allowed?" affordance
without committing capacity **MUST** use `cpt-cf-quota-enforcement-fr-evaluate-preview`.

The Engine returns a `Decision` (shape per §3.4 Decision Contract; binding-Quota selection rules and per-entry
Debit-Plan invariants in §5.9 canonical FR). The system **MUST** apply the returned `debit_plan` atomically — the
caller never observes a state where `result = Allowed` but only some entries were applied, nor where `Denied` or any
canonical error was returned but any counter changed.

- **Rationale**: Atomic decision-and-mutation eliminates the race window where duplicate concurrent calls could each see
  the same remaining capacity and both succeed against a hard cap. Idempotency keys make at-least-once retry semantics
  safe. Driving mutations from the Engine's `debit_plan` (rather than from a hardcoded "debit every applicable Quota
  with the full request amount") lets the default `most-restrictive-wins` Engine implement subject-tier cascade
  single-entry selection while leaving cross-tier splits, weighted distributions, attribute-routing, and
  AND-across-Quotas patterns expressible in the Policy layer through `cel` — all without changes to the core
  evaluation engine.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Credit Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-credit`

The system **MUST** provide an idempotent S2S credit operation that increases the remaining capacity of **a single
caller-named Quota** by a positive integer amount. The request **MUST** carry an explicit `quota_id` identifying the
target Quota; credit does **not** invoke subject resolution or the Quota Resolution Engine — it mutates the named
Quota's counter directly. The request's explicit target tenant **MUST** be PDP-authorized for the authenticated service
principal and match the named Quota; cross-tenant credit attempts are rejected per `cpt-cf-quota-enforcement-fr-tenant-isolation`.
Credit is a management-plane operation: it **MUST** be authorized as its own action, so a service allowed to debit is
not thereby allowed to credit; deployments **MUST** grant it only to Quota Manager and platform-operator principals. A
request whose `quota_id` is unknown, refers to a deactivated Quota, or refers to a Quota outside the caller's tenant
**MUST** be rejected with an actionable error before any mutation.

The request **MUST** carry a positive integer `amount > 0`. Requests with `amount ≤ 0` (zero or negative) **MUST** be
rejected with an actionable `INVALID_AMOUNT` error **before** idempotency lookup, the row-locked Quota read, or any
other pipeline step — no idempotency record is persisted, no operation log entry is written, no counter is mutated.

For allocation Quotas, credit decrements the in-flight counter (with a floor of zero). For consumption Quotas, credit
decreases the current-period consumed amount (with a floor of zero). Credit **MUST** require a client-supplied
idempotency key; replay of the same key **MUST** return the original outcome without modifying any counter a second
time.

Credit operates against the **currently-active** period of a consumption Quota. Attempts to credit a period whose
calendar window has elapsed (`time >= period_end` at the moment the credit transaction is evaluated) **MUST** be
rejected with an actionable `PERIOD_CLOSED` error in P1. Closure is keyed on **calendar time**, not on
`period-rollover` event emission per `cpt-cf-quota-enforcement-fr-period-rollover` "Event timing" — credit is rejected
immediately at the calendar boundary, even while lease flow may still be settling cross-period commits into the
closing period's counter. Backdated credits to a closed period are deferred (target resolution: production feedback)
and would require operator-elevated authorization. Consumer-side refund-on-cancel for a specific prior debit is served
by the **rollback** and **return** primitives (`cpt-cf-quota-enforcement-fr-rollback`,
`cpt-cf-quota-enforcement-fr-return`), not by credit.

Every successful credit **MUST** emit a `quota-counter-adjusted` notification event carrying the credited amount, the
target `quota_id`, and the authenticated service principal, so tenant-side dashboards remain consistent with
mid-period counter adjustments that are not the result of a natural debit/rollback flow.

- **Rationale**: Credit is the explicit redistribution / compensatory primitive that authorized services, including
  Quota Manager, compose into higher-level workflows (redistribution, SLA-breach grants, manual adjustments). These workflows
  always target a specific Quota — "give user U 500 more tokens", "restore the EU-region cap by 100 hours". Rollback is
  the consumer-side undo for a specific prior debit and is identified by the original idempotency key;\
  credit and rollback are intentionally separated by *identity of the operation* (free-form adjustment vs. undo of a
  specific debit). PDP authorization of the explicit target prevents unauthorized adjustments. Rejecting credits to
  closed periods prevents retroactive accounting that would invalidate already-emitted
  `period-rollover` events. The `quota-counter-adjusted` event keeps observability on tenant-facing surfaces consistent
  when counters move outside the debit/rollback flow.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-manager`

#### Rollback Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-rollback`

The system **MUST** provide a rollback operation that returns everything that remains of a charge. A rollback names
the charge by the tenant and the charge's operation and idempotency key. It carries no amount and no idempotency
key of its own. It is a return of the remainder over the charge ledger (`cpt-cf-quota-enforcement-fr-return`,
ADR-0011), applied to each entry as a return applies it, and it fails as a return fails. After a partial return,
rollback gives back only the remainder; one charge is never given back twice. On a charge with nothing left, rollback
**MUST** succeed and change nothing, so a repeated rollback is safe. A rollback whose charge is unknown, a deleted
charge included, **MUST** be rejected with an actionable `UNKNOWN_OPERATION` error.

On QE's own API a rollback **MUST** also carry the attribution of the charged operation (the subjects and the metric
with its type and optional id) and **MUST** be authorized for it, for the action `rollback`, as a debit is authorized
for its attribution (`cpt-cf-quota-enforcement-fr-authorization`). A caller **MUST NOT** be able to roll back a charge
for whose attribution it has no `rollback` grant.

**Rollback against lease-derived charges.** Rollback **MAY** target any charge, including one that originated from a
lease `commit` (per `cpt-cf-quota-enforcement-fr-lease-commit`) — the commit writes its charge under the **commit
call's** idempotency key, and rollback names the operation `commit` and that key. (Rollback of a lease **before**
commit is not the rollback primitive's job — use `release` per `cpt-cf-quota-enforcement-fr-lease-release`.)

**Rollback and settled periods (consumption Quotas).** A rollback decreases a consumption counter only inside the
charge's return window and while the charge's period is not settled, that is, until the `period-rollover` event is
emitted for it (`cpt-cf-quota-enforcement-fr-period-rollover`). This is keyed on settlement, not on the calendar
boundary — intentionally asymmetric with `cpt-cf-quota-enforcement-fr-credit`'s calendar-keyed closure: cross-period
lease commits land against the closing period's counter between `period_end` and event emission, and stay reversible
there. Once `period-rollover` fires, the closing-period `consumed` is final. Callers needing guaranteed cancellation
across a period boundary **MUST** model the work via the lease flow (`cpt-cf-quota-enforcement-fr-lease-acquire` /
`commit` / `release`) so that period attribution stays bound to acquisition time per
`cpt-cf-quota-enforcement-fr-lease-commit` "Cross-period-boundary commit".

Rollback semantics are equivalent to refund-on-cancel for the canceling caller's purposes — a job that aborted before
consuming its debited capacity can rollback to release it. Rollback against a credit operation is **NOT** supported:
a credit writes no charge (use a counter-credit if needed).

Every rollback that gives back an amount **MUST** emit one `quota-rollback-applied` notification event
(`cpt-cf-quota-enforcement-fr-notification-plugin`), so subscribers observe it distinctly from credits (which use
`quota-counter-adjusted`) and from natural debit completion (which is silent on the notification surface). A rollback
that fails or changes nothing emits no event.

- **Rationale**: Rollback by the charge's name gives callers a deterministic compensation primitive without requiring
  them to remember the debit amount or the inverse — the charge's name and the attribution of the original request are
  sufficient.
  It is intentionally separate from credit because rollback's identity is "undo this specific debit", whereas credit's
  identity is "issue this specific refund".
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Return Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-return`

The system **MUST** keep a **charge ledger**: every allowed debit and every lease commit writes a charge in the same
transaction as its counters, with the tenant, the operation, the idempotency key, the request fingerprint, the attribution digest, the metric,
the amount, and per Debit Plan entry its chain, position, Quota, period, charged amount, and returned amount. The
**attribution digest** is a digest of the attribution that admission accepted for the charged operation: the tenant,
the subjects, and the metric with its type and optional id. A charge is unique per `(tenant, operation, key)` and
keeps no subject identifier.

The system **MUST** provide an idempotent return operation. It is reachable only through the resolver plugin
(`cpt-cf-quota-enforcement-fr-resolver-plugin`); QE's own API offers rollback, which returns everything that remains
(`cpt-cf-quota-enforcement-fr-rollback`). A return names the tenant, the charge's operation and key,
the id of the metric type, a positive amount, and its own idempotency key. A return that names a metric type other
than the charge's **MUST** be rejected with `METRIC_TYPE_MISMATCH` (canonical `InvalidArgument`). It unwinds the amount over the charge's entries
(`cpt-cf-quota-enforcement-fr-quota-resolution-engine`); each entry's returned amount grows, and the entry's Quota
takes the amount by its type:

- allocation: the in-flight counter decreases;
- consumption: the counter of the charge's period decreases, inside the return window and while that period is not
  settled (its `period-rollover` event not yet emitted); otherwise the counter does not change;
- rate: the counter does not change.

A deactivated Quota takes a return as an active one does: deactivation stops new debits and leases, not returns. A
counter **MUST NOT** go below zero: an entry decreases it by the entry's amount, or to zero, as a credit does; the
entry's returned amount grows by the whole amount, so repeated returns never exceed the charge.

A return is all or nothing: it **MUST** fail with an actionable error and change nothing when the charge is unknown
(`UNKNOWN_OPERATION`), the amount exceeds what remains of the charge, or no entry can take a part of the amount: every
entry that the amount reaches is on a rate Quota, or on a consumption Quota outside the return window or in a settled
period. Whether an entry can take an amount depends on the type of its Quota and on the window, never on the current
value of its counter: an allocation entry always takes a return, also when its counter is already at zero. So a return
on a charge with only rate Quotas or only a settled consumption period fails, and a return that reaches an allocation
entry succeeds.

The key of a return is unique per `(tenant, return, key)`, like the key of any operation; the charge it names and the
amount are its fingerprint. QE **MUST** record the key of every applied return once per tenant, with its charge and
amount, for as long as the charge lives; a return has no other idempotency record. A repeated return with the same
charge and amount changes nothing and gets the first result; the same key with another charge or amount fails with
`IDEMPOTENCY_PAYLOAD_MISMATCH`, also when the two returns run at the same time.

**Return window and deletion.** The return window of a charge is the replay window
(`cpt-cf-quota-enforcement-fr-idempotency`), counted from the charge's creation. A window has the length that is
configured when its record is written: a charge takes its return window at its creation, and the time it is kept
after a return or rollback when that one is applied. A charge is deleted when the replay
window has passed since its creation and since the last return or rollback applied to it, and no allocation entry of
it holds an amount on an active Quota (an entry holds an amount while its returned amount is less than its charged
amount); a repeated return or rollback therefore finds its charge for one replay window. From that moment
the charge is unknown to every operation, whether or not the sweeper has removed it, and a debit or commit under its
key is a new operation that replaces it; a background sweeper removes deleted charges in batches. A
consumption charge cannot be returned after its return window; work whose result comes later holds a lease.

An entry on a deactivated Quota does not keep its charge: a deactivated Quota admits nothing, so its held amount
limits nothing. An allocation charge on an active Quota has no time limit: the system **MUST NOT** free a held amount
because time has passed.

**Lost returns.** The system does not compare held amounts with the resources that exist. The module that made a
charge returns it. When a module does not return the charge of a resource that is gone, the amount stays held until
that module sends the return: a return is idempotent by its key, and an allocation charge stays returnable for as long
as it holds an amount, so the module can send the return at any later time. The system offers no operation by which an
operator returns the charge of another caller. Comparing held amounts with the resources that exist is the task of a
component above QE that reads the Usage Collector's records and the quota state (§3.3); QE does not do it. A credit
(`cpt-cf-quota-enforcement-fr-credit`) is not the correction of a lost return: it changes no charge, so the return,
when it arrives, would lower the counter a second time. Until the module returns the charge, the operator can raise
the cap of the Quota. A rollback (`cpt-cf-quota-enforcement-fr-rollback`) is recorded in its charge, so a return of
that charge after it **MUST** fail and change no counter. The system **MUST** report, per
metric, the number of allocation charges that hold an amount on an active Quota and are older than the replay window,
and the age of the oldest one (`cpt-cf-quota-enforcement-fr-telemetry`).

Every applied return **MUST** emit one `quota-rollback-applied` notification event
(`cpt-cf-quota-enforcement-fr-notification-plugin`).

- **Rationale**: The Quota Resolver requires a return of allocation capacity that a resource no longer holds and of a
  charge for work that failed, named by the charge only; an allocation charge outlives the replay window
  (ADR-0011).
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-resolver`

#### Evaluate Preview (read-only dry-run)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-evaluate-preview`

The system **MUST** provide a read-only **preview** operation that executes the full evaluation pipeline (subject
resolution → applicable-Quotas → active Quota Resolution Policy → Engine → Decision) for the same input shape as a
debit — attribution, metric object, and amount — and returns the resulting Decision **without applying any counter
mutation, without holding any capacity, and without persisting any idempotency record or operation-log entry**. Preview
is intended for:

- Quota Manager UIs that surface "if you debit X right now, here is what would happen" affordances to tenant
  administrators and end users.
- Operator authoring and testing of `cel` (and future) Engine Policies — exercising candidate Policy logic against real
  Quota state before deploying.
- Calling-service capacity planning and debugging ("would my next batch fit?", "why was the last call denied?").

Preview **MUST**:

- Require the same PDP authorization (`cpt-cf-quota-enforcement-fr-authorization`) as the corresponding write operation;
  reads outside the caller's authorized metric / subject scope are rejected.
- Apply the same trust-boundary rules (§3.4) — Decision-shaped fields are silently ignored if present in the request
  body.
- Use the **current** counter state at evaluation time (a transactional snapshot read; no read-modify hold and no
  contention against concurrent debits).
- Return the full Decision shape (`result`, `debit_plan`, `diagnostics`) the system would produce for an equivalent
  debit, plus an explicit `preview: true` marker so callers cannot conflate it with a real Decision. Diagnostics include
  `policy_id` and `policy_version` (per `cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`) so operators
  inspecting "what would happen if" see exactly which Policy version they are testing against.
- **NOT** require an idempotency key — preview is read-only and idempotent by construction.
- **NOT** persist any record — no operation log entry, no idempotency record, no lease row, no Engine-evaluation record
  beyond ephemeral telemetry counters.

Preview **MUST NOT** be used as a substitute for `reserve` when capacity needs to be held — preview's verdict can be
invalidated by concurrent debits between the preview call and a follow-up real debit. Callers needing held capacity
continue to use `reserve` (`cpt-cf-quota-enforcement-fr-lease-acquire`).

- **Rationale**: A read-only preview is a more-ergonomic alternative to `reserve(ttl=1s) + release(token)` for the
  common "what would happen if I debit X?" question — no idempotency-key generation, no capacity hold, no two-phase
  commit, no row mutation, no sweeper churn, no contention with concurrent writers. The `reserve+release` pattern
  remains the right tool when the caller actually wants to hold capacity briefly; preview is for the cases where the
  caller never intended to consume yet (UI affordances, Policy authoring, capacity planning). PDP scoping
  (`cpt-cf-quota-enforcement-fr-authorization`) and tenant-isolation are the only access controls; preview shares the
  standard hot-path NFR with debit and is bounded by the same baseline throughput protections that apply to every read
  operation.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`, `cpt-cf-quota-enforcement-actor-quota-manager`,
  `cpt-cf-quota-enforcement-actor-quota-reader`

### 5.6 Lease & Two-Phase Operations

#### Lease Acquisition

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-lease-acquire`

The system **MUST** provide a lease operation that holds capacity on the Quotas of the Engine's Debit Plan for a
bounded TTL without finalizing consumption. A successful lease **MUST** return an opaque lease token and its expiry
and **MUST** decrease each planned Quota's remaining capacity by its entry's amount for the TTL duration. Lease
requires an idempotency key; replay returns the original outcome (the token, or the original `Denied` Decision). Lease
**MUST** be subject to multi-quota evaluation identically to debit
(`cpt-cf-quota-enforcement-fr-multi-quota-evaluation`); a lease that the Engine denies **MUST NOT** hold capacity in
any Quota.

The request **MUST** carry a positive integer `amount > 0`. Requests with `amount ≤ 0` (zero or negative) **MUST** be
rejected with an actionable `INVALID_AMOUNT` error **before** idempotency lookup, the multi-quota evaluation, or any
hold acquisition — no idempotency record is persisted, no lease row is created, no Quota's capacity is reserved, the
per-`(tenant, metric)` active-lease counter is unchanged.

The lease TTL **MUST** be supplied by the caller and **MUST** fall within an operator-configurable window
`[min_lease_ttl, max_lease_ttl]` (platform defaults: 1 s and 1 h respectively). A TTL outside the window — or a missing
`ttl` field — **MUST** be rejected with an actionable `TTL_OUT_OF_BOUNDS` error **before** idempotency lookup,
multi-quota evaluation, or any hold acquisition — no idempotency record, no lease row, no capacity hold, no change to
the per-`(tenant, metric)` active-lease counter (mirroring the `INVALID_AMOUNT` fail-fast above). Clamping is **NOT**
performed: silent TTL adjustment would mask client miscalculations and is incompatible with the lease contract, which
entitles the holder to the exact TTL it reserved.

The system **MUST** persist the lease token, the held amount, the affected Quotas with each entry's chain and
position, the tenant, and the expiry timestamp.

**Atomic multi-Quota acquisition.** When the Engine's Debit Plan names multiple Quotas (e.g., a user Quota plus a
tenant Quota that both apply to one operation, or several Quotas selected by a cascade or attribute-based
Policy), the system **MUST** acquire holds on every Quota in the plan **atomically**: either every named Quota's
capacity is held or none is. Partial holds (some Quotas reserved, others not) are not a permitted intermediate state,
including under failure or concurrent contention. Concurrent multi-Quota lease traffic **MUST NOT** deadlock the
acquisition path. The choice of acquisition ordering, locking discipline, and storage-plugin transactional primitives
that achieve these properties is a DESIGN-time concern and is described in DESIGN §3.3 (storage-plugin invariants
I7/I8/I9) and §3.6 sequence diagrams.

**Acquisition contention timeout.** Concurrent leases and debits against the same Quota row contend for the underlying
counter mutation. The system **MUST** bound how long an acquisition request waits on contention via an
operator-configurable per-metric **acquisition contention timeout** (default: 50 ms; 0 ms means no wait). When the
timeout is exceeded, the acquisition **MUST** be rejected with an actionable `LEASE_CONTENTION_TIMEOUT` error and
**MUST NOT** hold any Quota (consistent with the atomic-acquisition rule above). The scope is per-metric (not per-tenant) because
row-level contention is a platform-tuning concern driven by metric hotness — different metrics ( `ai-tokens-input` vs
`vCPU-hours`) have different contention profiles, but a single metric's contention curve does not vary meaningfully by
tenant. This timeout is distinct from:

- the **lease TTL** (1 s – 1 h, controls how long a lease is held *after* successful acquisition),
- the **per-Policy Engine evaluation timeout** (default 5 ms, the Engine compute budget per
  `cpt-cf-quota-enforcement-fr-quota-resolution-policy`),
- the **per-`(tenant, metric)` active-lease cap** (default 1000, bounds concurrent live leases per
  `cpt-cf-quota-enforcement-fr-lease-timeout`).

Operators MAY tune all four independently. The same timeout applies to every counter-mutating operation of a request:
debit, batch debit, credit, lease commit, lease release, return, and rollback. Each of them waits on contention at
most the timeout of its metric, then fails with `LEASE_CONTENTION_TIMEOUT` and changes nothing. A repeat with the same
idempotency key can succeed. Background sweepers have no contention timeout. The normative timeout and telemetry are
stated here once.

Operations on one Quota run one after another. A module with a high request rate on one Quota charges in fewer, larger
operations: one lease for many units of work, or a batch debit.

**Contention telemetry.** The system **MUST** expose:

- a counter `contention_rejected_total` incremented on every `LEASE_CONTENTION_TIMEOUT` rejection of any operation,
  labelled by the closed `operation` kind (a batch counts as a debit),
- a counter `lease_contention_rejected_total` incremented on every `LEASE_CONTENTION_TIMEOUT` rejection of a debit or
  a lease acquisition, the operations whose metric the catalogue has served, labelled by canonical registered
  `metric`,
- a histogram `lease_acquisition_wait_seconds` covering the wait time before successful acquisition or rejection,
  labelled by canonical registered `metric`.

These counters belong to the §5.16 telemetry surface. They let operators detect hot-key contention (single-row write
hotspot per the §12 risk) before it manifests as user-visible latency, and they let Quota Manager surface
contention-driven denials distinctly from `LEASE_INFLIGHT_LIMIT_EXCEEDED`, `LEASE_NOT_ACTIVE`, and Engine-`Denied`
rejections in operator UIs.

- **Rationale**: Two-phase lease models long-running operations whose consumption isn't known until completion ( e.g.,
  "I'm starting a 30-minute job that may abort"). Without lease, callers must invent ad-hoc pending-debit tables or risk
  over-committing capacity. Atomic multi-Quota acquisition is the only safe semantic when one operation needs holds
  across several Quotas — partial holds would produce ambiguous state for callers and dirty rows for the sweeper. The
  acquisition contention timeout (default 50 ms) bounds the time a request waits behind row locks, so waiting requests
  do not pile up without a limit, and two requests at the same moment on one Quota do not fail one another; operators
  tune the timeout based on workload preferences (low latency vs higher acquisition success rate).
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Lease Commit

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-lease-commit`

The system **MUST** provide a commit operation that converts an active lease into a debit. Commit **MUST** be
idempotent (replay returns the original outcome). Commit **MAY** specify an `actual_amount` less than or equal to the
originally reserved amount, and **MAY** name with it the id of the metric type, as a commit of the Quota Resolver
does; a commit that names a metric type other than the lease's **MUST** be rejected with `METRIC_TYPE_MISMATCH` (canonical
`InvalidArgument`) and change nothing; the unused part (reserved minus actual) is unwound from the lease's holds
(`cpt-cf-quota-enforcement-fr-quota-resolution-engine`, ADR-0010), and the commit writes a charge for the actual
amount to the charge ledger (`cpt-cf-quota-enforcement-fr-return`). `actual_amount > reserved_amount` **MUST** be
rejected with an actionable `OVER_COMMIT_NOT_AUTHORIZED` error — callers needing to commit beyond their lease must
reserve a higher worst-case estimate up front. Committing an expired or already-resolved (committed/released) lease
**MUST** be rejected with an actionable `LEASE_NOT_ACTIVE` error, except a commit that repeats the key of the commit
that resolved the lease: it gets that commit's stored result, also when the two run at the same time.

**Cross-validity-boundary commit.** A commit MAY arrive after the underlying Quota's `validity_window` has expired (the
lease was acquired while the window was still valid; commit happens after `validity_end`). Such commits **MUST** succeed
and the corresponding counter mutation **MUST** be applied: the lease guarantee was earned at acquisition time and the
holder is entitled to its capacity. Operators wanting a strict cutoff at `validity_end` should constrain TTL bounds (per
`cpt-cf-quota-enforcement-fr-lease-acquire`) so that no lease can outlive the validity window. Commits whose underlying
lease TTL has expired remain rejected per the rule above (lazy expiry of TTL is independent of validity-window expiry).

**Cross-period-boundary commit (consumption Quotas).** A lease is attributed to the consumption-Quota period whose
half-open interval `[period_start, period_end)` contains its **acquisition timestamp**. The held amount counts against
that period's `consumed` from the moment of acquisition until the lease reaches a terminal state (`committed` /
`released` / auto-released); it does **NOT** count against any subsequent period's capacity, even if the lease's TTL
crosses one or more period boundaries. A `commit` that fires after a period rollover **MUST** apply its counter mutation
against the **acquisition period's** counter; the new period's `consumed` is not incremented by such a commit.
Symmetrically, a `release` (or TTL auto-release) after a period boundary returns held capacity against the acquisition
period's counter only — the new period's `remaining` is unaffected. This mirrors the cross-validity-boundary rule above:
the lease guarantee, including its period attribution, is earned at acquisition time. Operators wanting strict
period-boundary cutoffs should constrain lease TTL so that leases cannot outlive the acquisition period.

- **Rationale**: Allowing the actual committed amount to be less than reserved is the common case (the job consumed less
  than budgeted); rejecting `actual > reserved` unconditionally prevents silent over-commit and keeps the lease contract
  tight — the held capacity is the upper bound the caller is entitled to consume.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Lease Release

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-lease-release`

The system **MUST** provide a release operation that returns a lease's held amount to every affected Quota's remaining
capacity without committing a debit. Release **MUST** be idempotent (replay is a no-op after the first invocation,
also when the two calls run at the same time). Releasing an already-resolved lease under another key **MUST** be
rejected with an actionable `LEASE_NOT_ACTIVE` error.

- **Rationale**: Release is the inverse of lease; together with TTL-based auto-release it ensures held capacity is
  always eventually returned to the affected Quotas even if the calling service crashes or is partitioned.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Lease TTL Auto-Release

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-lease-timeout`

The system **MUST** automatically release every lease whose TTL has expired without a commit or release. Auto-release
**MUST** be deterministic with respect to the lease expiry timestamp. The requirement is fulfilled by **two distinct
tiers**, deliberately separated so that correctness never depends on a background process being live:

1. **Semantic release (lazy, correctness-critical).** Once a lease's expiry timestamp has passed, the held amount **MUST
   NOT** be counted against any affected Quota's in-flight capacity. Every reader and every writer **MUST** treat the
   lease as released, regardless of whether its underlying storage row still exists or has been reclaimed. Semantic
   release is the only tier on which correctness depends and **MUST** remain correct under sweeper outage, partition,
   restart, or any other lifecycle event of the reclamation tier below. New leases and debits **MUST NOT** be blocked by
   zombie rows of already-expired leases.

1. **Physical reclamation (eventual, storage hygiene).** A background sweeper **MUST** be deployed to perform physical
   cleanup of expired lease rows — deletion, or operator-defined archival to a long-term store. Reclamation **MUST**
   complete within 1 hour of expiry. The sweeper is also the canonical
   emission point for the `lease-auto-released` notification event: exactly one event per lease, carrying the lease ID,
   tenant, held amount, affected Quotas, and expiry timestamp; the subject of each affected Quota is the Quota's own.

Sweeper liveness **MUST NOT** gate correctness. If the sweeper is delayed, paused, or crashed, the semantic-release tier
continues to behave correctly — expired leases remain released for accounting purposes, and new operations are not
blocked. Unreclaimed rows accumulate until the sweeper resumes, and the corresponding `lease-auto-released` events are
deferred until reclamation; the system **MUST** surface unreclaimed-expired-lease count via telemetry so operators can
detect sweeper outages.

**Per-`(tenant, metric)` active-lease cap.** To bound the unreclaimed-row ceiling under sweeper failure or under abusive
usage patterns (callers that open leases and never settle them), the system **MUST** enforce an operator-configurable
per-`(tenant, metric)` cap on concurrent **active** leases (default: 1000). Lease acquisition (
`cpt-cf-quota-enforcement-fr-lease-acquire`) **MUST** reject with an actionable `LEASE_INFLIGHT_LIMIT_EXCEEDED` error
any request that would push the active-lease count above the cap, regardless of underlying Quota capacity. Expired
leases do **not** count toward the cap (they are released by the lazy semantic above) — the cap exists to limit live
in-flight leases and to bound row growth between sweeper runs.

- **Rationale**: Lease safety depends on the absence of zombie holds. Lazy expiry interpretation makes the semantics
  safe under partitions or sweeper outages — an expired lease never blocks new operations even if the ledger row hasn't
  been physically reclaimed yet. Bounding reclamation lag and capping concurrent active leases together prevent
  unbounded storage growth from misbehaving callers and from sweeper outages, without coupling correctness to sweeper
  liveness.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

### 5.7 Bulk Operations

#### Batch Debit

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-batch-debit`

The system **MUST** provide a batch debit operation that submits multiple debit items targeting potentially different
metrics in a single call. The batch **MUST** carry an envelope idempotency key; replay of the same envelope key returns
the original outcome without modifying counters a second time, as long as the batch is within the current maximum
batch size. Each item is a debit: it **MUST** carry its own idempotency key,
writes a `debit` charge under that key when the batch is allowed, and shares the key space of single debits. A batch
is replayed only as a whole, by its envelope key: a batch with an item key that already names a record of a single
debit, which is a `debit` charge or the idempotency record of a denied debit, **MUST** be rejected as a whole with
`IDEMPOTENCY_PAYLOAD_MISMATCH` naming the item, and change nothing. The system **MUST** check the item keys again in
the transaction that writes the charges: a single debit that commits first under an item key rejects the batch, and a
batch **MUST NOT** return the stored result of a single debit.

Every batch item **MUST** carry a positive integer `amount > 0`. The batch **MUST** be rejected with an actionable
`INVALID_AMOUNT` error (envelope-level, naming the offending item index) **before** any item is evaluated, regardless of
`mode`, if **any** item carries `amount ≤ 0`. No envelope idempotency record is persisted, no per-item idempotency
records are persisted, no counter is mutated.

The system **MUST** support two batch modes selected by the caller via a required `mode` field on the request:

1. **`atomic` (P1).** All-or-nothing semantics for **multi-metric admission of a single logical operation**. Every item
   is evaluated against its own applicable-Quotas set under the active Quota Resolution Engine; each item's evaluation
   **MUST** observe counter state reflecting the application of every previously-evaluated item in the same batch,
   otherwise the Engine cannot produce a correct Debit Plan. If **every** item evaluates to `Allowed`, the system
   **MUST** apply the union of all per-item `debit_plan`s; if **any** item evaluates to `Denied` or fails with a
   canonical error, the system **MUST** apply no counter mutations and report the batch outcome as `Denied` (in the
   body) or surface the canonical error accordingly, with per-item statuses included for diagnostic purposes only.
   Atomic mode is the natural primitive for "this single operation needs metric A AND metric B AND metric C — admit or
   deny as a whole" (e.g., an LLM call that consumes input tokens, output tokens, and compute seconds simultaneously).

1. **`independent` (P2).** Partial-success semantics for **bulk independent operations** submitted in one RPC purely as
   a transport optimization (e.g., recording consumption for N unrelated background jobs). Each item is evaluated and
   applied independently; per-item outcomes do not affect each other. **P1 implementations MUST reject
   `mode = independent` requests with an actionable `NOT_YET_IMPLEMENTED` error** until the mode ships.

The response **MUST** include the batch-level outcome and a per-item array preserving submission order. Callers **MUST**
treat `Denied` and a canonical error distinctly: `Denied` is a deterministic over-cap signal in the Decision body (retry
is futile until a credit or period rollover), whereas a canonical error is a non-deterministic technical failure (retry
under the same envelope key is replay-safe).

The system **MUST** enforce a configurable maximum batch size (default: 100 items per batch) and reject oversized
batches with an actionable error.

**Batch-level evaluation timeout.** Per-Policy Engine timeouts (default 5 ms per
`cpt-cf-quota-enforcement-fr-quota-resolution-policy`) compose poorly with batches: a 100-item batch could spend up to
`100 × 5 ms = 500 ms` on Engine evaluation alone. The system **MUST** therefore enforce a **batch-level evaluation
timeout** that supersedes per-Policy timeouts for the batch as a whole. The timeout is a single
**operator-configurable** flat duration applied to the entire batch evaluation; deployment-default **250 ms**.
The timeout bounds the evaluation of the batch. Once evaluation completes, applying its outcome is never interrupted,
so a batch is never reported as timed out after its effects took hold. Worst-case evaluation latency is bounded by this
timeout together with the maximum batch size (default 100 items per batch);
adaptation to load is the caller's responsibility (client-side retry / batch splitting), not a server-side concern.

A batch-level timeout fire in **atomic** mode **MUST** surface a canonical `DeadlineExceeded` error (with
`reason = "BATCH_TIMEOUT"` carried in the envelope) for the whole batch with no counter mutations applied; the caller
retries with the same envelope key (replay-safe) or, if persistent, with a smaller batch under a new envelope key. In
**independent** mode (P2), items whose Engine evaluation has not completed when the batch-level timeout fires **MUST**
be reported in the per-item array as a canonical `DeadlineExceeded` failure (`reason = "BATCH_TIMEOUT"`) with no counter
mutations for those items; items already evaluated within the budget retain their `Decision` outcomes, and items pending
in the queue at fire-time are also reported with `BATCH_TIMEOUT`.

- **Rationale**: Single-item idempotency keys do not compose naturally for multi-metric debits common in cross-resource
  operations. Atomic mode preserves the user-facing all-or-nothing semantics of a single logical operation that consumes
  multiple resources — without atomicity, a partial application leaves counters inconsistent with the operation the
  caller actually performed (e.g., input tokens deducted but the call ultimately denied on compute), forcing brittle
  compensating logic at every consumer. Independent mode is deferred to P2 because shipping it alongside atomic from day
  one risks callers picking the wrong mode for multi-metric admission and inheriting that footgun; the bulk-independent
  use case can be served in P1 by issuing N parallel single-item debits. The batch-level timeout prevents per-Policy
  timeouts from accumulating linearly into NFR-violating worst-case latency.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

### 5.8 Idempotency

#### Idempotency Guarantee for All Write Operations

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-idempotency`

The system **MUST** require a client-supplied idempotency key on every write operation except rollback: debit, credit,
return, reserve, commit, release, every batch envelope, and every batch item. A key is unique per
`(tenant_id, operation_type, key)`; a batch item is a `debit`. The subjects are part of the **request fingerprint** (a
digest of the payload), not of the key's scope. The fingerprint covers the metric's `metadata` in its canonical form
(`cpt-cf-quota-enforcement-fr-quota-metadata`). The caller is part of neither, so every caller that charges one
tenant shares the key space of that tenant: a key **MUST** be unique within the tenant and operation type, and a
random UUID is recommended. A key longer than 256 characters **MUST** be rejected with `InvalidArgument`
(`IDEMPOTENCY_KEY_TOO_LONG`) before anything is stored. The same key string under another tenant or operation type is an
independent record and is never cross-matched. A rollback names a charge and has no key of its own
(`cpt-cf-quota-enforcement-fr-rollback`); a return's key is recorded once per tenant for as long as its charge lives
(`cpt-cf-quota-enforcement-fr-return`).

Within a single `(tenant_id, operation_type, key)` scope, a submitted key falls into one of two cases:

1. **Exact replay** — same scope and identical payload as the original request. The system **MUST** return the original
   outcome (Decision — including `result`, `debit_plan`, `diagnostics`, and the originally returned identifiers and
   amounts) without re-evaluating, without modifying counters, and without re-binding the non-deterministic
   EvaluationContext fields (notably `time`). A replay run at a different wall-clock time **MUST** still produce the
   original verdict; this is required for time-gated Policies (e.g., an Engine config that blocks during a maintenance
   window) to remain replay-safe. The persisted idempotency record **MUST** capture the full Decision blob plus the
   `engine_id`, `policy_id`, and `policy_version` under which the original Decision was produced; replay diagnostics
   surface those attribution fields verbatim for audit and forensics.

1. **Payload divergence** — same scope but a payload that differs from the original (different amount, metric, or
   subjects). The system **MUST** reject with an actionable `IDEMPOTENCY_PAYLOAD_MISMATCH` error before any change and
   **MUST NOT** touch the original record.

**The charge as idempotency record.** For an allowed debit and a lease commit, the charge is the idempotency record
for as long as the charge lives (`cpt-cf-quota-enforcement-fr-return`). It stores the request fingerprint: a repeat
with the same fingerprint gets the first result, also after the replay window; a repeat with another fingerprint is a
payload mismatch.

**Exception — no applicable Quota.** A denial with reason `NO_APPLICABLE_QUOTA` is not an idempotency outcome. The
system **MUST NOT** persist it, and a later request under the same key **MUST** be evaluated as a new operation against
the Quotas present at that time. The denial changes no counter and holds no capacity, so re-evaluation can produce at
most one counter effect for the key; persisting it would instead keep the key denied for the whole replay window
after an operator provisions the missing Quota. A caller that received this denial and did not perform the work
**MUST** treat a later `Allowed` under the same key as the authorization for that work.

Replay requests **MUST NOT** carry any Decision-shaped field in the body; per §3.4, the server silently ignores any such
fields if present, regardless of which branch above would otherwise apply.

The records of every other operation — a denied debit, a credit, a lease acquire or release, a batch envelope —
**MUST** live for the **replay window** (default: 24 hours), operator-configurable per `(tenant, metric)` and
sufficient to bound legitimate retry windows. A record lives for the window that is configured when it is written; a
later change of the configuration applies to new records only. A repeat that arrives after its record is gone **MUST**
be treated as a new operation (re-evaluated against current state) — legitimate retries fall well within the window.

- **Rationale**: At-least-once delivery from upstream callers (network retries, message-queue redelivery, background-job
  duplicate execution) is a fact of life; without strict idempotency the counter integrity collapses under load.
  Payload-mismatch detection prevents accidental key reuse across distinct logical operations.
- **Key-generation guidance** (non-normative): recommended key generators are UUIDv4 (collision-safe by construction) or
  structured `<consumer-prefix>-<request-id>` patterns where `<request-id>` is itself collision-free per the consumer's
  own discipline (e.g., a UUIDv4 minted at request-receipt time). Anti-patterns include hashes of mutable inputs (
  request URL, request body) — two distinct operations that happen to share the hashed inputs would silently collide.
  The system does **NOT** validate idempotency-key generator quality and **MUST NOT** infer collision behavior from the
  key shape — collisions across distinct logical operations land as `IDEMPOTENCY_PAYLOAD_MISMATCH` errors at the
  consumer's expense. Consumers (including Quota Manager and direct Quota Consumers) own this discipline.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`, `cpt-cf-quota-enforcement-actor-quota-resolver`

### 5.9 Multi-Quota Evaluation, Quota Resolution Policy & Pluggable Engine

#### Multi-Quota Evaluation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-multi-quota-evaluation`

The system **MUST** evaluate every operation against the full set of applicable Quotas produced by subject resolution (
`cpt-cf-quota-enforcement-fr-subject-resolution`). Evaluation **MUST** consider every active Quota whose
`(subject_type, subject_id)` is in the applicable-subjects set, whose metric matches the operation's metric, and whose
conditions all hold on the request's metadata (`cpt-cf-quota-enforcement-fr-quota-metadata`). QE core selects these
Quotas before the Engine runs.
Evaluation **MUST** select the active Quota Resolution Policy (`cpt-cf-quota-enforcement-fr-quota-resolution-policy`)
and invoke the Engine that the Policy references (`cpt-cf-quota-enforcement-fr-quota-resolution-engine`) to produce a
Decision (§3.4).

Evaluation results **MUST** include, for each applicable Quota, the quota ID, type (allocation/consumption),
`enforcement_mode`, current consumed/in-flight amount, cap, and the contribution to the final Decision (e.g., "this
Quota was selected by the Debit Plan with `amount=50`"). This per-Quota detail is included in the Decision response so
callers can surface useful diagnostics.

- **Rationale**: Without explicit multi-quota evaluation, callers must perform N round trips to Quota Enforcement for N
  applicable Quotas, breaking the latency budget. Single-call evaluation with full per-Quota detail also provides the
  information needed for downstream products to render meaningful UI explanations.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Quota Resolution Policy

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-resolution-policy`

A **Quota Resolution Policy** is a separate operator-managed entity that selects which Quota Resolution Engine to use
for a given scope and supplies that Engine's config. The system **MUST** model the Policy as a first-class entity with:

- a unique stable policy ID,
- a scope (P1, closed enum): `global` or `metric=<metric_name>`. The scope representation is intentionally extensible —
  narrower scopes (e.g., per-subject) are not specified in P1 and are tracked in §13 Open Questions; the persistence and
  selection layers should be designed to admit additional scope levels without breaking changes, but no narrower scope
  is reachable through the P1 API,
- an `engine_id` referencing a registered Engine (P1: `most-restrictive-wins` or `cel`; future: any Engine registered
  per `cpt-cf-quota-enforcement-fr-quota-resolution-engine`),
- an opaque `engine_config` (engine-specific structure; e.g., for `cel` it carries the CEL expression; for
  `most-restrictive-wins` it is empty),
- an optional per-Policy evaluation timeout (operator-configurable; default 5ms; clamped to operator-configured upper
  bound),
- an optional description.

For each operation, the system **MUST** select the active Policy via most-specific-scope precedence: the `metric` Policy
if one is defined for the operation's metric, else the `global` Policy. The system **MUST** seed a built-in `global`
Policy at gear bootstrap with `engine_id=most-restrictive-wins` and empty `engine_config`; this seeded Policy **MUST
NOT** be deletable but **MAY** be replaced by an operator with a different `global`-scoped Policy referencing any
registered Engine.

Policy creation and update **MUST** delegate `engine_config` validation to the named Engine's config validator (e.g.,
the `cel` Engine performs CEL parse + type-check at this stage; the `most-restrictive-wins` Engine rejects any non-empty
config). Validation failures **MUST** be returned as actionable errors before persistence; persisted Policies are
guaranteed to carry a Engine-validated config.

For a `metric`-scoped Policy the validator **MUST** receive the resolved schema of the metric type. It describes
`request`, the `metadata` of the request's metric object. The `cel` validator **MUST** statically check references to
`request` properties against that schema and return line/column diagnostics. The metric type is read at Policy
create/update, never on the evaluation hot path. A `global` Policy applies to metrics of different types, so its
references to `request` properties are not checked at create/update: when such a Policy reads a property that a
request does not carry, the request is rejected with `POLICY_EVALUATION_FAILED`. Writing a `global` Policy that fits
every metric it serves is the operator's responsibility.

**A Policy change is the operator's responsibility.** A Policy reads no Quota Metadata, so a Policy change leaves every
Quota readable; what it changes is the arbitration among the Quotas that apply. After the change the operator can check
with the preview operation (`cpt-cf-quota-enforcement-fr-evaluate-preview`) and roll the Policy back.

Operators **MAY** create, update, and delete narrow-scope Policies; the global Policy **MUST NOT** be deleted. A Policy
referencing an `engine_id` that is not registered in the current gear deployment **MUST** be rejected at create/update
time.

**Versioning.** Every Quota Resolution Policy is versioned per
`cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`. Updates create a new immutable version; old versions
remain queryable; rollback is a first-class operation. Evaluation always selects the active version per `policy_id`;
Decision diagnostics carry the `policy_version` evaluated under, so audit and replay attribute every Decision to a
specific Policy state. The seeded built-in global Policy materializes at bootstrap as `policy_version = 1` and follows
the same versioning rules thereafter.

**`engine_config` content discipline — no secrets, no sensitive data.** A Policy's `engine_config` carries arbitration
\* *logic*\*, not sensitive **data**: CEL expressions (and future Starlark / Lua / Wasm engine configs) **MUST NOT**
embed credentials, API keys, tokens, signing keys, pricing or commercial rates, customer identifiers, or any other
content the operator considers sensitive. Engine config is classified as Platform Operational Data per §6.2 alongside
Quota state — it is visible to platform operators with Policy-management PDP grants, surfaced in operation logs and
audit trails (when those land in P2), and not encrypted at rest by the P1 storage plugin. Sensitive values that the
Policy needs to consult **MUST** be referenced indirectly through the validated `request` object (set by the calling service from a
sanctioned config source) — never inlined into the engine_config payload itself. Operators are responsible for upholding this discipline; the system
does not inspect engine_config content for secrets.

- **Rationale**: Separating the Policy entity from the Engine cleanly maps two independent operator concerns: *where* (
  which scope) and *how* (which Engine + config). The P1 scope ladder is deliberately minimal — only `global` and
  `metric` — because no concrete operator use case in P1 requires narrower targeting, and a wider ladder introduces
  selection ambiguities (e.g., multiple subject types simultaneously present in the applicable-subjects set produce a
  tie-break problem with no obvious right answer). Keeping the scope representation extensible without prescribing
  additional levels lets P2 (or later) add narrower scopes once production demand surfaces and the right shape is clear.
  Delegating config validation to the Engine keeps the Policy entity engine-agnostic and avoids hardcoding parser logic
  for every supported language in the Policy layer. A per-Policy timeout (rather than a per-Engine global) lets
  operators cap risky customizable-Engine evaluations independently of the fast `most-restrictive-wins` path.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Quota Resolution Policy Versioning

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`

The system **MUST** treat every Quota Resolution Policy record as **immutable per version**. Updates **MUST NOT** mutate
existing fields in place; instead, every change produces a new version with a higher `policy_version` integer, and a
separate latest-pointer per `policy_id` names the currently-active version. Rollback to a previous version is a
first-class operation.

**Version record shape**. Each Policy version carries:

- `policy_id` — stable identifier shared across all versions of the same Policy.
- `policy_version: uint32` — monotonically increasing per `policy_id`; first version is `1`.
- `created_at: RFC3339_UTC_ms` — emission timestamp of this version.
- `created_by: string` — caller identity from SecurityContext at version creation (operator / Quota Manager). Captured
  for audit attribution.
- `comment: string` (optional) — operator-supplied free-text note (change rationale, ticket reference, author, or any
  other annotation).
- `version_state: enum { active, superseded, rolled_back, deleted }`:
  - `active` — at most one version per `policy_id` carries this state at any time; the version pointed to by latest.
  - `superseded` — replaced by a later `active` version through normal update flow.
  - `rolled_back` — was active but has been abandoned via rollback (replaced by an earlier version becoming active
    again). Terminal — never re-activated.
  - `deleted` — set on the previously-active version when the entire `policy_id` is soft-deleted via `delete_policy`.
    Terminal — never re-activated. After deletion the `policy_id` has no active version; evaluation falls through to the
    next-most-specific scope per the precedence ladder in `cpt-cf-quota-enforcement-fr-quota-resolution-policy`.

**Bootstrap.** The seeded built-in `global` Policy (per `cpt-cf-quota-enforcement-fr-quota-resolution-policy`)
materializes at gear bootstrap as
`policy_id = global, policy_version = 1, version_state = active, engine_id = most-restrictive-wins, engine_config = {}`.
Operator updates produce versions 2, 3, …. When narrower-scope Policies are deleted or rolled back, the seeded global
remains the ultimate fallback — evaluation never enters a "no Policy applies" state.

**Latest-pointer.** Per `policy_id`, the system **MUST** maintain a separate latest-pointer that names the
currently-active `policy_version`. The latest-pointer is updated atomically with version creation or rollback. Readers
default to the latest version unless they explicitly request a specific version.

**Operations** (version-based optimistic concurrency, no idempotency-key requirement):

- `create_policy(scope, engine_id, engine_config, [comment])` — creates `policy_version = 1` with
  `version_state = active`; latest-pointer initialized. Rejected with `POLICY_SCOPE_OCCUPIED` (409) if an active Policy
  already exists at the exact same scope.
- `update_policy(policy_id, if_match_version, engine_id?, engine_config?, [comment])` — creates `policy_version = N+1`
  with `version_state = active`; previous active version transitions to `superseded`; latest-pointer moved to `N+1`.
  **Rejected with `VERSION_CONFLICT` if `if_match_version` does not equal the current latest** (lost-update protection).
  `if_match_version` is a required positive integer; a request without a valid one is rejected before any write.
  Atomic.
- `rollback_policy(policy_id, target_version, [comment])` — makes `target_version` active again (its `version_state`
  returns to `active`); the previously-active version transitions to `rolled_back` (terminal). Latest-pointer moved to
  `target_version`. Atomic. Rejected with `UNKNOWN_POLICY_VERSION` if `target_version` does not exist; rejected with
  `VERSION_ROLLED_BACK` if `target_version` is in `rolled_back` state; rejected with `POLICY_DELETED` if the Policy has
  been deleted, since its versions are terminal. Naturally idempotent on retry against the same
  target.
- `delete_policy(policy_id, [comment])` — soft-deletes a narrow-scope Policy entirely. The currently-active version
  transitions to `deleted` (terminal); the latest-pointer is cleared so subsequent evaluations falling within this
  Policy's scope fall through to the next-most-specific scope per `cpt-cf-quota-enforcement-fr-quota-resolution-policy`.
  Historical versions retain their existing `superseded` / `rolled_back` state — only the active version moves to
  `deleted`. The seeded global Policy **MUST NOT** be deletable; `delete_policy` against it is rejected with
  `CANNOT_DELETE_SEEDED_GLOBAL_POLICY`. A subsequent `delete_policy` against an already-deleted `policy_id` is a no-op
  (idempotent). A `policy-changed` notification event is emitted with `change_kind = "deleted"` per
  `cpt-cf-quota-enforcement-fr-notification-plugin`. All retained versions (`superseded`, `rolled_back`, and the
  `deleted` terminal version) follow the standard 90-day retention window per the **Retention** paragraph below; after
  the window they are hard-deleted by the storage retention sweeper.
- `read_policy(policy_id, version=N | latest)` — returns the specified or current version. After
  `delete_policy(policy_id)`, `latest` returns the `deleted` terminal version (so audit and inspection remain possible
  for the retention window); explicit historical-version reads continue to work until hard-delete.
- `list_policy_versions(policy_id)` — returns the ordered list of versions with `policy_version`, `created_at`,
  `created_by`, `comment`, `version_state`.

**Atomicity.** Version creation, latest-pointer update, and previous-version state transition (`active → superseded` on
update; `active → rolled_back` on rollback; `active → deleted` plus latest-pointer clear on delete) **MUST** commit
atomically. No reader observes intermediate states; any concurrent evaluation either sees the prior version, the new
version, or (post-delete) falls through to the next-most-specific scope, never an inconsistent mix.

**EvaluationContext + Decision diagnostics.** The `EvaluationContext.active_policy` field exposes both `policy_id` and
`policy_version` to Engines. Decision `diagnostics` **MUST** include `engine_id`, `policy_id`, and `policy_version` of
the Policy that produced the Decision so operators inspecting a Decision (via
`cpt-cf-quota-enforcement-fr-evaluate-preview` or via idempotency replay) see exactly which Engine and Policy version
were active at evaluation time. The same three fields are persisted by the idempotency layer per
`cpt-cf-quota-enforcement-fr-idempotency`, so replay Decision diagnostics are identical in shape to live ones.

**Idempotency replay attribution.** Decisions stored in the idempotency cache (per
`cpt-cf-quota-enforcement-fr-idempotency`) retain the original `policy_version`. Replay surfaces the version under which
the original Decision was made, even if the Policy has subsequently been updated to a higher version — the stored
Decision is the source of truth, and forensic analysis can attribute Decisions to specific Policy state.

**Retention.** `superseded`, `rolled_back`, and `deleted` versions are retained for an operator-configurable window
(default: **90 days**) for audit forensics, then hard-deleted by the storage retention sweeper. `active` versions are
never auto-deleted — they remain until explicitly replaced via update, rolled back, or soft-deleted via `delete_policy`.
Cross-reference §6.2 retention table.

**Telemetry.** The system **MUST** expose counters for policy version transitions (total create / update / rollback /
delete) and for `VERSION_CONFLICT` rejections. Label cardinality is a DESIGN-time concern per the existing
telemetry-cardinality discipline.

- **Rationale**: Versioning is the foundation for safe Policy evolution — without it, rollback requires re-editing live
  state (re-introducing race conditions), audit cannot attribute Decisions to specific Policy state, and any future
  shadow-evaluation / promotion / canary mechanism (P2 forward-pointers in §13) lacks a primitive to point at.
  Optimistic concurrency via `if_match_version` keeps the control-plane mutation path simple and avoids the overhead of
  idempotency-record retention for low-frequency operations. Retaining old versions for 90 days serves audit forensics
  without unbounded storage growth.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-manager`

#### Quota Resolution Engine — Pluggable Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-resolution-engine`

The system **MUST** define a **Quota Resolution Engine** plugin contract and **MUST** ship at least two built-in Engines
registered at gear bootstrap: `most-restrictive-wins` (hardcoded; no config; fastest path) and `cel` (sandboxed CEL
evaluator; customizable). Additional Engines (e.g., Starlark, Lua, Wasm-loaded operator engines) are P2-or-later
candidates and **MUST** plug in via the same contract without changes to the multi-quota-evaluation core.

Each Engine **MUST** expose:

- a stable engine_id (matched against Policies' `engine_id` field),
- a config-validation entry point — invoked at Policy create/update; consumes the raw `engine_config`, returns a
  validated/parsed representation or a structured error,
- an evaluate entry point — invoked per operation; receives the EvaluationContext (see below) and the validated config;
  returns a Decision.

The EvaluationContext passed to evaluate **MUST** carry:

- `applicable_quotas` — the full list of applicable Quotas, whose conditions already hold on the request
  (`cpt-cf-quota-enforcement-fr-quota-metadata`), with id, subject reference, subject tier, type,
  `enforcement_mode` (per `cpt-cf-quota-enforcement-fr-enforcement-mode`), cap, period boundary (consumption only),
  and `validity_window` (when set; else absent — see `cpt-cf-quota-enforcement-fr-quota-lifecycle`); the system has
  already applied the conditions of Quota Metadata, so the Engine does not check them,
- `current_usage` — the current consumed/in-flight amount per applicable Quota,
- `active_policy` — the active Quota Resolution Policy's `policy_id` and `policy_version` (per
  `cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`); Engines see the version they are running under so
  audit / replay diagnostics carry it through,
- `request` — the `metadata` of the request's metric object (forwarded by the calling service for debit, reserve,
  preview, and each batch item; validated against the schema of the metric type; QE core reads it only to match the conditions of Quota Metadata;
  classified as Platform Operational Data per §6.2 — calling services are responsible for sanitizing values before
  forwarding, since QE persists request metadata in operation logs, idempotency state, and Engine diagnostics where it
  remains visible to authenticated platform actors); this is the value exposed to CEL as `request`,
- `metric`, `metric_id`, and `amount` — the metric type of the request's metric object, its optional `id` (exposed
  to CEL as `metric_id`, an empty string when the request names no `id`), and the requested integer amount,
- `time` — the current evaluation timestamp (UTC).

The stable CEL namespace for operation attributes is `request`, the validated `metadata` of the request's metric
object. A `cel` Policy sees each Quota's subject type and tier but not its subject id. Authenticated principal data and the tenant and subject identifiers of
the request **MUST NOT** appear in the input of a `cel` Policy. An Engine receives the subject reference of each applicable Quota and **MUST NOT**
let a subject id affect apportionment, selection, or counter keys.

The Decision returned by the Engine **MUST** match the contract in §3.4: `result`, `debit_plan` (a list of chains),
`diagnostics`. The system **MUST** enforce the following Debit-Plan
invariants on every returned Decision; any violation **MUST** surface a platform-canonical error (fail-closed) with no
counter mutation, and the Decision shape **MUST NOT** be returned to the caller:

1. Every quota_id in `debit_plan` **MUST** be a member of `applicable_quotas`. The Engine cannot invent or reference
   Quotas outside the EvaluationContext.
2. For every entry: `amount ≥ 0`.
3. In every chain the amounts add up to exactly `request.amount`.
4. A Quota appears at most once in the plan.
5. Result-plan consistency:
   - `result = Allowed` ⇒ `debit_plan` is non-empty.
   - `result = Denied` ⇒ `debit_plan` is empty.

A cascade or a weighted split is one chain with several entries, primary pool first; an AND, where each Quota counts
the whole operation, is one chain per Quota (ADR-0010). Engines commit to integer per-entry amounts; rounding choices
in CEL or future Engines are the Engine author's responsibility.

**Unwinding.** A lease commit below the held amount and a return (`cpt-cf-quota-enforcement-fr-return`) take an amount
X back from a charge the same way: in every chain, X is taken from the entries in reverse order, last entry first,
each entry at most what it still holds, so each chain gives back exactly X.

Engines **MUST** be deterministic given the EvaluationContext (the system relies on this for idempotent replay). Engines
**MUST NOT** perform I/O. The system **MUST** enforce a per-Policy evaluation timeout (default 5ms, see Quota Resolution
Policy FR); on timeout, the system surfaces a canonical `DeadlineExceeded` error (fail-closed) and the Engine's partial
Decision (if any) is discarded.

Engine plugins are registered in-process at gear bootstrap. Adding a new Engine to a deployment requires building a
gear binary that includes the Engine; runtime registration of arbitrary user-supplied Engines is out of scope.

**Bootstrap failure is fail-fast.** If any built-in Engine declared in the deployment manifest fails to register at
gear bootstrap (e.g., the `cel` evaluator throws during initialization, a future Wasm-loaded Engine fails to compile,
the Engine binary version mismatches the QE core version), the gear **MUST** fail readiness and **MUST** refuse to
serve requests. The system **MUST NOT** silently fall back to a different Engine for Policies that referenced the failed
one — silent fallback is unsafe because operator Policies referencing the unavailable Engine would change behavior
without operator awareness, potentially widening enforcement (a CEL Policy that denied is replaced by
`most-restrictive-wins` that allows). Failed-bootstrap state **MUST** surface a structured log entry and a telemetry
counter `engine_bootstrap_failures_total` so operators can diagnose without reading log files. Recovery requires fixing
the registration failure and restarting the gear.

##### P1 Built-in Engines

The system **MUST** ship the following two built-in Engines in P1; both **MUST** be registered automatically at gear
bootstrap:

(1) `most-restrictive-wins` — hardcoded for maximum throughput; rejects any non-empty `engine_config`. Behavior:

- **Empty applicable-Quotas set**: `result = Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` — for a
  `QuotaGated` metric, absence of any applicable Quota means absence of authorization (per §3.2). `debit_plan` is empty.
- **Validity-window prefilter (default)**: Quotas whose `validity_window` is set and whose `time` falls outside
  `[validity_start, validity_end]` are excluded from consideration before the cap comparison runs (per
  `cpt-cf-quota-enforcement-fr-quota-lifecycle` validity-window semantics). Quotas without a `validity_window` field are
  always considered. If every applicable Quota is excluded by the prefilter, the post-prefilter set is empty and the
  result is `Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` — same outcome as if no Quotas existed.
  Operators wanting a softer "out-of-window Quotas don't deny but also don't constrain" outcome express it through a
  `cel` Policy.
- **`request` is ignored.** This Engine does not read the Request Metadata. Region-gated, tier-gated, and
  environment-gated caps work under it unchanged, because QE core has already left out the Quotas whose conditions do
  not hold (`cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`).
- **Binding-Quota selection.** A Quota is satisfiable if its remaining capacity is ≥ `request.amount` (unbounded Quotas
  — `cap = null` — are trivially satisfiable, since remaining is infinite). The binding Quota is selected from the
  satisfiable set by, in priority order:
  1. **Subject tier** — the `Subject` tier before the `Tenant` tier; every subject type other than the tenant type is
     in the `Subject` tier.
  2. **Bounded > unbounded** within the chosen tier — operator's explicit cap takes precedence; unbounded becomes
     binding only as overflow when no bounded satisfiable Quota exists in the tier.
  3. **Smallest remaining capacity** within bounded satisfiable Quotas of the chosen tier (literal "most restrictive");
     ties broken by ascending `quota_id` (UUIDv7). Among unbounded Quotas (reached only when rule 2 falls through),
     ascending `quota_id` is the sole tiebreaker.
- `result = Denied(violated_quota_ids, …)` when no Quota is satisfiable — every applicable bounded Quota has remaining
  capacity below `request.amount` and no applicable unbounded Quota exists. `violated_quota_ids` names every such
  bounded Quota (no short-circuit; full enumeration). Unbounded Quotas are never violators.
- else `result = Allowed`. `debit_plan` is one chain with one entry against the binding Quota at `amount =
  request.amount`; non-binding applicable Quotas are absent from the plan and their counters are not mutated. The
  Engine charges one Quota with room, so a subject whose Quota is exhausted spills into the tenant Quota. A Policy
  that must hold every cap, for example of both a user and a device, is a `cel` Policy that charges each Quota, one
  chain per Quota.
- **Engine-specific invariant (stricter than the general Engine contract):** on `result = Allowed` the Debit Plan
  **MUST** be exactly one chain with one entry. The system enforces this at the Engine boundary in addition to the
  general invariants; any deviation surfaces a canonical `Internal` error.

(2) `cel` — sandboxed CEL evaluator. Config: `{ expr: <CEL string> }`. Behavior:

- Validation (Policy create/update): parse + type-check expression against the EvaluationContext, the schema of the
  metric type for `request`, and the Decision return schema; reject errors with line/column.
- Per operation: bind the EvaluationContext into the CEL environment; evaluate the expression under sandbox (no I/O;
  deterministic; fixed step/cost cap, default tuned to the per-Policy timeout); interpret the returned record as a
  Decision whose `debit_plan` is a list of chains, `[[{id, amount}, ...], ...]`, and apply the standard Debit-Plan
  invariants.
- Runtime errors (cost-cap exceeded, type error at evaluation, malformed return record) **MUST** surface a
  platform-canonical error (per §3.4 failure surface) with no counter mutation.

P2 Engine candidates (Starlark, Lua, Wasm) plug in via the same contract; selection criterion: sandboxability (no I/O),
determinism, predictable resource limits, fast startup. The choice is deferred to a later PRD update once production CEL
coverage is observed.

- **Rationale**: A pluggable Engine contract treats arbitration logic the same way the platform already treats storage
  and notification — as a versioned trait with a small set of in-tree implementations and room for future ones. Strict
  Debit-Plan invariants enforced at the Engine boundary mean malformed Engines (including future bugs in
  operator-authored CEL) cannot corrupt counters; the system always converts invariant violations into a canonical
  `Internal` error rather than partial mutations. P1 ships both a fastest-possible hardcoded path
  (`most-restrictive-wins`) and a customizable path (`cel`) so cascade and similar arbitration patterns are expressible
  from day one without sacrificing the default-path latency budget.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Cascade Arbitration Capability

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-cascade`

The Engine layer (per `cpt-cf-quota-enforcement-fr-quota-resolution-engine`) **MUST** be expressive enough for cascade
arbitration — directing debits at one Quota (the "primary pool") first and falling through to another (the "fallback
pool") only when the primary's remaining capacity is insufficient. P1 ships two complementary capabilities:

- **Built-in subject-tier cascade** — the default `most-restrictive-wins` Engine cascades from the `Subject` tier to
  the `Tenant` tier with one-entry Debit Plans. No operator authoring required.
- **Customizable cascade** — the `cel` Engine produces chains of several entries expressing cross-tier **splits**
  (primary takes `X`, fallback takes `amount − X`), intra-tier cascade between same-tier Quotas identified by
  subject type or id, weighted splits, and multi-tier (3+ pool) cascades. Standard Debit-Plan invariants from
  `cpt-cf-quota-enforcement-fr-quota-resolution-engine` apply uniformly.

Future Engine plugins added to QE **MUST** preserve at least the customizable cascade expressiveness.

**Reference scenario.** For the `llm_gateway` owner's AI-token metric, tenant T owns a `(user subject type, U)` Quota
`user_q` with `cap=100, remaining=20` and a `(tenant subject type, T)` Quota `tenant_q` with `cap=10000,
remaining=9700`. A debit of 50 that names `U` reaches both:

- `most-restrictive-wins`: `user_q` not satisfiable (20 < 50); cascade falls through to `tenant_q`. Result: `Allowed`,
  `debit_plan = [[{ tenant_q.id, amount: 50 }]]`; `user_q.remaining` stays at 20.
- `cel` split-cascade Policy: `user_q` contributes its full remaining; `tenant_q` covers the residual. Result:
  `Allowed`, `debit_plan = [[{ user_q.id, amount: 20 }, { tenant_q.id, amount: 30 }]]`.

The split-vs-fallthrough difference appears only when the primary has some but insufficient remaining; with
`user.remaining = 0`, both Engines produce the same single-entry plan against the fallback pool.

- **Rationale**: Cascade is the most operator-requested non-default arbitration pattern (license-pack workflows:
  per-user pool first, tenant top-up pool as fallback). The default `most-restrictive-wins` covers the common case
  without operator authoring; `cel` adds split-cascade and weighted splits for license-overage and
  multi-tier billing workflows. Declaring both as a first-class **capability obligation** on the Engine layer anchors
  operator expectations and prevents future Engine candidates from dropping the expressiveness needed.

- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Attribute-Based Quota Selection (Metadata-Gated)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`

Operators frequently need attribute-based Quota selection — region-gating, tier-gating, or environment-gating, where
each operation is counted against the Quotas whose attributes match the operation's context. The conditions of a
Quota's metadata express this (`cpt-cf-quota-enforcement-fr-quota-metadata`), and QE core applies them under every
Engine.

The system **MUST** guarantee:

- **One schema for both sides.** The Request Metadata is validated against the metric type, and a Quota's conditions
  name properties of the same schema and are checked against it when the Quota is written.
- **Selection before arbitration.** `applicable_quotas` contains only the Quotas whose conditions hold on the request.
  A Quota whose condition names a property the request lacks is not in the set.
- **Arbitration stays in the Engine.** Among the Quotas that apply, the active Policy decides which are charged, as
  for Quotas without conditions.

When no Quota applies, the result is `Denied` with `NO_APPLICABLE_QUOTA` (§5.9); a metric that needs unconstrained
consumption is declared `direct` by its metric type.

Worked example. Tenant T owns user U; operator wants per-region caps for metric `gpu-hours`, whose metric type
declares `metadata.region` as a string:

- Quotas on `(user subject type, U)`: `Q_us(metadata={region:["us-east-1"]}, cap=100)`,
  `Q_eu(metadata={region:["eu-west-1"]}, cap=50)`.

`debit(metric={type: gpu-hours, metadata: {region: "us-east-1"}}, amount=20)`:

- `applicable_quotas = {Q_us}`: the condition of `Q_eu` does not hold,
- the default Policy returns `Allowed` with `debit_plan = [[{ Q_us.id, amount: 20 }]]`,
- `Q_us.consumed += 20`; `Q_eu` untouched.

Policies **MUST NOT** branch, apportion shared budgets, select Quotas, or construct Debit Plans from the authenticated
service principal or the target tenant/subject identifiers. Those fields are authorization and lookup inputs, not
arbitration data.

The default Policy `most-restrictive-wins` (per §5.9) needs no change for attribute-based selection: it chooses among
the Quotas that apply.

- **Rationale**: Region-, tier-, and environment-gating are the most-requested attribute-based patterns in production
  deployments. QE core selects the Quotas by one closed rule, equality or membership in a list
  (`cpt-cf-quota-enforcement-fr-quota-metadata`), so the selection is the same for every Engine and a mistake in a
  condition is found when the Quota is written. Arbitration among the selected Quotas stays in the Policy, so
  operators compose attribute-based selection with cascade and other patterns. Metadata-agnostic subject resolution means future subject types (resource-group
  hierarchy, P3) inherit the capability without any QE-side change.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

### 5.10 Quota Snapshot Read API

#### Quota Snapshot Read for a Subject

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-quota-snapshot-read`

The system **MUST** provide an S2S read API that returns, for explicit PDP-authorized tenant/subject/metric filters, a
**Quota Snapshot**: the list of every applicable Quota with its current per-Quota state. A filter **MAY** carry the
`metadata` of a request's metric object; it then selects only the Quotas whose conditions hold on that metadata
(`cpt-cf-quota-enforcement-fr-quota-metadata`), the set a debit with that metadata is checked against, and the
metadata **MUST** fit the metric type. A filter without `metadata` selects the Quotas whatever their conditions.
Subject admission applies per
`cpt-cf-quota-enforcement-fr-subject-resolution`; the response is engine-agnostic — no aggregate "headline" cap or
balance is computed, since under cascade, split, or attribute-gated Engines no single number is universally meaningful.
Callers needing an admission verdict for a specific operation **MUST** use the read-only Decision Preview
(`cpt-cf-quota-enforcement-fr-evaluate-preview`), which runs the active Engine and returns the would-be `Decision` and
`debit_plan`.

For each applicable Quota the response **MUST** include:

- `quota_id`, subject reference, metric, quota type (`allocation` / `consumption`),
- `cap` (numeric or `null` for unbounded Quotas per `cpt-cf-quota-enforcement-fr-quota-lifecycle`), current consumed (or
  in-flight, for allocation), `remaining` (numeric, or `null` when `cap` is `null`),
- `enforcement_mode` (per `cpt-cf-quota-enforcement-fr-enforcement-mode`),
- the period boundary and next reset timestamp (consumption types only) or "no period" (allocation types),
- the Quota's `metadata` map (subject to PDP scoping),
- `validity_window` (when set) plus a server-computed boolean `currently_within_window` so callers can render expiry
  state without recomputing the comparison.

The response **MUST NOT** carry Quota Resolution Policy attribution (no `policy_id`, `policy_version`, `scope`,
`engine_id`, `engine_config`, or any summary/content-hash thereof). Snapshot is per-Quota state only. Callers needing to
attribute the read to a specific Policy state **MUST** use `cpt-cf-quota-enforcement-fr-evaluate-preview` (whose
`diagnostics` carry `policy_id` and `policy_version` per
`cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`) or the Policy-read API exposed by the same versioning
FR.

The resolver plugin serves the Quota Resolver's quota state from this read
(`cpt-cf-quota-enforcement-fr-resolver-plugin`) and **MUST** leave out the fields only operators see: the Quota's
`enforcement_mode` and validity window (`validity_window`, `currently_within_window`). It returns the Quota's
`metadata`, so that a caller can tell which requests each Quota applies to. The Quota
Resolver's tenant plane shows quota state to any authenticated end user. On the plugin path a Quota outside its
validity window **MUST** be left out, and a metric that does not admit the subject's type **MUST** yield only the
tenant's Quotas, not an error.

- **Rationale**: Returning the per-Quota list, rather than an aggregate "effective cap", keeps the read engine-agnostic
  and avoids baking `most-restrictive-wins` semantics into the data model. UIs and dashboards compose the display they
  need (single progress bar of the minimum, multi-row breakdown, cascade-aware "user pool first, then tenant", etc.) on
  top of the same primitive. The authoritative "would this operation be admitted?" question is answered by the Decision
  Preview, which exercises the active Engine instead of guessing.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`, `cpt-cf-quota-enforcement-actor-quota-reader`,
  `cpt-cf-quota-enforcement-actor-quota-resolver`

#### Bulk Quota Snapshot Read

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-bulk-quota-snapshot-read`

The system **MUST** support a bulk Quota Snapshot read API that returns the per-Quota state for multiple
`(subject, metric)` pairs in a single call, subject to the caller's PDP-authorized scope. Bulk reads **MUST** be
paginated when the result set exceeds an operator-configured page size (default: 100 entries per page) and **MUST**
support cursor-based continuation.

- **Rationale**: Dashboards and billing systems need to read quota state for many subjects at once; per-subject calls do
  not scale and break latency budgets.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-reader`

#### Self-Service Quota Snapshot Read

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-end-user-quota-snapshot-read`

An end user **MUST** be able to read their own quota state through the Quota Resolver's tenant plane, which reaches QE
through the resolver plugin. End users **MUST NOT** call QE directly. A product backend may also serve an end-user
view through QE's S2S snapshot by supplying explicit tenant/subject attribution authorized against its service
principal. Both paths **MUST**:

- return only Quotas of the supplied subject and its tenant — no cross-subject or cross-tenant Quotas (on the plugin
  path the Quota Resolver fixes the subject to the caller's mapped principal and the tenant to the caller's tenant),
- return **every** applicable Quota for that subject — quotas that govern a subject's consumption are transparent to
  that subject; the plugin path leaves out Quotas outside their validity window,
- return the per-Quota state contract of `cpt-cf-quota-enforcement-fr-quota-snapshot-read`, without the operator-only
  fields on the plugin path,
- carry no Quota Resolution Policy attribution — same rule as the base snapshot; Policy-attribution callers route
  through `cpt-cf-quota-enforcement-fr-evaluate-preview` or the Policy-read API.

On QE's own API the complete explicit target **MUST** be authorized against the authenticated service principal before
storage.

- **Rationale**: Letting users observe their own quota state without going through a tenant administrator is
  fundamental to a self-service platform; the Quota Resolver serves it without a policy grant per user. Quotas are
  transparent to the subjects they govern: every Quota applicable to the subject and its tenant is surfaced. On QE's
  own API, PDP scoping is the only partitioning mechanism — operator-level grants see across tenants/subjects,
  consumer and tenant-admin grants see only their own subject set; no per-Quota or per-key invisibility primitive
  exists.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-resolver`, `cpt-cf-quota-enforcement-actor-quota-consumer`

### 5.11 Quota Enforcement Mode

#### Enforcement Mode Closed Enum

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-enforcement-mode`

Every Quota **MUST** carry an `enforcement_mode` field drawn from GTS instances under base
`gts.cf.core.qe.enforcement_type.v1~`. **P1 reserves one instance**:
`gts.cf.core.qe.enforcement_type.v1~cf.core.qe.hard.v1` (`hard`) — operations whose execution would cause a
`hard` Quota's remaining capacity to drop below zero are denied per `cpt-cf-quota-enforcement-fr-hard-quota-reject`;
counter integrity is preserved (the counter never crosses cap).

> **Notation.** Throughout this document, enforcement modes are referenced by their short instance names (`hard`,
> future `hard-with-clamp`) in prose for readability. API requests, storage rows, and outbox events use the full GTS
> URI form shown above.

A Quota's `enforcement_mode` is set at creation and does not change: a Quota with another mode is a new Quota
(`cpt-cf-quota-enforcement-fr-quota-lifecycle`). The field is anchored to the GTS instance form so future modes are
addable as new instances without API breakage.
Future phases MAY extend additively:

- `hard-with-clamp` (P3 candidate, see §13) — for batch-style workloads where admitting a clamped magnitude is
  preferable to rejecting outright. Requires registering a new instance
  `gts.cf.core.qe.enforcement_type.v1~cf.core.qe.hard_with_clamp.v1` and the Decision contract to gain an
  `AllowedWithClamp(quota_id, admitted_magnitude)` arm, whose chains add up to the admitted magnitude.

- **Rationale**: A closed enum centralizes the "what does this Quota do at the cap?" decision in a single field rather
  than scattering it across ad-hoc boolean flags. The P1 single-value enum keeps the model simple while reserving design
  space for the cap-clamp future extension without forcing it now. Commercial overage pricing is intentionally deferred
  to the billing layer per §4.2 — billing composes Usage Collector observations with Quota records to compute over-cap
  consumption without QE itself admitting over-cap operations.

- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-quota-consumer`

#### Cap Violation Rejection

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-hard-quota-reject`

The system **MUST** deny any operation whose execution would cause a Quota's remaining capacity to drop below zero
(`hard` being the only `enforcement_mode` value in P1; see `cpt-cf-quota-enforcement-fr-enforcement-mode`). The denial
decision **MUST** identify each violating Quota by quota ID, the requested amount, the current remaining capacity, and
the violation amount. Counters **MUST NOT** be modified for denied operations.

- **Rationale**: Cap-violation rejection is the P1 strict-enforcement primitive; its violation must be unambiguous and
  auditable. Multi-Quota evaluation (§5.9) ensures every violating Quota is named (no short-circuit on the first
  violation) so callers and audit consumers see the full denial reason.
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-consumer`

### 5.12 Tenant Isolation

#### Strict Tenant Isolation on All Operations

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-tenant-isolation`

The system **MUST** enforce strict tenant isolation on every read and write operation against per-tenant entities.
Every Quota, counter, lease, charge, idempotency record, and operation log entry **MUST** carry the target `tenant_id`
at write time: PDP-authorized on QE's own API, named by the Quota Resolver on the resolver plugin path. Every read and
write **MUST** bind that tenant plus the access scope at storage: the PDP-returned scope on QE's own API, a scope that
names the request's tenant only on the plugin path (ADR-0009). Cross-tenant read or write operations **MUST** be
rejected, with the only exception being explicit operator-level PDP grants for platform-wide operator activities
(e.g., aggregated utilization dashboards).

Operator-managed platform-wide entities — Quota Resolution Policies, registered metric types, and registered Quota
Resolution Engines — are **not** tenant-scoped. Their addressing uses scope discriminators specific to each entity
(e.g., a Policy's `scope` field per `cpt-cf-quota-enforcement-fr-quota-resolution-policy`) rather than a `tenant_id`
column. Mutation of these entities is gated by operator-level PDP grants and is out of scope for tenant isolation
enforcement.

- **Rationale**: Tenant isolation is a baseline security and compliance requirement (REQ.md §9). Tagging every persisted
  row with `tenant_id` at write time makes isolation enforceable at the storage layer rather than only at the API
  boundary.
- **Actors**: All actors

### 5.13 Authorization

#### PDP-Gated Authorization

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-authorization`

The system **MUST** authorize every operation of QE's own API (REST and in-process) via the platform PDP
(`authz-resolver`). Authorization \*
*MUST*\* verify that the caller's authenticated identity (from SecurityContext) is permitted to perform the requested
operation against the targeted subject and metric. PDP-returned constraints (e.g., scoping a read to specific tenants)
\* *MUST*\* be applied as additional filters before query execution. Authorization failures **MUST** result in immediate
rejection with no partial operation; the system **MUST** fail closed (see §3.4 — "Quota Enforcement itself fails closed
on internal errors").

Consumer operations **MUST** supply the target `tenant_id`, subjects, and metric with its type and optional id; a
rollback supplies them for the operation whose charge it names, with the charge's operation and key. QE **MUST**
authorize the attribution of a rollback as it authorizes the attribution of a debit, and **MUST** then check, by the
attribution digest (`cpt-cf-quota-enforcement-fr-return`), that it is the attribution of the charge. A charge outside
the caller's access, or with another attribution, **MUST** get the same answer as an unknown charge. QE **MUST**
reject malformed public request/target shape before authorization, send the complete structurally valid untrusted
tuple to PDP, and proceed only when PDP authorizes it for the authenticated service principal. Subject admission and
validation of the metric object **MUST** occur only after authorization. Management operations likewise carry an
explicit target and apply PDP-returned scope. End-user principals never call QE consumer endpoints directly.

The authenticated service principal is the caller identity; QE **MUST NOT** accept a second self-declared caller
identity. Neither principal data nor attribution identifiers may influence Policy branching, quota apportionment,
Quota selection, Debit Plans, or counter/allocation keys beyond selecting the authorized subjects' Quotas.

**Resolver plugin exception.** The resolver plugin path (`cpt-cf-quota-enforcement-fr-resolver-plugin`) calls no PDP:
its trusted admission checks the target's shape and returns an access scope that names the request's tenant only.
Trusted admission is reachable only through the plugin client, and policy management in it is always refused. The
trust boundary is the process: any module in the same process can obtain the plugin client and is trusted like a
caller of the Quota Resolver's platform plane. When the Quota Resolver authorizes its platform-plane callers, calls
that come through it are authorized there; the plugin client itself stays without a policy check (ADR-0009). Until
then no component checks which module may charge, return, or read the quotas of any tenant on this path. This is an
accepted exception, the same one that the Quota Resolver records for its platform plane; authorization of
platform-plane callers is tracked in #6037.

- **Rationale**: Centralizing authorization in the platform PDP keeps policy decisions out of the data plane —
  operators adjust permissions without redeploying QE, and authz logic stays under a single audit boundary in
  `authz-resolver`. On QE's own API, requiring PDP to authorize the complete supplied attribution tuple is what makes
  tenant isolation enforceable: a misconfigured or compromised service cannot charge another tenant merely by changing
  request fields. Failing closed on PDP unavailability prevents an outage from degrading into a permissive bypass. The
  same discipline is applied by Usage Collector (`cpt-cf-usage-collector-fr-tenant-attribution`,
  `cpt-cf-usage-collector-fr-ingestion-authorization`) for the same reasons.
- **Actors**: `cpt-cf-quota-enforcement-actor-authz-resolver`

### 5.14 Pluggable Storage Backend

#### Pluggable Storage Backend Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-pluggable-storage`

The system **MUST** define a storage plugin contract that abstracts the persistence layer for Quotas, counters,
leases, charges, Quota Resolution Policies, idempotency records, and operation logs. The contract **MUST** expose
primitives sufficient to support transactional debit, credit, rollback, return, reserve, commit, and release
operations with strong consistency within a single tenant scope. P1 ships a single plugin implementation built on
`toolkit-db`; alternative backends are viable under the same plugin contract. The operator selects the active plugin
via configuration.

The plugin contract **MUST** be versioned with the gear's major version; plugins implementing previous contract
versions are not supported in newer gear versions.

- **Rationale**: Pluggability avoids storage lock-in and lets future deployments adopt different backends (e.g.,
  distributed key-value stores for higher throughput) without changing the Quota Enforcement gateway. P1 commitment to a
  single `toolkit-db`-based plugin keeps the initial scope focused.
- **Actors**: `cpt-cf-quota-enforcement-actor-platform-operator`, `cpt-cf-quota-enforcement-actor-storage-backend`

### 5.15 Notification Plugin Contract

#### Notification Plugin Contract & Event Catalog

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-notification-plugin`

The system **MUST** define a notification plugin contract that allows deployment-specific event sinks to receive
structured notification events. The plugin contract **MUST** be an in-process Rust trait in P1; P2 introduces a
standardized routing layer through the platform EventBus. The system **MUST** support multiple registered plugins per
deployment; events are dispatched to all registered plugins.

The system **MUST** emit events for the following catalog of event kinds:

- `threshold-crossed` — a Quota's actual `consumed` amount crossed at least one operator-configured threshold (e.g.,
  50%, 80%, 100% of cap) on an **upward transition**. The system tracks the highest threshold crossed so far per
  `(Quota, period)`; emission fires only when a successful counter mutation moves `consumed` strictly above that marker.
  **One** `threshold-crossed` event is emitted per such transition; its payload carries
  `crossed_thresholds: [<list of every threshold the mutation crossed, ascending>]` and `highest_crossed_threshold` (the
  maximum of that list). `Denied` outcomes and canonical-error responses do **NOT** emit `threshold-crossed` — counters
  were not modified, so no actual threshold transition occurred. The marker resets at period rollover so thresholds can
  fire again in the new period,
- `period-rollover` — a consumption-type Quota crossed a period boundary, with closing-period consumed amount,
  closing-period cap, and the new period boundary,
- `lease-auto-released` — a lease TTL expired without commit/release,
- `lease-resolved-by-deactivation` — a lease was atomically resolved when its Quota was deactivated (one event per
  affected lease; emitted from `cpt-cf-quota-enforcement-fr-quota-lifecycle` deactivation flow),
- `quota-changed` — a Quota was created, updated, or deactivated,
- `quota-counter-adjusted` — a credit was applied to a Quota's counter outside the natural debit/rollback flow (e.g.,
  redistribution between subjects, SLA-breach grant, manual adjustment by Quota Manager), carrying the credited amount,
  the target Quota, and the authenticated service principal. Fires only for `credit`
  (`cpt-cf-quota-enforcement-fr-credit`); a return or a rollback uses the `quota-rollback-applied` event below,
- `quota-rollback-applied` — a charge, in full or in part, was given back by a return or a rollback
  (`cpt-cf-quota-enforcement-fr-return`, `cpt-cf-quota-enforcement-fr-rollback`). One event kind serves both, with no
  field that tells them apart. Carries the charge's operation and key, the returned amount, what remains of the
  charge, the affected Quotas, and the actor: the caller on QE's own API, the platform identity of the resolver's
  caller on the resolver plugin path. Distinct from `quota-counter-adjusted`, which describes a credit,
- `policy-changed` — a Quota Resolution Policy was created, updated, or deleted.

Each event **MUST** carry `event_id`, `event_kind`, its scope (`tenant_id` for a tenant-scoped Quota, lease, or
consumption event; `policy-changed` is platform-scoped and carries none), `quota_id` or `policy_id` (whichever applies),
`subject` (when applicable), event-specific payload, and an emission timestamp. Event delivery is best-effort in phase 1
— sustained delivery failures **MUST** be reflected in operational telemetry but **MUST NOT** block Quota Enforcement
write operations. Delivery MAY duplicate under retry; sinks **MUST** tolerate duplicate delivery of the same
`event_id`.

- **Rationale**: Threshold-based notifications are a recurring requirement across consuming services — without a shared
  primitive every team reimplements them inconsistently. Centralizing the catalog and emission contract in QE eliminates
  that duplication. Plugin pluggability bridges the gap until the platform EventBus is available.
- **Actors**: `cpt-cf-quota-enforcement-actor-notification-sink`, `cpt-cf-quota-enforcement-actor-platform-operator`

### 5.16 Operational Telemetry

#### Gear-Specific Telemetry

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-telemetry`

The system **MUST** follow platform observability conventions for the baseline observability surface — HTTP request
counts and latencies, health endpoints, OpenTelemetry traces — provided by `toolkit-observability` and the `api-gateway`
framework. On top of that baseline, the system **MUST** expose the following gear-specific counters, histograms, and
gauges that surface QE-internal policy decisions and guard-rail rejections invisible to the framework baseline:

- **`denial_total`** — count of admission denials by closed reason kind.
- **`contention_rejected_total`** — operations rejected due to the acquisition contention timeout, by closed
  `operation` kind.
- **`lease_contention_rejected_total`** — debits and lease acquisitions rejected due to the acquisition contention
  timeout per `cpt-cf-quota-enforcement-fr-lease-acquire`, by canonical registered `metric`.
- **`lease_acquisition_wait_seconds`** — histogram of wait times during lease acquisition (both successful waits and
  rejected ones), by canonical registered `metric`.
- **`lease_inflight_limit_exceeded_total`** — acquisitions rejected by the per-`(tenant, metric)` active-lease cap per
  `cpt-cf-quota-enforcement-fr-lease-timeout`, by canonical registered `metric`.
- **`lease_unreclaimed_expired`** — count of expired-but-unreclaimed leases, by canonical registered `metric`.
- **`allocation_charges_past_replay_window`** — gauge of allocation charges that hold an amount on an active Quota and
  are older than the replay window (`cpt-cf-quota-enforcement-fr-return`), by canonical registered `metric`. A hold
  on a deactivated Quota **MAY** be counted until QE releases it.
- **`allocation_charge_max_age_seconds`** — gauge of the age of the oldest such charge, by canonical registered
  `metric`.
- **`engine_bootstrap_failures_total`** — Engine registration failures at gear bootstrap per
  `cpt-cf-quota-enforcement-fr-quota-resolution-engine`.
- **`engine_evaluation_seconds`** — Engine evaluation latency.
- **`policy_evaluation_failures_total`** — requests rejected with `POLICY_EVALUATION_FAILED`, by `engine_id` and
  canonical registered `metric`.
- **`policy_version_transitions_total`** — Policy-version create, update, rollback, and delete transitions, labelled by
  `transition_kind` from the closed set `{create, update, rollback, delete}`.
- **`policy_version_conflict_rejections_total`** — Policy writes rejected with `VERSION_CONFLICT`; no labels.
- **`debit_plan_invariant_violations_total`** — Decisions rejected for violating Debit-Plan invariants per
  `cpt-cf-quota-enforcement-fr-quota-resolution-engine`, labelled by invariant from the closed set
  `{quota_id_outside_applicable_set, negative_amount, chain_sum_mismatch, duplicate_quota,
  result_plan_inconsistency}`.
- **`quota_cap_zero_total`**, **`quota_cap_unbounded_total`** — gauges of the Quotas with lifecycle status `active`
  whose `cap = 0` and `cap = null` respectively, whatever their validity window, per
  `cpt-cf-quota-enforcement-fr-quota-lifecycle` cap value semantics (operator misconfiguration surfaces).

- **`quota_for_direct_metric_total`** — gauge of the Quotas with lifecycle status `active` whose metric has a served
  catalogue entry that classifies it `Direct` per the §3.2 inertness rule (surfaces operator misconfigurations where a
  Quota was declared on a non-gated metric); a deactivation takes effect at the next gauge refresh.
- **`quota_conditions_unfit_total`** — gauge of the Quotas with lifecycle status `active` whose conditions do not fit
  their metric type as it is now (`cpt-cf-quota-enforcement-fr-quota-metadata`); such a Quota may not apply to the
  requests its operator meant.
- **`contract_validation_failures_total`** — rejected metric objects by closed validation surface/reason; it also
  counts each metric type that makes its metric unavailable
  (`cpt-cf-quota-enforcement-fr-subject-type-registry`), and a log entry names that metric and the reason.
- **`admitted_metric_violations_total`** — subject types not admitted by the metric, by closed validation surface.
- **`catalog_read_failures_total`** — reads of `types-registry` that fail or reach their time limit, by `phase` from the
  closed set `{start, first_use, scheduled}`.
- **`catalog_last_scheduled_read_timestamp_seconds`** — gauge of the time of the last scheduled catalogue read that
  succeeded on the replica.
- **`catalog_unavailable_metrics`** — gauge of the number of unavailable catalogue entries that the replica holds now.
- **`notification_dispatch_failures_total`** — per-sink dispatch failures, by `sink_id` and `event_kind`.
- **`outbox_pending_rows`** — outbox backlog gauge, by `queue`.
- **`outbox_rejections_total`** — handler `Reject` outcomes, by `queue`; this is not a durable dead-letter row count.

Labels **MUST NOT** include high/unbounded-cardinality identifiers (`tenant_id`, `subject_id`, `quota_id`, `policy_id`,
`idempotency_key`, `lease_token`), subject type, caller attribution, or raw/unregistered metric input. Permitted
dimensions are deployment-bounded values — canonical registered `metric` only on instruments that declare it above,
`engine_id`, `operation`, `transition_kind`, validation `surface`, `invariant`, closed `reason` enums, `sink_id` (the
deployment-registered sink set), `event_kind` (the closed event catalog), and `queue` (the fixed outbox queue set). A
`metric` label **MUST** be populated only after registry/catalogue validation and **MUST** use the canonical registered
identity; unknown or unadmitted caller input **MUST NOT** appear as a label. Caller attribution is retained on sampled
traces/diagnostics, not metrics.

- **Rationale**: The framework baseline already covers HTTP-server-level observability and health endpoints uniformly
  across gears; restating it here would duplicate convention. The instruments above expose policy-decision and
  guard-rail signals unique to QE and invisible to the framework — they are the difference between "gateway is up" and
  "quota arbitration is healthy". Bounding label cardinality at the PRD level prevents a 100M-subject deployment from
  creating per-tenant time series that exhaust the metrics backend, while selected registered-metric labels remain
  bounded by the deployment catalogue and preserve operational localization.
- **Actors**: `cpt-cf-quota-enforcement-actor-monitoring-system`, `cpt-cf-quota-enforcement-actor-platform-operator`

### 5.17 Quota Resolver Plugin

#### Quota Resolver Backend Plugin

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-fr-resolver-plugin`

The system **MUST** implement the Quota Resolver backend plugin contract (`QuotaResolverPluginClient`) and register
itself as a Quota Resolver plugin instance, reachable in process only. The plugin **MUST** serve debit, lease
acquisition, commit, release, return, and quota state by running QE's own operations, with the same replay,
evaluation, and storage behaviour, and convert resolver requests to QE requests and QE results to resolver results.
A resolver request names each metric as a metric object with its amount (a **metric item**); the plugin gives the
metric object to QE unchanged. It **MUST**:

- run every operation in **trusted admission mode**: no PDP call, an access scope that names the request's tenant
  only, and policy management always refused; this mode is reachable only through the plugin client;
- run without a `SecurityContext`: the plugin receives none and builds none; the operation log records as the actor
  the platform identity of the resolver's caller, which the plugin request carries and QE does not authorize by;
- keep every other QE check: subject admission, the metric object, amounts, idempotency, and the tenant of a lease
  token;
- reject with `InvalidArgument` a request that carries more than one metric item, or a return that names a metric
  other than the charge's: a QE operation has one metric;
- leave the operator-only fields and the Quotas outside their validity window out of quota state, and return only
  the tenant's Quotas, without an error, for a metric that does not admit the subject's type
  (`cpt-cf-quota-enforcement-fr-quota-snapshot-read`).

The trust boundary is the process: any module in the same process can obtain the plugin client and is trusted like a
caller of the Quota Resolver's platform plane. One request never spans tenants; the caller names the tenant.

QE's own API keeps its PDP check: debit, lease, rollback, and snapshot through it still need a PDP grant. New
consumers use the Quota Resolver.

- **Rationale**: The Quota Resolver's plugin contract carries a fixed tenant and subjects and no `SecurityContext`,
  so a PDP check could not authorize the module that charges; one implementation per operation keeps both paths
  identical (ADR-0009).
- **Actors**: `cpt-cf-quota-enforcement-actor-quota-resolver`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Evaluation Latency

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-evaluation-latency`

Single-quota evaluation operations (debit, credit, rollback, return, reserve, commit, release, evaluate_preview, Quota
Snapshot read for one subject/metric pair) **MUST** complete within 100ms at p95 under target load.

- **Threshold**: p95 ≤ 100ms at 10 000 ops/sec sustained
- **Rationale**: Quota evaluation sits on the hot path of every consuming-service request; latency above this budget
  breaks consumer SLOs.
- **Architecture Allocation**: See DESIGN.md

#### Throughput

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-throughput`

The system **MUST** sustain at least 10 000 evaluation operations per second under normal operation across all tenants
combined.

- **Threshold**: ≥ 10 000 ops/sec sustained
- **Rationale**: High-volume services (LLM Gateway, API Gateway) generate significant per-request quota checks; the
  evaluation path must not become a bottleneck.
- **Architecture Allocation**: See DESIGN.md

#### Subject Scale

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-subject-scale`

The system **MUST** support at least 100 000 000 distinct subjects (all subject types combined) without
degrading evaluation latency below the `cpt-cf-quota-enforcement-nfr-evaluation-latency` threshold.

- **Threshold**: ≥ 100M subjects supported
- **Rationale**: CF/Gears are targeted at multi-tenant deployments with very large user populations; subject-scale
  must not be the limiting factor.
- **Architecture Allocation**: See DESIGN.md

#### Quota Density per Subject

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-quota-density`

The system **MUST** support at least 10 active Quotas per subject without degrading evaluation latency below the
`cpt-cf-quota-enforcement-nfr-evaluation-latency` threshold. Combined with subject scale, the system supports at least 1
billion total active Quotas.

- **Threshold**: ≥ 10 Quotas per subject; ≥ 1B total active Quotas
- **Rationale**: Each consuming service introduces its own quota dimensions; subjects in production routinely accumulate
  many distinct Quotas as the platform grows.
- **Architecture Allocation**: See DESIGN.md

#### High Availability

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-availability`

The system **MUST** maintain 99.95% monthly availability for evaluation endpoints. A request that fails because its
contention timeout passed (`LEASE_CONTENTION_TIMEOUT`, `cpt-cf-quota-enforcement-fr-lease-acquire`) counts as a failed
request against this target.

- **Threshold**: 99.95% uptime per calendar month
- **Rationale**: Quota evaluation is on the critical path for every guarded operation; downtime translates directly to
  consuming-service downtime.
- **Architecture Allocation**: See DESIGN.md

#### Authentication Required

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-authentication`

The system **MUST** require authentication for all API operations. Unauthenticated requests **MUST** be rejected before
any operation is performed. The resolver plugin path is not an API: it is reachable only in process, the Quota
Resolver authenticates its own callers, and the plugin receives no `SecurityContext`
(`cpt-cf-quota-enforcement-fr-resolver-plugin`).

- **Threshold**: Zero unauthenticated API access
- **Rationale**: Quota state is tenant-sensitive; unauthenticated access is a security violation.
- **Architecture Allocation**: See DESIGN.md

#### Authorization Enforcement

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-authorization`

The system **MUST** enforce authorization for every operation of QE's own API via the platform PDP, fail closed on PDP
unavailability or denial, and apply PDP-returned constraints as additional query filters before query execution. The
resolver plugin path is the only exception (`cpt-cf-quota-enforcement-fr-resolver-plugin`).

- **Threshold**: Zero unauthorized data access or write
- **Rationale**: Authorization prevents unauthorized counter mutation and cross-tenant data leakage.
- **Architecture Allocation**: See DESIGN.md

#### Tenant Isolation Integrity

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-tenant-isolation-integrity`

The system **MUST** ensure zero cross-tenant data leakage in any read or write operation; storage-layer enforcement and
PDP authorization **MUST** independently enforce tenant boundaries. On the resolver plugin path, which makes no PDP
call, storage-layer enforcement alone **MUST** confine every operation to the request's tenant.

- **Threshold**: Zero cross-tenant read or write events in soak testing and chaos testing
- **Rationale**: Tenant isolation breaches are an existential platform risk; defense in depth is required.
- **Architecture Allocation**: See DESIGN.md

#### Idempotency Guarantee

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-idempotency-guarantee`

The system **MUST** guarantee that every write operation with an idempotency key produces exactly one counter effect
regardless of the number of replays, under at-least-once delivery from upstream callers, and that a repeated rollback
gives nothing back twice.

- **Threshold**: Zero double-count events under simulated retry storms (10× normal RPS, 5% retry rate)
- **Rationale**: Idempotency is the single most important correctness property for a counter/ledger system; violations
  corrupt billing-relevant state.
- **Architecture Allocation**: See DESIGN.md

#### Storage Fault Tolerance & RPO

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-fault-tolerance`

The system **MUST** guarantee that every write operation that has returned `Allowed` or `Denied` to the caller is
durably persisted; no committed counter mutation may be lost due to storage backend restarts, gateway restarts, or
single-node failures.

- **Threshold**: RPO = 0 for committed operations (zero committed-data loss)
- **Rationale**: Quota counters are billing-relevant; lost mutations create reconciliation risk and tenant complaints.
- **Architecture Allocation**: See DESIGN.md

#### Recovery Time

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-nfr-recovery`

The system **MUST** resume normal evaluation operations within 15 minutes of storage backend recovery from an outage.

- **Threshold**: RTO ≤ 15 minutes from storage backend recovery
- **Rationale**: Bounded recovery time prevents extended platform-wide unavailability when storage outages occur.
- **Architecture Allocation**: See DESIGN.md

### 6.2 Data Governance

**Data Steward**: Platform Engineering team owns the Quota Enforcement schema and notification contracts. The data
steward is responsible for managing the storage plugin contract and curating the seeded global Quota Resolution
Policy.

**Data Classification**: Quota state contains numeric counters, metric names, timestamps, tenant-scoped opaque
identifiers (tenant_id, user_id, subject_id, quota_id, policy_id, idempotency_key, lease_token), operator-supplied
**Quota Metadata** (`cpt-cf-quota-enforcement-fr-quota-metadata`), and caller-supplied opaque \*\*request
metadata \*\* (forwarded into the Engine EvaluationContext per `cpt-cf-quota-enforcement-fr-quota-resolution-engine` and
persisted as part of operation log / idempotency state for replay determinism). All four classes — counters,
identifiers, Quota Metadata, request metadata — are classified as **Platform Operational Data**: internal,
business-sensitive, restricted to authenticated platform actors, no natural-person PII stored directly. Operators are
responsible for Quota Metadata content; **calling services are responsible for sanitizing request metadata before
forwarding it** — the system does not inspect or validate request-metadata values against PII rules, and any PII a
caller injects will land in idempotency records, operation logs, Engine diagnostics, and downstream telemetry per the
request-metadata's natural data flow. Calling services that handle regulated data (GDPR, HIPAA, etc.) MUST strip or hash
such fields before forwarding (e.g., pass `session_id_hash` rather than `session_id`).

**Data Ownership**:

| Data                                        | Owner                            | Custodian                                 |
| ------------------------------------------- | -------------------------------- | ----------------------------------------- |
| Quotas                                      | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |
| Counters and ledger rows                    | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |
| Leases                                      | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |
| Quota Resolution Policies                   | Platform Operator                | Quota Enforcement gear (storage plugin) |
| Charge ledger                               | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |
| Metric types                                | Metric owner                     | `types-registry` gear                   |
| Idempotency records                         | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |
| Notification dispatch records (best-effort) | Tenant identified by `tenant_id` | Quota Enforcement gear (storage plugin) |

**Retention**:

| Data class                                    | Retention policy                                                                                                                                                                                                                                     | Reclamation mechanism                                                           |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| Quotas and Quota Resolution Policies (active) | Retained until explicitly deactivated or deleted; the seeded global Policy cannot be deleted                                                                                                                                                         | Operator-initiated only                                                         |
| Quotas (deactivated)                          | Retained indefinitely for read access in P1; no automatic purge. P2 may introduce a configurable grace-period auto-purge tied to audit infrastructure (see §13)                                                                                      | P2 candidate for auto-purge                                                     |
| Counters for consumption-type Quotas          | Active period plus operator-configurable historical window (default: 13 months) sufficient for year-over-year reporting                                                                                                                              | Background sweeper, period-rollover-aware                                       |
| Leases (active and committed/released)        | Active leases live until commit, release, or TTL expiry (lazy expiry per `cpt-cf-quota-enforcement-fr-lease-timeout`). Resolved leases (committed, released, or auto-released) are retained as ledger entries for the operation-log retention window | Background sweeper                                                              |
| Leases (expired but unreclaimed rows)         | Physically reclaimed within 1 hour of expiry — see `cpt-cf-quota-enforcement-fr-lease-timeout`                                                                                                       | Background sweeper; correctness independent of sweeper liveness (lazy semantic) |
| Idempotency records                           | The replay window: operator-configurable per `(tenant, metric)` (default: 24 hours), sufficient to bound legitimate retry windows. The record of an allowed debit or lease commit is its charge | Background sweeper |
| Charge ledger                                 | Until the replay window has passed since the charge's creation and since the last return or rollback applied to it, and no allocation entry of it holds an amount on an active Quota; then the charge is deleted and unknown to every operation (`cpt-cf-quota-enforcement-fr-return`) | Background sweeper, in batches |
| Operation log                                 | Operator-configurable (default: 30 days)                                                                                                                                                                                                             | Background sweeper                                                              |
| Notification dispatch records (best-effort)   | Operator-configurable retention for delivery-failure diagnostics (default: 7 days); records older than the window are reclaimed                                                                                                                      | Background sweeper (shared with idempotency cleanup)                            |
| Audit-grade retention                         | Deferred to P2 when platform audit infrastructure is available                                                                                                                                                                                       | P2 component                                                                    |

### 6.3 NFR Exclusions

The following commonly applicable NFR categories are not applicable to this gear:

- **Safety (ISO/IEC 25010:2023 §4.2.9)**: Not applicable — Quota Enforcement is a server-side data API with no physical
  interaction, no safety-critical operations, and no ability to cause harm to people, property, or the environment.
- **Accessibility and Usability (UX)**: Not applicable — Quota Enforcement exposes no user-facing UI. It provides a
  developer SDK and a server-side API consumed exclusively by platform services.
- **Internationalization / Localization**: Not applicable — the gear exposes no user-facing text, labels, or
  locale-sensitive output.
- **Privacy by Design (GDPR Art. 25)**: Not applicable as a standalone gear requirement. Subject IDs stored by Quota
  Enforcement are opaque internal platform identifiers; PII management is the responsibility of the platform identity
  layer (e.g., `account-management`). Caller-supplied request metadata and operator-supplied Quota Metadata are
  Platform Operational Data under the caller obligations of §6.2 (no PII; callers strip or hash it before sending).
- **Regulatory Compliance (GDPR, HIPAA, PCI DSS, SOX)**: Not applicable as a standalone gear requirement — this is an
  internal platform infrastructure gear. Quota Enforcement handles no payment card data (PCI DSS N/A), no healthcare
  records (HIPAA N/A), and no financial reporting data (SOX N/A). Platform-level regulatory obligations are governed at
  the platform level.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Quota Enforcement SDK Trait

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-interface-sdk-client`

- **Type**: Programmatic SDK (language-native client interface)

- **Stability**: unstable (v0)

- **Description**: Public SDK interfaces cover S2S consumer evaluation and reads, Quota Manager operations, and
  operator-only Policy management. Consumer inputs carry explicit attribution (`tenant_id`, `subjects: [{type, id}]`)
  and a metric object, authorized by PDP. Metric type registration remains a `types-registry` operation. The gear also
  registers the Quota Resolver plugin (`cpt-cf-quota-enforcement-fr-resolver-plugin`). Exact interfaces are defined in
  DESIGN.md.

- **Breaking Change Policy**: Unstable during initial development; will stabilize in a future version (target: v1).

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-interface-rest-api`

- **Type**: HTTP/REST

- **Stability**: unstable (v0)

- **Description**: REST endpoints covering the same operations as the SDK interfaces, intended for cross-language
  integration and for external consumers that cannot link the Rust SDK directly.

- **Breaking Change Policy**: Unstable during initial development; will follow `/quota-enforcement/v1/...` versioning at
  stabilization.

Metric type registration adds **no QE REST endpoint**. Owners register metric types through `types-registry`; QE
reads them by id and validates the metric object of each request at ingress.

### 7.2 External Integration Contracts

#### Owner-Published Metric Type

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-contract-owner-projection`

- **Direction**: supplied by the metric owner through `types-registry`; consumed by QE and every Quota Consumer.
- **Protocol/Format**: one metric type per metric, derived from `gts.cf.core.quota.metric.v1~`, with the traits
  `kind`, `admitted_subjects`, and `enforcement` and a schema that refines the `metadata` of the metric object.
  Consumers send `tenant_id`, `subjects: [{type, id}]`, and one metric object `{type, id?, metadata}`.
- **Compatibility**: `types-registry` admits a change under the same type id only when the new schema accepts every
  instance that the previous one accepted; an optional-property addition is such a change. Another major version is
  another metric with its own Quotas.

#### Storage Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-contract-storage-plugin`

- **Direction**: required from plugin implementor

- **Protocol/Format**: Rust trait implemented by each storage backend plugin.

- **Compatibility**: Plugin contract versioned with the gear's major version; plugins must match the gear's major
  version.

#### Cluster Coordination Contract (consumed)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-contract-cluster-coordination`

- **Direction**: consumed from the platform `cluster` gear

- **Protocol/Format**: the cluster gear's leader-election primitive, resolved for the `quota-enforcement` cluster
  profile with a linearizable-election requirement. One named election per sweeper singleton (`lease-sweeper`,
  `retention-sweeper`) under the `qe` scope prefix. The operator binds the profile to a backend in the cluster section
  of the deployment YAML; Quota Enforcement code does not change with the backend.

- **Compatibility**: versioned by the cluster gear per primitive; Quota Enforcement tracks the cluster SDK major
  version. A capability mismatch or an unbound profile fails startup (embedded profile) or readiness (deployed
  profile) and never degrades silently. This consumed contract replaces the Quota-Enforcement-owned coordination
  plugin contract that the first version of the coordination ADR defined.

#### Notification Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-contract-notification-plugin`

- **Direction**: required from notification sink implementor

- **Protocol/Format**: Rust trait implemented by each notification sink plugin in P1; P2 will additionally route through
  the platform EventBus.

- **Compatibility**: Plugin contract versioned with the gear's major version; backwards-compatible additive changes
  are allowed within a major version.

#### Subject Manager Lifecycle Contract — Indirect via Quota Manager

- [ ] `p2` - **ID**: `cpt-cf-quota-enforcement-contract-subject-manager`

- **Direction**: indirect — Subject Managers (e.g., `account-management`) signal subject lifecycle events to Quota
  Manager (a separate platform component); Quota Manager consumes those events upstream and reaches Quota Enforcement
  only through QM-issued CRUD on Quota records (see `cpt-cf-quota-enforcement-fr-quota-lifecycle`). Quota Enforcement
  defines no direct Subject Manager-facing API surface in P1; this entry exists to document the indirection rather than
  a QE-side endpoint.

- **Protocol/Format**: not a QE-owned protocol — QE observes Subject Manager intent solely through Quota Manager-driven
  Quota CRUD. A direct contract (REST or gRPC) MAY be added in a later major release if a use case justifies bypassing
  Quota Manager; it would land here as additive without breaking P1 callers.

- **Compatibility**: P1 exposes no Subject Manager-facing surface to version. Future direct contract would follow
  backwards-compatible additive changes within a major version.

#### Quota Resolution Engine Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-contract-quota-resolution-engine-plugin`

- **Direction**: required from Engine implementor; P1 built-ins (`most-restrictive-wins`, `cel`) ship with the gear.
  Additional Engines are linked into the gear binary at build time.

- **Protocol/Format**: Rust trait (`QuotaResolutionEngineV1`) with config-validation and evaluate entry points;
  `engine_config` is opaque to the Quota Enforcement core and validated by the Engine's own validator.
  Decision shape is `{ result, debit_plan: [[{quota_id, amount}, ...], ...], diagnostics }` with the Debit-Plan
  invariants enforced at the QE core boundary (see `cpt-cf-quota-enforcement-fr-quota-resolution-engine`).

- **Compatibility**: Plugin contract versioned with the gear's major version; backwards-compatible additive changes (
  new optional Debit Plan entry fields, new `EvaluationContext` bindings) are allowed within a major version. Removing
  or changing the meaning of an existing field is a major-version break.

## 8. Use Cases

#### Create a Quota

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-create-quota`

**Actor**: `cpt-cf-quota-enforcement-actor-platform-operator` or `cpt-cf-quota-enforcement-actor-quota-manager` (acting
on behalf of a tenant administrator)

**Preconditions**:

- Caller is authenticated with the appropriate scope (operator-level for cross-tenant operations; Quota Manager
  forwarding a tenant administrator's SecurityContext for own-tenant operations)
- Target metric is registered in `types-registry`
- The subject type is registered, derived from `gts.cf.core.quota.subj.v1~`, and admitted by the metric type

**Main Flow**:

1. Caller submits a Quota payload (subject reference, metric, type, period if applicable, `enforcement_mode`, cap,
   `source` (defaults to `licensing` when omitted; `operator` for manual caps), optional notification thresholds,
   optional validity window, optional failure-mode hint, optional metadata)
1. System validates the payload (the metric type is registered and served; type/period combinatorics are valid; the
   subject type is registered and admitted by the metric; Quota metadata is within the size limit and its conditions
   fit the metric type's `metadata` schema).
1. System persists the Quota with a server-assigned quota ID and emits a `quota-changed` notification event

**Postconditions**:

- The Quota is immediately effective and joins any other applicable Quotas; arbitration is resolved by the active Quota
  Resolution Policy (default `most-restrictive-wins`).

**Alternative Flows**:

- **Unknown metric**: System rejects with actionable error
- **Subject type not admitted by the metric**: System rejects with `InvalidArgument`
- **`types-registry` unreachable**: System rejects with actionable error
- **`type=rate`**: System rejects with `NOT_YET_IMPLEMENTED` error in P1
- **Quota metadata does not fit the metric type**: System writes nothing and rejects with `InvalidArgument`
  (`QUOTA_METADATA_REJECTED`) and the list of what is wrong
- **Metric with an unavailable catalogue entry**: System rejects with `PROJECTION_NOT_RESOLVABLE`

#### Configure a Quota Resolution Policy

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-configure-policy`

**Actor**: `cpt-cf-quota-enforcement-actor-platform-operator`

**Preconditions**:

- Operator is authenticated with operator-level SecurityContext
- The default global Policy (most-restrictive-wins) is already active

**Main Flow**:

1. Operator submits a Policy payload (scope: `metric=<name>`; `engine_id`: e.g., `cel`; `engine_config`: engine-specific
   structure such as a CEL expression; optional per-Policy timeout)
1. System resolves `engine_id` against the registered Engines (rejects unknown engine_id with actionable error) and
   delegates `engine_config` validation to the named Engine's validator (e.g., the `cel` Engine performs CEL parse +
   type-check; parse/type errors are returned with line/column)
1. System persists the Policy with a server-assigned policy ID and emits a `policy-changed` event
1. Subsequent evaluations matching the Policy's scope use the new Policy via most-specific-scope precedence

**Postconditions**:

- The Policy is immediately effective for matching evaluations
- The default global Policy remains in place for non-matching evaluations
- Existing Quotas are not checked against the new Policy; the operator makes sure that their metadata fits it
  (`cpt-cf-quota-enforcement-fr-quota-resolution-policy`)

**Alternative Flows**:

- **Unknown `engine_id`**: System rejects with `UNKNOWN_ENGINE` error naming the registered Engines available in this
  deployment
- **Engine config validation error** (e.g., CEL parse error): System rejects with actionable error from the Engine's
  validator (e.g., line/column for CEL)

#### Author a Quota Cascade Policy via CEL

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-cascade-via-cel`

**Actor**: `cpt-cf-quota-enforcement-actor-platform-operator`

**Preconditions**:

- A tenant `T` has both `(user subject type, U)` and `(tenant subject type, T)` Quotas for metric `M`
  (e.g., `ai-tokens-input`); operator wants
  per-user pool consumed first, falling through to the tenant pool when user-pool is exhausted
- The `cel` Engine is registered in this deployment

**Main Flow**:

1. Operator submits a Policy payload with `scope=metric=M`, `engine_id=cel`, and `engine_config.expr` containing a CEL
   expression that emits a one-chain `debit_plan` based on per-pool remaining capacity (route to the user Quota
   first; fall through to the tenant Quota for the residual; deny when both pools combined are insufficient — see
   `cpt-cf-quota-enforcement-fr-quota-cascade` for the worked example)
1. System validates the CEL expression and persists the Policy
1. A subsequent debit `debit(metric=M, amount=A, …)` for a user `U` in tenant `T` resolves applicable-Quotas →
   `{user_q, tenant_q}` → invokes the `cel` Engine, which emits a Debit Plan that draws from `user_q` first
1. Quota Enforcement validates the Debit Plan against the standard invariants and atomically applies it; counter
   mutations are limited to the Quotas named in the plan

**Postconditions**:

- Operations split across `user_q` and `tenant_q` as authored by the `cel` expression: `user_q` contributes up to its
  remaining capacity, `tenant_q` covers the residual; deny when `user_q.remaining + tenant_q.remaining < amount`
- The **multi-entry split** is the defining capability over the default `most-restrictive-wins` Engine. When
  `user_q.remaining > 0` but `< amount`, this `cel` Policy emits one chain of two entries (`[[{user_q,
  user_q.remaining}, {tenant_q, amount − user_q.remaining}]]`); `most-restrictive-wins` would instead fall through to
  a single-entry plan on `tenant_q` (consuming the full `amount`) and leave `user_q.remaining` unused. When
  `user_q.remaining = 0`, both Engines route everything to `tenant_q` and produce the same plan

**Alternative Flows**:

- **Both pools insufficient**: Engine returns `Denied(violated_quota_ids=[user_q.id, tenant_q.id])`; counters are not
  modified
- **Engine produces malformed plan** (e.g., references an unknown quota_id, a chain whose amounts do not add up to
  `request.amount`, negative `amount`): system enforces Debit-Plan invariants and surfaces a canonical `Internal`
  error (fail-closed) with no counter mutation; the operator is alerted via telemetry

#### Author a Region-Gated Quota via Metadata

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-region-gated-via-metadata`

**Actor**: `cpt-cf-quota-enforcement-actor-platform-operator`

**Preconditions**:

- Tenant T has user U; metric `gpu-hours` is registered in `types-registry`
- Operator wants per-region caps for U: 100 in `us-east-1`, 50 in `eu-west-1`
- The metric type of `gpu-hours` declares `metadata.region` as a string, and calling services pass `region` on every
  `debit` invocation

**Main Flow**:

1. Operator creates two Quotas via `cpt-cf-quota-enforcement-usecase-create-quota`; QE checks their conditions against
   the metric type:
   - `Q_us`: `subject=(user subject type, U)`, `metric=gpu-hours`, cap=100, hard,
     `metadata={region: ["us-east-1"]}`
   - `Q_eu`: `subject=(user subject type, U)`, `metric=gpu-hours`, cap=50, hard,
     `metadata={region: ["eu-west-1"]}`
1. Calling service makes the request `debit(metric={type: gpu-hours, metadata: {region: "us-east-1"}}, amount=20)`
1. Quota Enforcement resolves `applicable_quotas = {Q_us}`, since the condition of `Q_eu` does not hold, invokes the
   Engine of the active Policy, and applies Debit Plan `[[{ Q_us.id, amount: 20 }]]`
1. `Q_us.consumed` increases by 20; `Q_eu.consumed` is **not modified**

**Postconditions**:

- The user's regional usage is tracked independently per-region; the per-region cap is enforced
- A subsequent request with `region=eu-west-1` is debited against `Q_eu`

**Alternative Flows**:

- **Request metadata `region` matches no Quota** (e.g., `region=ap-south-1`): no Quota applies, and the request is
  `Denied` with `NO_APPLICABLE_QUOTA`, unless the operator declared a fallback Quota without a `region` condition
- **Metadata exceeds 4 KB byte-size limit** at create or update (per `cpt-cf-quota-enforcement-fr-quota-metadata`):
  rejected with an actionable validation error
- **Condition on a property the metric type does not declare** (e.g., `metadata={regions: [...]}`): rejected with
  `QUOTA_METADATA_REJECTED`

#### Debit a Quota for an Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-debit`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Preconditions**:

- Quota Consumer is authenticated as a service principal and PDP authorizes its supplied tenant/subject/metric tuple
- At least one Quota is active for one or more applicable subjects and the requested metric. If no Quota applies for a
  `QuotaGated` metric, Quota Enforcement returns `Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` per
  §3.2 — operators must provision Quotas (typically via subject-manager hooks at tenant/user creation) before
  consumption is permitted.

**Main Flow**:

1. Consumer calls `debit(tenant_id, subjects, metric, amount, idempotency_key)`, where `metric` is a metric object
   `{type, id?, metadata}`
1. QE rejects malformed public request shape; PDP authorizes the complete structurally valid tuple
1. QE checks that the metric admits each subject type and validates the metric object against the metric type
1. Quota Enforcement fetches all applicable Quotas for the operation's metric
1. Quota Enforcement selects the active Quota Resolution Policy by most-specific-scope precedence, then invokes that
   Policy's Engine (e.g., `most-restrictive-wins` or `cel`) against the applicable-Quotas set; the Engine returns a
   Decision with `result`, `debit_plan`, and `diagnostics`
1. Quota Enforcement validates the Decision against the Debit-Plan invariants (
   `cpt-cf-quota-enforcement-fr-quota-resolution-engine`) and, on `Allowed`, atomically applies the `debit_plan`: each
   Quota named in the plan is mutated by `entry.amount`
1. Quota Enforcement returns the Decision to the caller; consumer applies it (e.g., proceeds with the guarded operation
   on `Allowed`, returns 429 on `Denied`)

**Postconditions**:

- Counter state reflects the `debit_plan` on `Allowed` (Quotas NOT named in `debit_plan` are not touched, even if they
  appeared in the applicable-Quotas set)
- Counter state is unchanged for `Denied` outcomes and for any canonical-error response
- `threshold-crossed` notification events are emitted for each Quota whose post-mutation usage crossed an upward
  threshold configured in `notification_thresholds`

**Alternative Flows**:

- **Replay (same idempotency key, same payload)**: returns the original decision; counters are not modified again
- **Replay (same idempotency key, divergent payload)**: rejected with `IDEMPOTENCY_PAYLOAD_MISMATCH`
- **Engine returns malformed `debit_plan`** (unknown quota_id, a chain whose amounts do not add up to
  `request.amount`, negative amount): system rejects with a canonical `Internal` error (fail-closed) and emits
  engine-invariant-violation telemetry
- **Engine timeout (per-Policy default 5ms)**: surfaces a canonical `DeadlineExceeded` error (fail-closed); no counters
  mutated
- **PDP unreachable mid-evaluate**: surfaces a canonical `ServiceUnavailable` error (fail-closed)

#### Reserve, Then Commit (or Release) for a Long-Running Operation

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-reserve-and-commit`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Preconditions**:

- Same as Debit use case; a worst-case capacity estimate is available for lease

**Main Flow**:

1. Consumer calls `reserve(metric, amount, ttl, idempotency_key)` with the worst-case estimate; `ttl` **MUST** lie
   within the operator-configurable `[min_lease_ttl, max_lease_ttl]` window per `cpt-cf-quota-enforcement-fr-lease-acquire`
1. Quota Enforcement evaluates the lease against applicable Quotas under the active Quota Resolution Policy; returns
   `lease_token` and its expiry on success or denial otherwise
1. Consumer performs the long-running operation (e.g., 30-minute compute job)
1. Consumer calls `commit(lease_token, actual_amount, idempotency_key)` with the realized usage
1. Quota Enforcement converts the lease into a debit of `actual_amount`; the unused part (reserved minus actual) is
   unwound from the lease's holds

**Postconditions**:

- Counter state reflects the actual consumed amount
- The lease is marked as `committed`

**Alternative Flows**:

- **Operation aborted**: Consumer calls `release(lease_token, idempotency_key)`; held capacity is fully returned
- **Lease TTL expired before commit**: System auto-releases the lease, emits `lease-auto-released`; subsequent `commit`
  returns `LEASE_NOT_ACTIVE`
- **Actual amount exceeds reserved**: rejected with `OVER_COMMIT_NOT_AUTHORIZED`

#### End User Reads Own Quota State Through the Quota Resolver

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-end-user-quota-snapshot-read`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-resolver`

**Preconditions**:

- An end user is authenticated
- The Quota Resolver maps the user's principal type to a quota subject type

**Main Flow**:

1. End user requests their quota state through the Quota Resolver's tenant plane
1. The Quota Resolver calls the resolver plugin with the user's tenant and subject
1. The plugin runs QE's snapshot read in trusted admission mode, bound to that tenant
1. Quota Enforcement fetches all active Quotas for the subject and the implicit tenant subject
1. The plugin returns per-Quota state for every applicable Quota per
   `cpt-cf-quota-enforcement-fr-end-user-quota-snapshot-read`, without the operator-only fields and without Policy
   attribution

**Postconditions**:

- End user receives a complete view of their applicable Quotas, without administrator intervention

**Alternative Flows**:

- **No applicable Quotas**: Quota Enforcement returns an empty list (not an error)
- **The metric does not admit the subject's type**: the plugin returns only the tenant's Quotas (not an error)
- **A product backend reads through QE's S2S snapshot instead**: PDP authorizes its explicit target; an unauthorized
  tenant or subject returns `PermissionDenied` and QE performs no storage read.

#### Bulk Debit Across Multiple Quotas

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-usecase-batch-debit`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Preconditions**:

- Caller has multiple debit items to apply for a single logical operation that requires all metrics to be admitted
  together (e.g., LLM call needing input tokens, output tokens, and compute seconds)

**Main Flow** (`mode = atomic`, P1):

1. Consumer calls `batch_debit(items[], envelope_idempotency_key, mode = "atomic")`
1. Quota Enforcement evaluates each item against its applicable-Quotas set under the active Quota Resolution Policy;
   each item's evaluation observes counter state reflecting every previously-evaluated item in the same batch
1. If every item evaluates to `Allowed`, Quota Enforcement applies the union of all per-item `debit_plan`s and returns a
   batch-level `Allowed` outcome with a per-item array preserving submission order
1. If any item evaluates to `Denied` or fails with a canonical error, Quota Enforcement applies no counter mutations and
   returns the batch-level outcome (`Denied` in the body, or the canonical error envelope) with per-item statuses for
   diagnostics

**Postconditions**:

- Atomic batch: either all counters in the union of `debit_plan`s are mutated, or none are
- Replay with the same envelope key returns the original outcome

**Alternative Flows**:

- **Batch exceeds maximum size**: rejected with `BULK_TOO_LARGE` error before any item is evaluated
- **`mode = independent`**: rejected with `NOT_YET_IMPLEMENTED` in P1; deferred to P2 for bulk-independent operations
- **Batch-level timeout (atomic)**: the entire batch surfaces a canonical `DeadlineExceeded` error
  (`reason = "BATCH_TIMEOUT"`) with no counter mutations; caller retries with the same envelope key or a smaller batch

## 9. Acceptance Criteria

- [ ] Quotas can be created, updated, deactivated, and read; deactivated Quotas stop accepting new debits but remain
  readable
- [ ] Quota creation accepts multiple Quotas for the same `(subject, metric)` pair; all such Quotas enter the
  applicable-Quotas set at evaluation time and are resolved by multi-quota evaluation under the active Policy
- [ ] QE defines and registers no GTS type for subjects or metrics, and registers only its own quota type, period
  type, enforcement mode type, and source type with their instances; it uses the Quota Resolver's subject base,
  tenant subject type, and metric base, reads a metric type from `types-registry` by its id, and serves a
  metric only when its type is a valid metric type; a registration by another module does not change how a metric is
  served
- [ ] A metric type registered after QE started is served on its first request without a restart, or, when QE
  answered for that id before the registration, on the first request after QE's memory of unknown ids expires; a metric type that
  is not registered returns `InvalidArgument`; a wrong metric type makes only its metric unavailable
  (`FailedPrecondition`, no counter mutation), does not fail bootstrap, and is reported in a log entry and telemetry
- [ ] A change or a deletion of a registered metric type reaches every replica within one read interval; credit,
  commit, release, return, and rollback run while a metric is unavailable
- [ ] A request whose metric object does not fit the schema of its metric type returns `InvalidArgument`; the `id` of
  the metric object reaches the PDP and does not change which counter is charged
- [ ] A consumer operation supplying `tenant_id=T` and subject `{type=<user subject type>, id=U}` reaches the Quotas
  of `(user subject type, U)` and of the implicit `(tenant subject type, T)`; tenant Quotas constrain every subject in
  the tenant and remain shared across callers
- [ ] On QE's own API, PDP authorizes the complete caller-supplied attribution (the tenant, the subjects, and the
  metric with its type and optional id) against the authenticated service principal before subject admission or
  evaluation
- [ ] Ingress error precedence is stable: malformed public request shape returns `InvalidArgument` before PDP;
  structurally valid unauthorized attribution returns `PermissionDenied` before admission or a read of the metric
  type; authorized unregistered or not admitted subject types and invalid metric objects return `InvalidArgument`
- [ ] Every subject-based consumer evaluation request sends a metric object with required `metadata`, at least one
  subject, no two subjects of one type, a tenant subject only for the request's tenant, and only admitted subject
  types (a type derived from an admitted type is admitted); violations return canonical `InvalidArgument`, never
  `Decision::Denied`
- [ ] Two different services that debit one metric and name the same subject types resolve the same applicable-Quota
  key
- [ ] Attribution and authenticated principal data are excluded from Policy input; PDP and `token_scopes` (the token scopes of the platform `SecurityContext`) remain the
  permission boundary
- [ ] Allocation Quotas track in-flight capacity without periodic reset; consumption Quotas track per-period consumption
  with reset at calendar-aligned UTC boundaries
- [ ] `type=rate` Quota creation requests are rejected with `NOT_YET_IMPLEMENTED` in P1 per
  `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection`
- [ ] Period rollover resets the consumed counter to zero atomically with respect to in-flight operations and emits a
  `period-rollover` notification event with closing-period consumed amount, closing-period cap, and new period boundary
- [ ] `period-rollover` event for `P` MAY be delayed up to `max_lease_ttl` past the calendar boundary (default 1 hour)
  because P-attributed leases may settle cross-period; the event signals **settlement completion**, not the calendar
  transition itself; consumers needing a calendar-time signal (dashboards, threshold-marker resets, utilization-roll-up
  alerts) compute it from `period_boundary` arithmetic without subscribing to any event
- [ ] Credit closure is calendar-keyed: `credit` to period `P` rejected with `PERIOD_CLOSED` from `time >= period_end`
  onwards, regardless of whether `period-rollover` has been emitted for `P` yet
- [ ] A rollback, and a return inside the return window, decreases the counter of period `P` until the
  `period-rollover` event has been emitted for `P`, the settlement window between `period_end` and event emission
  included; after it the counter of `P` does not
  change
- [ ] Debit, credit, return, reserve, commit, and release are idempotent under retry; replay with same key + same
  payload returns the original outcome; replay with divergent payload is rejected; a rollback carries no key of its
  own, and a repeated rollback gives nothing back twice
- [ ] Credit takes an explicit `quota_id` and mutates only the named Quota's counter (no subject resolution, no Engine
  invocation); cross-tenant `quota_id`, unknown `quota_id`, or `quota_id` of a deactivated Quota are rejected with
  actionable error before any mutation; every successful credit emits one `quota-counter-adjusted` event with the
  credited amount, target `quota_id`, and authenticated service principal
- [ ] Lease auto-releases at TTL expiry; subsequent commit/release returns `LEASE_NOT_ACTIVE`; auto-release emits
  `lease-auto-released`
- [ ] Lease commit with `actual_amount ≤ reserved_amount` is allowed and unwinds the unused part from the holds, last
  entry of each chain first; `actual_amount > reserved_amount` is rejected with `OVER_COMMIT_NOT_AUTHORIZED`
- [ ] The seeded global Quota Resolution Policy uses `engine_id=most-restrictive-wins` with empty config; cannot be
  deleted but may be replaced; under it, multi-quota evaluation denies only when no Quota is satisfiable, identifying
  every violating Quota; P1 admission outcomes are `Allowed` / `Denied` (Decision body) or a platform-canonical error
- [ ] Quota Resolution Policies can be created with scope `metric` (in addition to the seeded `global` Policy);
  most-specific-scope precedence (`metric` > `global`) selects the active Policy at evaluation time; Policies
  referencing an unknown `engine_id` are rejected at create/update time; narrower scopes than `metric` are not reachable
  through the P1 API and are tracked as an Open Question
- [ ] Quota Resolution Engine plugin contract is implemented; P1 ships `most-restrictive-wins` (hardcoded) and `cel`
  (sandboxed); both are registered automatically at gear bootstrap
- [ ] Engine config validation is delegated to the named Engine at Policy create/update; the `cel` Engine performs parse
  \+ type-check at this stage and rejects syntactic and type errors with line/column; the `most-restrictive-wins` Engine
  rejects any non-empty config
- [ ] Quota Resolution Policy versioning: `create_policy` initializes `policy_version = 1` with
  `version_state = active`; subsequent `update_policy(if_match_version=N)` calls produce monotonically increasing
  versions, transition the previous active version to `superseded`, and atomically move the latest-pointer;
  `update_policy` with mismatched `if_match_version` is rejected with `VERSION_CONFLICT`;
  `rollback_policy(target_version)` makes the target active and transitions the previous active to `rolled_back`
  (terminal); rollback to a `rolled_back` or non-existent version is rejected with actionable error;
  `delete_policy(policy_id)` soft-deletes the Policy — active version transitions to `deleted` (terminal),
  latest-pointer cleared, evaluation falls through to the next-most-specific scope, `policy-changed` event emitted with
  `change_kind = "deleted"`; `delete_policy` against the seeded global Policy is rejected with
  `CANNOT_DELETE_SEEDED_GLOBAL_POLICY`; the seeded built-in global Policy materializes at bootstrap as
  `policy_version = 1`
- [ ] Another major version of a metric type is another metric with its own Quotas and counters; Quota and Policy
  creates and changes for a metric with no served catalogue entry fail, while a Quota deactivation, a Policy
  rollback, and a Policy delete run; a metric type that stops admitting the subject type of a stored Quota, or that
  an active Policy does not fit, stays served; a metric type of major version 0 is unavailable
- [ ] Consumer DTOs contain no `caller_type`, that is, no field that names the kind of the caller; the authenticated service principal is taken from `SecurityContext`, and
  the only operation attributes in the Policy input are `request`
- [ ] Decision diagnostics on every evaluation include `engine_id`, `policy_id`, and `policy_version` of the Policy that
  produced the Decision; idempotency replay returns the Decision with the original `engine_id` and `policy_version`
  verbatim, even after the Policy has been updated to a higher version
- [ ] `superseded`, `rolled_back`, and `deleted` Policy versions are retained for the operator-configured retention
  window (default 90 days), then hard-deleted by the storage retention sweeper; `active` versions are never auto-deleted
- [ ] Decision shape carries `result ∈ {Allowed, Denied}`, `debit_plan` (a list of chains of `{quota_id, amount}`
  entries), and `diagnostics`; counters are mutated strictly per `debit_plan` when result is `Allowed`, and not
  mutated for `Denied` or for any canonical-error response
- [ ] Debit-Plan invariants are enforced at the Engine boundary: quota_ids ⊆ applicable-Quotas set; per-entry
  `amount ≥ 0` (integer); every chain adds up to exactly `request.amount`; a Quota at most once in the plan;
  result/plan consistency (`Allowed` ⇒ non-empty plan; `Denied` ⇒ empty plan). Any violation surfaces a canonical
  `Internal` error with no counter mutation; the `debit_plan_invariant_violations_total` counter records:
  `quota_id_outside_applicable_set`, `negative_amount`, `chain_sum_mismatch`, `duplicate_quota`,
  `result_plan_inconsistency`
- [ ] Unwinding a cascade, an AND, and a mix of both gives back exactly the requested amount from every chain, last
  entry first
- [ ] `debit`, `credit`, `return`, `reserve`, and per-item `batch_debit` requests with `amount ≤ 0` are rejected with
  an actionable `INVALID_AMOUNT` error **before** idempotency lookup or any other pipeline step; no idempotency record
  is persisted, no operation log entry is written, no counter / lease row / hold is created. For `batch_debit`, the
  envelope is rejected naming the offending item index when any item carries `amount ≤ 0`
- [ ] `most-restrictive-wins` Engine produces a Debit Plan of **one chain with one entry** — the binding Quota — at
  `amount = request.amount`. Binding-Quota selection ranks satisfiable Quotas (remaining ≥ `request.amount`; unbounded
  trivially satisfiable) by three priority rules in order: (1) the `Subject` tier before the `Tenant` tier, for any
  subject types; (2) within the chosen tier, bounded > unbounded (operator's explicit cap takes precedence; unbounded
  acts as overflow when no bounded satisfiable in the tier); (3) within bounded satisfiable Quotas of the chosen tier,
  smallest remaining wins, ties broken by ascending `quota_id`. `Denied` is returned only when no Quota is satisfiable
  (every applicable bounded Quota has remaining capacity below `request.amount` and no unbounded Quota exists);
  `violated_quota_ids` then enumerates every such bounded Quota
- [ ] Cascade scenario (`cpt-cf-quota-enforcement-fr-quota-cascade`) verified end-to-end: with user-pool empty and
  tenant-pool available, an operator-authored Policy that routes user→tenant produces `Allowed` with `debit_plan`
  covering only `tenant_q`; user counter is not mutated; with both pools insufficient, `Denied` is returned naming both
  Quotas; with malformed cascade plan, system surfaces a canonical `Internal` error
- [ ] Quota Metadata (`cpt-cf-quota-enforcement-fr-quota-metadata`) is checked by QE core at create and at a change of
  the metadata, against the 4 KB bound and the metric type's `metadata` schema; a property the metric type does not
  declare, a non-scalar property, or a value of another type writes nothing and returns `InvalidArgument`
  (`QUOTA_METADATA_REJECTED`) with the list of what is wrong; stored metadata is not checked again
- [ ] A Quota applies to a request only when all its conditions hold; a condition on a property the request lacks
  does not hold; the system applies the conditions before the Engine runs
- [ ] Metadata changes emit `quota-changed` events without invalidating the quota_id and take effect on the next
  evaluation
- [ ] Region-gated scenario (`cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`) verified end-to-end under
  each built-in Engine: with `Q_us(metadata.region=[us-east-1])` and `Q_eu(metadata.region=[eu-west-1])`, a `debit`
  whose metric object carries `metadata.region=us-east-1` debits only `Q_us`; `Q_eu` is not mutated; a request with
  `region=ap-south-1` is `Denied` with `NO_APPLICABLE_QUOTA`
- [ ] CEL Policy create/update statically validates references to `request` properties against the schema of the
  metric type, with line/column diagnostics
- [ ] Decision is response-only: request DTOs do not carry Decision-shaped fields by type design; any Decision-shaped
  field (`debit_plan`, `result`, `diagnostics`) appearing in a request body is silently ignored by the server,
  regardless of operation kind (debit, credit, rollback, return, reserve, commit, release, batch_debit)
- [ ] Idempotency replay returns the original Decision (including `debit_plan`); client-submitted Decision-shaped fields
  in a replay payload are silently ignored per the trust-boundary discipline (§3.4)
- [ ] Lease rows are physically reclaimed by the sweeper within 1 hour of expiry; telemetry surfaces unreclaimed-expired-lease count by canonical registered `metric`; sweeper outage does
  not affect correctness (lazy expiry semantic)
- [ ] Per-`(tenant, metric)` active-lease cap (default: 1000) is enforced at acquisition time; requests exceeding the
  cap are rejected with `LEASE_INFLIGHT_LIMIT_EXCEEDED`; expired leases do not count toward the cap
- [ ] Lease TTL is required on every acquire request and **MUST** fall within the operator-configurable
  `[min_lease_ttl, max_lease_ttl]` window (platform defaults: 1 s / 1 h); a missing `ttl` or a value outside the window is rejected
  with `TTL_OUT_OF_BOUNDS` before idempotency lookup, multi-quota evaluation, or any hold acquisition — no idempotency
  record, no lease row, no capacity hold, and no change to the per-`(tenant, metric)` active-lease counter; clamping is
  not performed
- [ ] Multi-Quota lease acquisition is atomic: when the Engine's Debit Plan names multiple Quotas, holds are placed on
  every named Quota or none; partial holds are never observable, including under concurrent contention. Concurrent
  multi-Quota lease traffic does not deadlock the acquisition path
- [ ] Acquisition contention timeout (operator-configurable, default: 50 ms; 0 ms means no wait) bounds wait time on row
  contention; exceeded timeouts produce `LEASE_CONTENTION_TIMEOUT` rejection with no holds;
  `lease_contention_rejected_total` and `lease_acquisition_wait_seconds` metrics are populated by canonical registered
  `metric` and let operators distinguish and localize contention-driven denials from cap-exceeded, not-active, and
  Engine-Denied rejections
- [ ] Quota deactivation while leases are outstanding: active leases are marked `resolved-by-deactivation` atomically
  with the deactivation transaction; subsequent `commit` / `release` against those leases return `LEASE_NOT_ACTIVE`; a
  `lease-resolved-by-deactivation` notification event is emitted per affected lease
- [ ] Cap reduction below current consumed amount is rejected with `CAP_BELOW_CONSUMED`; the comparison is evaluated at
  update transaction commit time (not request-receipt) to avoid TOCTOU races with concurrent debits; cap raises and
  equal-or-greater updates are unaffected
- [ ] Period rollover ordering: a debit committed at exactly `boundary_at` is accounted to period `P+1`;
  `period-rollover` event for period `P` is emitted strictly after the last commit attributed to `P`
- [ ] Idempotency replay returns the original Decision verbatim from server-side state; the original `time` binding is
  captured but NOT re-bound on replay (verified end-to-end with a time-dependent CEL Policy: original evaluation at 02:
  30 returns Denied; replay at 04:00 still returns Denied because the stored Decision is the source of truth)
- [ ] Batch-debit batch-level evaluation timeout supersedes per-Policy timeouts; the timeout is a single
  operator-configurable flat duration (deployment-default 250 ms) per `cpt-cf-quota-enforcement-fr-batch-debit` (§5.7);
  on timeout, atomic-mode batch surfaces a canonical `DeadlineExceeded` error (`reason = "BATCH_TIMEOUT"`) with no
  counter mutations; in independent mode (P2), pending and not-yet-completed items are reported with `BATCH_TIMEOUT`
  while already-completed items retain their `Decision` outcomes
- [ ] Atomic batch-debit per-item evaluation observes running batch state: each item's Engine call sees counter state
  reflecting every previously-evaluated item in the same batch. Verified end-to-end — `Quota Q (cap=800, consumed=0)`;
  `batch_debit(mode=atomic, items=[{metric=M, amount=500}, {metric=M, amount=500}])` returns batch-level `Denied`
  (item-1 evaluated against `remaining=800` returns `Allowed`; item-2 evaluates against `remaining=300` and returns
  `Denied`; batch rolls back, no mutations persisted)
- [ ] Engine bootstrap is fail-fast: if any built-in Engine in the deployment manifest fails to register, the gear
  fails readiness, refuses requests, increments `engine_bootstrap_failures_total`, and emits a structured log entry; the
  gear does NOT silently fall back to a different Engine
- [ ] Validity-window semantics are Engine-driven: built-in `most-restrictive-wins` excludes Quotas whose `time` falls
  outside `[validity_start, validity_end]` from the Debit Plan by default; operator-authored Policies MAY override (
  grace periods, expected-window matching against `request.expected_window`); leases acquired within a valid
  window remain commit-able after `validity_end`; `currently_within_window` boolean is surfaced on every Quota Snapshot
  read of QE's own API
- [ ] Cross-period-boundary lease attribution (consumption Quotas): a lease is attributed to the period containing its
  acquisition timestamp; commit/release/auto-release that fire after a period rollover apply against the acquisition
  period's counter, not the new period's. Verified end-to-end — `Quota Q (period=day, cap=1000, consumed=0)` at 23:59
  UTC: `reserve(amount=200)` succeeds, `Q.consumed_today += 200`. After 00:00 UTC rollover,
  `Q.consumed_today (new period) = 0`; `commit(lease_token, actual=150)` applies to the **prior** day's counter (closed)
  and does not increment the new day's `consumed`
- [ ] A rollback or return of a charge whose entries are all in a settled period or on rate Quotas fails and changes
  nothing; one of a charge that also has an allocation entry succeeds and changes only the allocation counter; for
  cross-period guaranteed cancellation, callers use the lease flow
- [ ] Rollback names a charge by its tenant, operation, and idempotency key, with no amount and no key of its own.
  Verified end-to-end — `reserve(amount=200, key=K1)` → `commit(actual=150, key=K2)` writes a charge under
  `(commit, K2)`; `rollback(operation=commit, key=K2)` restores `Q.consumed` by 150; after a return of 50 from that
  charge, a rollback restores only the remaining 100, and a second rollback succeeds and changes nothing. Rollback of
  an unsettled lease is **not** supported via this primitive — callers use `release`
  (`cpt-cf-quota-enforcement-fr-lease-release`) before commit
- [ ] On QE's own API the PDP authorizes a rollback, for the action `rollback`, for the full attribution of the
  charged operation (tenant, subjects, and metric), as it authorizes a debit; a charge outside the caller's access,
  or with another attribution, gets the same answer as an unknown charge
- [ ] Every return or rollback that gives back an amount emits exactly one `quota-rollback-applied` notification event
  carrying the charge's operation and key, the returned amount, what remains of the charge, the affected Quotas, and
  the actor (the caller on QE's own API, the platform identity of the resolver's caller on the resolver plugin
  path); the event kind is
  distinct from `quota-counter-adjusted` (which fires only for credits per `cpt-cf-quota-enforcement-fr-credit`). One
  that fails or changes nothing emits no event
- [ ] Negative `cap` is rejected at create/update with `CAP_MUST_BE_NON_NEGATIVE`; `cap = 0` is permitted with
  documented semantics (`hard` Quota with `cap=0` denies every debit); `cap = null` (unbounded) is permitted with
  documented semantics (always satisfiable, counter still increments, `remaining` reported as `null`);
  `notification_thresholds` on unbounded Quotas are rejected at create/update with `THRESHOLDS_REQUIRE_BOUNDED_CAP`;
  the `quota_cap_zero_total` and `quota_cap_unbounded_total` gauges count the active Quotas of each kind
- [ ] Quota record carries `enforcement_mode` from GTS instances under `gts.cf.core.qe.enforcement_type.v1~` (P1: only
  `hard` is accepted); attempts to create a Quota with an unsupported `enforcement_mode` value are rejected with an
  actionable error; future values are added as new GTS instances per `cpt-cf-quota-enforcement-fr-enforcement-mode`
- [ ] Quota record carries `source` from GTS instances under `gts.cf.core.qe.source_type.v1~` (P1: `licensing` (default),
  `operator`); attempts to create a Quota with an unsupported `source` value are rejected with an actionable error;
  mutation rules in P1 are uniform across both source values (operator-level or Quota-Manager PDP scope); P2
  source-kind extensions (`tenant_admin`, `user_self`) and per-source mutation rules are additive per
  `cpt-cf-quota-enforcement-fr-quota-lifecycle`
- [ ] (P2) Bulk Quota CRUD endpoints (`cpt-cf-quota-enforcement-fr-bulk-quota-crud`) — `bulk_create_quotas`,
  `bulk_update_quotas`, `bulk_deactivate_quotas` — are transactional: partial failure rolls back the entire batch;
  envelope idempotency replay returns original outcome; max batch size (default 50, operator-configurable) is enforced
  with `BULK_TOO_LARGE`; same PDP, tenant-isolation, and trust-boundary rules apply as for single-item CRUD
- [ ] Threshold notifications fire once per upward transition: a `Denied` outcome or a canonical-error response does NOT
  emit `threshold-crossed` (counters not modified, no transition); an `Allowed` mutation crossing one or more thresholds
  emits exactly **one** `threshold-crossed` event whose payload carries `crossed_thresholds` (ascending list) and
  `highest_crossed_threshold`. The highest-crossed marker per `(Quota, period)` resets at period rollover. Verified:
  `consumed` 30 → 85 with `notification_thresholds = [50, 80, 100]` emits 1 event (`crossed=[50,80]`, `highest=80`);
  subsequent `consumed` 85 → 90 emits 0 events; period rollover resets, new-period `consumed` 0 → 60 emits 1 event
  (`crossed=[50]`, `highest=50`)
- [ ] An idempotency key is unique per `(tenant_id, operation_type, key)`; the same key string under another tenant or
  operation type is an independent record; the same key with another payload, other subjects included, is rejected
  with `IDEMPOTENCY_PAYLOAD_MISMATCH` before any change per `cpt-cf-quota-enforcement-fr-idempotency`
- [ ] A repeated allowed debit or lease commit gets the first result for as long as its charge lives, also after the
  replay window; with another payload it is a payload mismatch
- [ ] An allowed batch debit item writes a `debit` charge under its key; a batch with an item key that already names a
  `debit` charge or the record of a denied debit is rejected as a whole and changes nothing, also when the single
  debit commits while the batch is evaluated
- [ ] A repeat of the return or rollback that took the last amount of a charge gets the first result for one replay
  window after it; one return key on two charges is a payload mismatch
- [ ] Telemetry counters for lease-inflight-limit and Debit-Plan-invariant rejection paths are exposed:
  `lease_inflight_limit_exceeded_total`, `debit_plan_invariant_violations_total` (with documented invariant values);
  each counter increments on its corresponding rejection path and is queryable via the standard metrics scrape;
  `lease_inflight_limit_exceeded_total` is labelled by canonical registered `metric`
- [ ] Operations targeting a Quota whose metric is classified `Direct` by its metric type are rejected with
  `METRIC_NOT_QUOTA_GATED` for every entry-point that takes or checks capacity (`debit`, `reserve`, batch-debit,
  `evaluate-preview`), and `credit`, `rollback`, `return`, `commit`, and `release` of an existing Quota, lease, or
  charge still run — for a rejected operation counters are not mutated, no idempotency record / operation
  log entry / lease row is created; Quota Snapshot reads still succeed. Creating a Quota for a `Direct`-mode metric is
  permitted, and the Quota is counted by the `quota_for_direct_metric_total` gauge while it stays active
- [ ] Operations against a `QuotaGated` metric for which subject resolution produces zero applicable Quotas are denied
  with `Denied(violated_quota_ids=[], reason="NO_APPLICABLE_QUOTA")` per §3.2 default-deny semantics; counters are not
  mutated, no idempotency record / operation log entry is created. The behavior is uniform across all built-in Engines
  (`most-restrictive-wins` returns this directly; `cel` is expected to return the same shape — Policies that emit
  `Allowed` with an empty `debit_plan` violate Debit-Plan invariant #5 and surface a canonical `Internal` error)
- [ ] Evaluate-preview operation (`cpt-cf-quota-enforcement-fr-evaluate-preview`) returns the Decision the system would
  produce for an equivalent debit, with `preview: true` marker; counters are not mutated; no idempotency record /
  operation log entry / lease row is created; PDP and trust-boundary rules are applied identically to debit
- [ ] Per-Policy Engine timeout enforced (default 5ms); timeout surfaces a canonical `DeadlineExceeded` error
  (fail-closed); runtime CEL errors (cost-cap exceeded, type error at evaluation, malformed return record) surface a
  canonical error (`ResourceExhausted` for cost-cap; `FailedPrecondition` with `POLICY_EVALUATION_FAILED` for a type
  error; `Internal` for a malformed return record), all fail-closed
- [ ] Quota Snapshot read (`cpt-cf-quota-enforcement-fr-quota-snapshot-read`) returns the engine-agnostic per-Quota
  state list for explicit PDP-authorized tenant/subject/metric filters; no aggregate "headline" cap/balance is computed
- [ ] Consumer and manager/operator snapshot calls return the same per-Quota state contract; both carry explicit
  targets authorized against their respective service/user-plane principals and PDP scope
- [ ] Snapshot responses carry no Quota Resolution Policy attribution (no `policy_id`, `policy_version`, `scope`,
  `engine_id`, `engine_config`, summary, or content hash); callers needing Policy attribution use
  `cpt-cf-quota-enforcement-fr-evaluate-preview` (`diagnostics.policy_id` + `policy_version`) or the Policy-read API in
  `cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`
- [ ] An end user reads their own quota state through the Quota Resolver and the resolver plugin; the page leaves out
  `enforcement_mode` and the validity window and carries each Quota's `metadata`;
  end users never call QE directly
- [ ] Bulk Quota Snapshot read (`cpt-cf-quota-enforcement-fr-bulk-quota-snapshot-read`) paginates results when the
  result set exceeds page size
- [ ] Operations that the Engine denies produce `Denied` decisions with full violator identity; counters are
  not modified
- [ ] Tenant isolation: every **per-tenant** persisted row carries `tenant_id`; cross-tenant reads/writes are rejected
  at storage and API layers; on the resolver plugin path storage alone confines every operation to the request's
  tenant. Platform-wide operator-managed entities (Quota Resolution Policies, metric types in `types-registry`,
  registered Quota Resolution Engines) are not tenant-scoped per `cpt-cf-quota-enforcement-fr-tenant-isolation`
  (§5.12) and are addressed via their own scope discriminators rather than `tenant_id`
- [ ] All API operations require authentication; unauthenticated requests are rejected before any operation; the
  resolver plugin path is in process only and takes no `SecurityContext`
- [ ] Authorization on QE's own API is PDP-gated; PDP denial surfaces a canonical `PermissionDenied` error and PDP
  unreachable surfaces a canonical `ServiceUnavailable` error mid-evaluate (both fail-closed); PDP-returned
  constraints are applied as additional read filters
- [ ] The resolver plugin path makes no PDP call, reaches only the request's tenant, and refuses policy management;
  QE's own API still calls the PDP; the operation log records the platform identity of the resolver's caller as the
  actor of plugin operations
- [ ] Quota state through the resolver plugin returns only the tenant's Quotas when the metric does not admit the
  subject's type, without an error, and leaves out Quotas outside their validity window
- [ ] Return of a charge: partial and repeated returns unwind in reverse chain order; a repeated return (same key,
  charge, and amount) changes nothing, also after the replay window while the charge lives, and the same key with another charge or amount
  is a payload mismatch; a return on a
  deactivated allocation or consumption Quota succeeds; a return after a credit takes a counter to zero, not below; a
  return on a rate Quota next to an allocation Quota succeeds and changes
  only the allocation counter; a return on rate Quotas alone, on a settled consumption period alone, above what
  remains, or of an unknown charge fails and changes nothing
- [ ] Charge deletion: a charge is deleted when the replay window has passed since its creation and since the last
  return or rollback applied to it, and no allocation entry of it holds an amount; from that moment every operation
  treats it as unknown, swept or not
- [ ] Storage plugin contract is implemented by the P1 plugin built on `toolkit-db`; alternative backends are viable
  under the same plugin contract; operator selects the active plugin via configuration
- [ ] Notification plugin contract dispatches all eight event kinds (`threshold-crossed`, `period-rollover`,
  `lease-auto-released`, `lease-resolved-by-deactivation`, `quota-changed`, `quota-counter-adjusted`,
  `quota-rollback-applied`, `policy-changed`) to all registered sinks
- [ ] Notification dispatch failure does not block Quota Enforcement write operations; failures are surfaced via
  telemetry
- [ ] Single-quota evaluation operations meet p95 ≤ 100ms at 10 000 ops/sec sustained
- [ ] System sustains ≥ 100M subjects with ≥ 10 Quotas per subject (≥ 1B total Quotas) without degrading evaluation
  latency
- [ ] System maintains 99.95% monthly availability for evaluation endpoints
- [ ] Idempotency guarantee: zero double-count events under simulated retry storms (10× normal RPS, 5% retry rate, 7-day
  soak)
- [ ] Storage fault tolerance: zero committed-data loss across storage backend restarts and gateway restarts
- [ ] Recovery time: normal evaluation resumes within 15 minutes of storage backend recovery

## 10. Dependencies

| Dependency                              | Description                                                                                                                                                                                                                                                                                                         | Criticality |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------- |
| `toolkit-db`                             | Database infrastructure used by the P1 storage plugin                                                                                                                                                                                                                                                               | p1          |
| `authz-resolver`                        | Platform PDP for read and write authorization                                                                                                                                                                                                                                                                       | p1          |
| `types-registry`                        | Metric types, with their traits and schemas, and quota subject types. QE reads a metric type by its id: at start for the active Quotas and Policies, at Quota/Policy writes, on the first request for a metric, on a request whose metric object does not fit the schema of the served entry, and on a bounded schedule; any other evaluation of a metric the metric catalogue serves makes no registry call. | p1          |
| Quota Resolver SDK                      | Quota subject base, tenant subject type, and metric base; the plugin contract that QE implements (`cpt-cf-quota-enforcement-fr-resolver-plugin`). | p1          |
| Engine evaluator                        | Library backing the active Quota Resolution Engine. P1: a sandboxed CEL evaluator (mandatory for the built-in `cel` Engine). P2-or-later candidates: Starlark, Lua, Wasm runtimes. The `most-restrictive-wins` Engine has no external library dependency.                                                           | p1          |
| `cluster` (platform gear)               | Leader election for the sweeper singletons via `cpt-cf-quota-enforcement-contract-cluster-coordination`. The operator binds the `quota-enforcement` cluster profile to a backend in deployment YAML (standalone for one process, Postgres for multi-instance, further backends as cluster plugins land) without touching Quota Enforcement code. | p1          |

## 11. Assumptions

| Assumption                                                                                                                  | Owner                              | Validation                                                                                                               |
| --------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| The `toolkit-db` infrastructure is available and provisioned with sufficient capacity for Quota Enforcement's expected scale | Platform Infrastructure            | Verified at gear bootstrapping; Quota Enforcement fails to start if `toolkit-db` is unreachable                         |
| `authz-resolver` is deployed and reachable                                                                                  | Platform Infrastructure            | Verified at gateway startup via health check; gateway fails readiness if PDP is unreachable                              |
| `types-registry` is deployed and reachable for metric validation                                                            | Platform Engineering               | Verified at gateway startup; Quota create/update operations, and requests for a metric the metric catalogue does not hold, fail with an actionable error if the registry is unreachable |
| Quota Enforcement is the backend of the Quota Resolver | Platform Engineering | The Quota Resolver gear publishes the GTS types of its SDK, and the platform runtime waits for their admission before any gear starts; Quota Enforcement registers none of them and needs no start order with the Quota Resolver, but a deployment of Quota Enforcement includes the Quota Resolver gear |
| Per-tenant quota density and operation rate fit within the published NFRs at first production deployment                    | Platform Engineering               | Pre-prod load testing at 10 000 ops/sec across 100M synthetic subjects                                                   |
| Notification sinks register with the Quota Enforcement plugin contract at deployment time                                   | Per-deployment integration team    | Quota Enforcement telemetry surfaces "no notification sinks registered" warning; events are dropped silently otherwise   |
| At least one storage plugin is deployed alongside the Quota Enforcement gear                                              | Platform Infrastructure / Operator | Gateway readiness check fails when no plugin resolves                                                                    |

## 12. Risks

| Risk                                                                                                                                           | Impact                                                                                                         | Mitigation                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ---------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Hot-key contention on a single tenant's high-RPS counter | Latency violations on that tenant's evaluations; `LEASE_CONTENTION_TIMEOUT` rejections, which count against the availability target | Operations on one Quota run one after another, and the contention timeout (default 50 ms) bounds the wait; a module with a high request rate on one Quota charges in fewer, larger operations (one lease for many units of work, or a batch debit); P1 latency and contention monitoring; sharded-counter design deferred to P2 if observed |
| Custom Engine footguns (infinite loops, expensive computation, malformed Debit Plans) degrade evaluation latency or threaten counter integrity | Latency violations on Policies using non-default Engines; would-be counter inconsistency if invariants slipped | Per-Policy Engine timeout (default 5ms); strict Debit-Plan invariants enforced at the Engine boundary; Engine failure or any invariant violation surfaces a platform-canonical error (per §3.4 — `DeadlineExceeded` for timeouts, `Internal` for invariant violations and engine-internal failures, `ResourceExhausted` for cost-cap exhaustion) with no counter mutation; Engine configs validated at Policy create/update (e.g., the `cel` Engine performs parse + type-check); sandboxed evaluators (no I/O); per-Engine telemetry surfaces invariant-violation rates |
| Lease TTL too long → held capacity blocks legitimate operations                                                                                | False denials                                                                                                  | Operator-configured TTL bounds; lazy expiry interpretation makes auto-release immediate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Notification plugin failure floods telemetry but blocks no operations                                                                          | Lost threshold notifications                                                                                   | Best-effort delivery + telemetry surface                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Multi-quota evaluation latency grows non-linearly with assignment count per subject                                                            | NFR violations at high quota density                                                                           | P1 indexing strategy in DESIGN.md; benchmarks at 10 assignments per subject at target RPS                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Operator-supplied Quota Metadata grows unbounded or carries semantics QE didn't anticipate                                                     | Storage bloat; classification drift; PII leakage if operator misuses the field                                 | Per-deployment configurable byte-size limit (default 4 KB canonical JSON); the check of the metadata against the metric type on write; explicit "no PII" data classification; PDP-scoped access                                                                                                                                |
| A wrong metric type does not fail bootstrap | Its metric is unavailable until an operator notices | Each unavailable metric is reported with its reason in a log entry and in telemetry, for an alert (`cpt-cf-quota-enforcement-fr-subject-type-registry`, ADR-0012) |
| The resolver plugin path runs without a PDP check | A module that charges through the Quota Resolver can charge any tenant it names | Trusted admission is reachable only through the plugin client, in process; modules in the process are trusted like callers of the Quota Resolver's platform plane; one request never spans tenants; QE's own API keeps PDP; when the Quota Resolver authorizes its platform-plane callers, calls through it are authorized there, and the plugin client itself stays without a policy check (ADR-0009) |
| Every allowed debit and lease commit writes a charge | Storage growth: about one replay window of charges, the same order as the idempotency records, plus the allocation charges that still hold an amount on an active Quota; the charge of a lost return stays until the module that made the charge returns it | A charge is deleted when the replay window has passed since its creation and since the last return or rollback applied to it, unless an allocation entry holds an amount on an active Quota; a gauge reports allocation charges older than the replay window; the sweeper removes deleted charges in batches; capacity stated in DESIGN (ADR-0011) |
| An idempotency key is taken for the whole tenant, and an `IDEMPOTENCY_PAYLOAD_MISMATCH` answer tells a caller that a key is in use (ADR-0011) | A caller learns that some operation of the tenant used that key. A caller that sends another caller's key first takes it: the other caller gets a payload mismatch for as long as the charge lives, with no time limit for an allocation charge | Accepted exception. The answer carries no part of the stored request; callers on the resolver path are trusted platform services, and callers of QE's API are authorized by the PDP for the tenant; a module builds its keys from its own name, so it meets only its own keys |

## 13. Open Questions

- **Metric-identifier namespace (resolved).** The metric base is `gts.cf.core.quota.metric.v1~`, owned by the Quota
  Resolver SDK, and each metric type is named under its single owning Gear. Usage Collector keeps its own
  usage-record family (`gts.cf.core.uc.usage_record.v1~`), so aligning the two is a mapping between families, not a
  shared base.
- **Built-in `cascade-priority` Engine**: cascade is expressible via `cel` in P1; should P2 ship a hardcoded
  `cascade-priority` Engine for operators who do not want to author CEL? Trade-off: convenience and lower
  CEL-attack-surface vs. coupling QE to one specific cascade variant before operator preferences are observed in
  production. — owner: Platform Engineering — target resolution: deferred — production feedback.
- **P2 Engine candidates**: which alternative Engine languages (Starlark, Lua, Wasm-loaded operator engines) earn a P2
  slot? Decision criterion: sandboxability (no I/O), determinism, predictable resource limits, fast startup, and
  demonstrated CEL coverage gaps. — owner: Platform Engineering — target resolution: P2 design discussion.
- **Burst semantics for rate Quotas (P3)**: token bucket vs sliding window vs fixed window for the future rate quota
  type, including per-tenant vs per-region smoothing axis and the interaction between burst and lease TTL. The P3
  contract enumerated in `cpt-cf-quota-enforcement-fr-quota-type-rate-declared` lists
  `(rate, burst_capacity, smoothing_window)` as the field shape but defers the bucket-vs-window choice to
  implementation. — owner: Platform Engineering — target resolution: rate-quota implementation phase.
- **Subscription-term and anniversary-aligned periods**: P1 periods are calendar-aligned UTC (`day`, `week`, `month`,
  `year`) plus non-recurring `one_time` per `cpt-cf-quota-enforcement-fr-period-semantics`. App licensing integrations
  (e.g., the platform licensing/quota integration guide for the Proctor app) need caps that reset on an agreement's
  anniversary or span a subscription term; today the only faithful representation is `one_time` plus a validity window,
  and an arbitrary anniversary reset is not representable. Candidate resolutions: a new period GTS instance carrying an
  anchor timestamp, or a Quota Manager-side rollover/re-materialization mechanism that recreates the Quota per term.
  Decision criterion: a committed app-fulfillment consumer plus the cross-gear rollover ownership question. — owner:
  Platform Engineering / Quota Manager — target resolution: first app-fulfillment integration.
- **Period-end carry-over of unused capacity**: P1 has no rollover-of-unused semantics — every period boundary resets
  `consumed` to 0 and forfeits leftover capacity, matching the licensing service P1 PRD; neither models carry. If
  concrete product demand for "unused tokens roll over to next period" emerges, ownership belongs to the
  subscription/licensing layer (Quota Manager), not QE: that layer subscribes to `period-rollover` events, computes
  carry per subscription tier, and pushes the new cap back to QE via `update_quota`. QE remains carry-policy-free.
  Decision criterion: a concrete carry use case from a licensing-tier requirement, observed in production. — owner:
  Platform Engineering — target resolution: deferred — production feedback.
- **Period-rollover events for idle Quotas**: period boundaries are detected lazily, so a `period-rollover` event is
  emitted only when a mutating operation reaches a Quota after its boundary; a period in which a Quota saw no mutation
  produces no event, and snapshot reads do not open periods. DESIGN §4.3 already plans the mechanism for P2: an active
  rollover scheduler, a scheduled scan keyed off the period end that is independent of operations on individual
  Quotas. Open: do consumers (Quota Manager, billing) need the event for every active Quota at every boundary, idle
  ones included, and by when? Decision criterion: a consumer that needs the closing figures of idle periods. — owner:
  Platform Engineering — target resolution: P2 design discussion.
- **Cap-clamp for batch-style admission (P3)**: workloads that admit "70 of the 100 instances I requested" are not
  modeled in P1 — every Quota is `enforcement_mode = hard`. If concrete batch admission use cases emerge (LLM token
  batches with partial-response semantics, scheduler bulk admissions), introduce `enforcement_mode = hard-with-clamp`
  plus an `AllowedWithClamp(quota_id, admitted_magnitude)` Decision arm, whose chains add up to the admitted
  magnitude instead of `request.amount`. The current
  `cpt-cf-quota-enforcement-fr-batch-debit` envelope (multiple metrics under atomic all-or-nothing in P1;
  bulk-independent partial-success deferred to P2) does not require clamp; clamp solves a different problem (single
  metric, partial admission). — owner: Platform Engineering — target resolution: P3 PRD update if a concrete batch use
  case emerges.
- **CEL-based notification policies**: P1 uses fixed `notification_thresholds` (percentages of cap) on each Quota and
  emits `threshold-crossed` events on actual upward transitions (`cpt-cf-quota-enforcement-fr-notification-plugin`). A
  P2 enhancement could let operators declare CEL predicates that gate event emission on richer signals — e.g., "emit
  only if `consumed > 0.8 * cap` AND `request.tier == 'premium'`", or "emit a `quota-counter-adjusted` only
  when the credit amount exceeds some threshold". This composes with the existing `cel` Engine plugin contract but lives
  in the notification layer, not the admission layer. Trade-off: operator authoring power vs. notification-fatigue risk
  and additional CEL evaluation cost on every counter mutation. Decision criterion: production demand observed after P1
  ships fixed thresholds. — owner: Platform Engineering — target resolution: deferred — production feedback.
- **Engine diagnostic schema standardization**: Engines currently produce free-form `diagnostics` for callers; should we
  standardize a schema across Engines so dashboards/UIs can render uniform breakdowns? — owner: Platform Engineering —
  target resolution: P2 telemetry pass.
- **Quota Metadata indexing**: P1 does not index Quota Metadata for direct query (QE core matches the conditions of the
  Quotas of the request's subjects and metric in memory). Should P2 add per-tenant metadata indexing for direct lookup APIs (e.g., "find all Quotas for tenant T where
  `metadata.region=us-east-1`")? Trade-off: query convenience vs storage cost vs cache-invalidation complexity. — owner:
  Platform Engineering — target resolution: deferred — production feedback.
- **Per-resource counter axis**: the `id` of the metric object is the place for this axis: it names the one thing
  the amount is counted for, and in P1 it reaches the PDP, the Engine, and the charge's attribution but not
  the counter key, so a Policy can filter by it but no counter exists per `id`. A Quota on a named
  thing is already expressible by matching request and Quota properties, and several such Quotas may legitimately
  share one `(subject_type, subject_id, metric)` key. The ceiling is explicit: properties do not build the
  applicable-Quota set, so every such Quota is included in every locked read for that subject/metric and QE
  applies its conditions only afterward (as the separate Metadata-indexing question notes). That scales to tens of ids, not
  thousands. If a concrete consumer needs materially larger cardinality, the metric `id` should become a counter-key
  component so counters materialize per id on first use; a property convention cannot create counters. Decision
  criterion: a concrete consumer plus an order-of-magnitude estimate of the number of ids. — owner: Platform
  Engineering — target resolution: demand-driven design.
- **Deactivated-Quota auto-purge**: P1 retains deactivated Quotas indefinitely for read access (no automatic delete).
  Should P2 introduce a per-tenant operator-configurable grace-period auto-purge (e.g., "hard-delete deactivated Quotas
  after 90 days") tied to audit infrastructure? Trade-off: storage growth on long-lived tenants vs audit/compliance
  trail durability. — owner: Platform Engineering / Compliance — target resolution: P2 audit infrastructure rollout.
- **Grace policies for hard caps**: detailed design for "+N% for T minutes" grace windows on `enforcement_mode = hard`
  Quotas (e.g., during a campaign or an incident, allow 110% of cap for the next 30 minutes). P2 enhancement; orthogonal
  to the rate-quota burst contract since grace applies to consumption Quotas while burst applies to rate Quotas. —
  owner: Platform Engineering — target resolution: P2 enhancement.
- **EventBus integration**: Routing semantics, delivery guarantees, and migration plan from in-process notification
  plugin to platform EventBus. — owner: Platform Infrastructure — target resolution: when EventBus availability is
  committed.
- **Audit design**: Schema and storage destination for definition-change audit and consumption-event audit, including
  retention and PII-handling expectations. — owner: Platform Engineering / Compliance — target resolution: P2 audit
  infrastructure rollout.
- **Multi-source Quotas and subscriber-customizable thresholds (P2)**: P1 ships a two-value `source` enum on every Quota
  — `licensing` (materialized by the licensing layer) and `operator` (manual caps for incidents and compliance) — with
  **uniform** mutation rules across both values (operator-level or Quota-Manager PDP scope). P2 introduces a Quota
  Source Registry that extends the enum additively with `tenant_admin` (tenant-administrator-imposed caps on subjects
  within their tenant — for example a per-user sub-cap inside a tenant license) and `user_self` (end-user
  self-protection caps, e.g., a Pay-as-you-go tenant limiting their own monthly spend). Each P2-introduced source kind
  declares the PDP scope required to mutate it and any cap-inheritance constraint (e.g., a `user_self` cap MUST NOT
  exceed the smallest applicable license cap for the same metric and subject). P2 also introduces
  subscriber-customizable notification thresholds via an auto-created `NotificationSubscription` per Quota: at Quota
  creation time the system automatically materializes a Subscription whose subscriber is the Quota's subject and whose
  thresholds are seeded from the Quota's `notification_thresholds` field. Subscribers MAY mutate, extend, or fully
  deactivate their auto-created Subscription to silence noisy default events or add custom thresholds — without
  modifying the underlying Quota. Concrete FRs and the corresponding schema additions land in the P2 PRD revision. —
  owner: Platform Engineering — target resolution: P2 PRD revision.
- **Hierarchical quota lookup as an additive extension**: P1 builds the applicable-Quota set per-
  `(subject_type, subject_id)` independently — no traversal of any subject hierarchy, no consumption against an
  ancestor's Quota. A future extension is expected to introduce hierarchy-aware lookup as a strictly additive change:
  an individual Quota opts in via a marker on the Quota itself, and evaluations targeting a descendant of that Quota's
  subject pick it up as part of the applicable set (potentially also propagating the debit up the chain). Open design
  points: (1) the marker's shape — a typed enum on the Quota record vs a reserved `metadata` key vs a
  trait on the metric type that opts a whole subject type in; (2) where the hierarchy itself lives and how
  the applicable-set builder discovers ancestors at evaluation time without inflating latency; (3) how ancestor walk
  composes with the Engine contract without altering `Decision` arms or Debit Plan invariants; (4) atomic multi-Quota
  acquisition (`cpt-cf-quota-enforcement-fr-lease-acquire`) across N ancestor counters under deadlock-free ordering;
  (5) period attribution when ancestor and descendant Quotas carry different period specs; (6) idempotency / rollback
  fan-out across the ancestor chain; (7) PDP scope for "descendant consuming against an ancestor's Quota". Decision
  criterion: concrete product demand for organization-tree consumption accounting plus a credible additive-shape
  proposal that preserves P1 behavior for non-opted-in Quotas. — owner: Platform Engineering — target resolution:
  deferred — production feedback.
- **Composable Policy patterns (P2)**: introduce a Pattern Registry parallel to metric types — operators select
  a parameterized pattern (`cascade`, `attribute_gated`, `most_restrictive_wins`, `cel` escape hatch) instead of
  authoring raw CEL for every Policy. Reduces operator-authored CEL footprint to the genuine long-tail. Patterns have
  proven cumulative behavior, statically-checkable parameter shapes, and are auditable at a glance. Concrete FR lands in
  P2 PRD revision. — owner: Platform Engineering — target resolution: P2 PRD revision.
- **Shadow evaluation for Policy promotion (P2)**: when an operator stages a new Policy version, it runs in shadow mode
  alongside the active version; both produce Decisions; production enforcement uses the active one; shadow Decisions are
  recorded for divergence analysis. Operator promotes (latest-pointer to shadow version) when divergence is acceptable;
  rollback is one click via the versioning primitive (`cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`).
  Concrete FR lands in P2 PRD revision. — owner: Platform Engineering — target resolution: P2 PRD revision.
- **Narrower-than-metric Policy scope**: P1 supports only `global` and `metric` scopes for Quota Resolution Policy
  (`cpt-cf-quota-enforcement-fr-quota-resolution-policy`). Real-world demand may emerge for narrower targeting —
  per-`subject_type` (different arbitration for tenant vs user subjects), per-specific-subject
  (premium-tenant carve-outs, surgical workarounds, A/B per-tenant Policy testing), or some other axis (e.g., per
  Quota-metadata attribute). The right shape is unclear: per-`subject_type` alone has a tie-break problem when an
  evaluation's applicable-subjects set spans several types simultaneously; per-`(subject_type, subject_id)` avoids the
  ambiguity but adds storage and operator-management cost; pinning to `quota_id` blurs the Quota-vs-Policy separation
  given that multiple Quotas can share `(subject, metric)`. The persistence and selection layers should be designed to
  admit additional scope levels without breaking changes (so a future addition does not require a Policy data
  migration), but no narrower scope is exposed in P1. Decision criterion: concrete operator use cases observed in
  production after P1, plus alignment with the related Composable Policy Patterns and Shadow Evaluation Open Questions
  (which may absorb some of the same use cases without expanding the scope ladder). — owner: Platform Engineering —
  target resolution: deferred — production feedback.
- **Policy unit-test framework (P3)**: operators declare `(EvaluationContext, expected Decision)` test cases stored
  alongside Policy; on create/update, the system runs the tests via the existing
  `cpt-cf-quota-enforcement-fr-evaluate-preview` infrastructure; failure rejects the change with actionable diff.
  Composes with versioning (each version has its own tests) and patterns (test obligation concentrates on residual CEL
  escape-hatch surface, not on parameterized templates whose properties are already proven). Deferred to P3 behind
  patterns (P2) because patterns reduce the volume of operator-authored CEL most needing coverage. Concrete FR lands in
  P3 PRD revision. — owner: Platform Engineering — target resolution: P3 PRD revision.
- **Constrained quota-arbitration DSL (post-P3 / open)**: design a domain-specific language for quota arbitration —
  statically typed, decidable, no loops/recursion, formally verifiable. Replaces CEL footprint over time as an
  alternative Engine. Decision criterion to revisit: scale of operator-authored CEL observed in P2/P3 production; if
  footprint stays small (most operators stay on patterns), CEL escape hatch remains adequate. If footprint grows
  unmanageable, design + prototype a constrained DSL. Indefinite timeline; re-evaluated yearly post-P3. — owner:
  Platform Engineering — target resolution: post-P3, conditional on production evidence.

## 14. Traceability

- **Informal upstream requirements**: no formal `UPSTREAM_REQS.md` is maintained for this gear
- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: see [DESIGN §5 Traceability](./DESIGN.md) for the canonical ADR catalogue. Metric types:
  [ADR-0013](./ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md) and
  [Quota Resolver ADR-0004](../../quota-resolver/docs/ADR/0004-cpt-cf-quota-resolver-adr-metric-types.md); metric
  catalogue: [ADR-0012](./ADR/0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md)
- **Features**: feature specifications under `docs/features/` (twelve documents, one per DECOMPOSITION entry)
- **Related gears**: [Usage Collector PRD](../../usage-collector/docs/PRD.md),
  [Account Management PRD](../../account-management/docs/PRD.md),
  [Resource Group PRD](../../resource-group/docs/PRD.md), [Types Registry PRD](../../types-registry/docs/PRD.md)
