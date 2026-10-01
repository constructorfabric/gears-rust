# Feature: Rating Foundation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-foundation-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Accept an upstream fact into the inbox](#21-accept-an-upstream-fact-into-the-inbox)
  - [2.2 Release a quarantined inbox entry](#22-release-a-quarantined-inbox-entry)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Authorize and scope every operation](#31-authorize-and-scope-every-operation)
  - [3.2 Record and settle an operation idempotency key](#32-record-and-settle-an-operation-idempotency-key)
  - [3.3 Record and sweep exceptions](#33-record-and-sweep-exceptions)
  - [3.4 Acquire and fence a lease](#34-acquire-and-fence-a-lease)
  - [3.5 Gate event intake and keep V1 compatible](#35-gate-event-intake-and-keep-v1-compatible)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Inbox entry](#41-inbox-entry)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Gear module, namespace and queues](#51-gear-module-namespace-and-queues)
  - [5.2 Tenancy, authorization and audit](#52-tenancy-authorization-and-audit)
  - [5.3 Inbox, exceptions and operation keys](#53-inbox-exceptions-and-operation-keys)
  - [5.4 Contract gates and provider test doubles](#54-contract-gates-and-provider-test-doubles)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Integration contracts: Provider Test Doubles (normative)](#integration-contracts-provider-test-doubles-normative)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Provide the shared runtime every pipeline feature builds on: the `rating` gear module and its
`bss_rating` namespace, migrations and work queues; the tenancy and authorization model with its
audited operator actions; one inbox for every non-usage upstream fact; the exception register and the
operation idempotency store; the lease port over the Cluster SDK; event intake that stays disabled
until a contract is adopted; the `rating-sdk` V1 compatibility rules; and the ClientHub provider test
doubles.

### 1.2 Purpose

Every pipeline path is at-least-once delivery plus one local transaction plus a deterministic key
(DESIGN §4.2), runs under one tenancy and operation matrix (DESIGN §4.8), and reaches other gears only
through ClientHub SDKs and one inbox. Building these once, before any capability, makes idempotency,
tenant isolation and fail-closed intake assertable in one place, and lets every other feature be
accepted against contract fakes before any upstream gear exists.

**Requirements** (primary, per DECOMPOSITION): none. **Supporting**: `cpt-cf-bss-rating-fr-idempotency`, `cpt-cf-bss-rating-nfr-resilience`, `cpt-cf-bss-rating-nfr-audit-segregation`.

**Principles**: `cpt-cf-bss-rating-principle-fail-closed`, `cpt-cf-bss-rating-principle-single-writer-cc`, `cpt-cf-bss-rating-principle-adopt-verbatim-cc`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Runs the inbox, workers and repositories under its service identity and a scope narrowed to one tenant |
| `cpt-cf-bss-rating-actor-subscriptions` | Producer of facts, segments and scopes that enter the inbox (identity verified per source) (TARGET; ASSUMED/ABSENT today — no Subscriptions code, UPSTREAM_REQS §2.6) |
| `cpt-cf-bss-rating-actor-billing` | Calls Rating's SDK as a service identity; source of the observational period hint (no Billing gear exists — ABSENT, UPSTREAM_REQS §2.7) |
| `cpt-cf-bss-rating-actor-platform-operator` | Releases quarantined inbox entries and performs every other audited operator action |
| `cpt-cf-bss-rating-actor-oss-ams` | Supplies tenant existence through the tenant directory (operator plane only) |

Actor labels grant nothing; every operation goes through the operation matrix of DESIGN §4.8.

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (idempotency), §7.1 (resilience, audit segregation), §9.
- **Architecture**: [DESIGN.md](../DESIGN.md) [§1.3](../DESIGN.md#13-architecture-layers) (crates and allowed dependencies), [§3.3](../DESIGN.md#33-api-contracts) (SDK conventions, REST plane, event contracts, V1 compatibility), [§3.5](../DESIGN.md#35-external-dependencies) (infrastructure dependencies), [§3.7](../DESIGN.md#37-database-schemas--tables) (namespace, ownership columns, `bss_rating__inbox`, `bss_rating__exception`, `bss_rating__operation`, outbox prefix), [§3.8](../DESIGN.md#38-deployment-topology), [§4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics), [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization); namespace 11 — [inbox and recovery](../DESIGN.md#contract-11-4-11), [contract governance](../DESIGN.md#contract-11-4-13); [10 §4.6](../DESIGN.md#contract-10-4-6). Provider test doubles are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.1.
- **Dependencies**: none at feature level; platform prerequisites in DECOMPOSITION §3.1.
- **Consumers**: every pipeline feature — [Usage Capture](04-usage-intake.md), [Attribution and Window Counters](05-attribution-counters.md), [Facts and Scheduling](06-fact-scheduling.md), [Child Evaluation](07-child-evaluation.md), [Corrections and Re-rate](08-corrections-rerate.md), [Roll-up and Delivery](09-rollup-delivery.md), [Order Evaluation](10-order-evaluation.md), [Operations](11-operations.md).
- **Upstream**: `cpt-cf-bss-rating-upreq-cluster-coordination-backend`, `cpt-cf-bss-rating-upreq-event-contracts` ([UPSTREAM_REQS §2.12](../UPSTREAM_REQS.md#212-platform-infrastructure)).

**UI applicability**: none; this feature specifies backend contracts. RFC 9457 problem details and non-disclosing `404` answers are its API-usability obligations.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flows are system flows shared by every capability.

### 2.1 Accept an upstream fact into the inbox

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-foundation-inbox-accept`

**Actor**: `cpt-cf-bss-rating-actor-subscriptions` (or another producer of a non-usage fact), through a pull sweep or, once enabled, an event consumer.

**Success Scenarios**: a new `(tenant, source, business_id, version)` is stored `accepted` with its digest and applied by the owning feature in the same transaction; a repeat with the same digest is a no-op.

**Error Scenarios**: same key with a different digest (quarantine and page); unverified producer identity or a payload tenant outside Rating's grant (refused); unsupported schema version (quarantined, never dropped).

**Steps**:
1. [ ] - `p1` - Verify the producer identity for the source and check the payload's owning tenant against Rating's grant - `inst-ia-verify`
2. [ ] - `p1` - BEGIN with a scope narrowed to that tenant; `Inbox::accept(tx, source, business_id, version, digest, payload, via)` - `inst-ia-accept`
3. [ ] - `p1` - **IF** the key is new, insert the row `accepted` and hand it to the owning feature's apply step in the same transaction - `inst-ia-new`
4. [ ] - `p1` - **IF** the key exists with the same digest, do nothing - `inst-ia-duplicate`
5. [ ] - `p1` - **IF** the key exists with a different digest, mark it `quarantined` with its reason and page - `inst-ia-conflict`
6. [ ] - `p1` - **IF** the entry came from an event consumer, commit its offset in this transaction (`LocalDbOffsetManager`) - `inst-ia-offset`
7. [ ] - `p1` - COMMIT; **RETURN** the inbox state - `inst-ia-return`

Canonical: [11 §4.11](../DESIGN.md#contract-11-4-11); usage entries never use this inbox ([Usage Capture](04-usage-intake.md)).

### 2.2 Release a quarantined inbox entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-foundation-inbox-release`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: the entry moves to `applied` (resolution `apply`) or stays recorded as discarded (resolution `discard`); the action is audited.

**Error Scenarios**: entry not quarantined (`409 FailedPrecondition`); entry outside the caller's scope (`404`); missing permission (`403`).

**Steps**:
1. [ ] - `p1` - Receive `POST /bss-rating/v1/inbox/{id}:release` with `{reason, resolution}` - `inst-ir-receive`
2. [ ] - `p1` - Authorize `usage × release` and bind the PDP scope; an entry outside it is `404` - `inst-ir-authorize`
3. [ ] - `p1` - **IF** the entry is not `quarantined`, refuse `409 FailedPrecondition` - `inst-ir-state`
4. [ ] - `p1` - Apply or discard per the resolution, record actor, subject and request id, and **RETURN** `InboxEntryView` - `inst-ir-apply`

The route, status codes and idempotency rule are owned by DESIGN §3.3; the list and retry routes of the exception register are served by [Operations](11-operations.md).

## 3. Processes / Business Logic (CDSL)

### 3.1 Authorize and scope every operation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-foundation-authorize`

**Input**: an operation of the DESIGN §4.8 matrix, the caller's `SecurityContext`, any tenant selector, or the `tenant_id` persisted on a work item.

**Output**: an `AccessScope` bound to every repository call, or a refusal.

1. [ ] - `p1` - Evaluate the operation's PDP check (deny by default); a PDP outage refuses - `inst-az-pdp`
2. [ ] - `p1` - Treat a tenant argument or filter as a selector that narrows the granted scope; a selector outside it returns `403` on collection reads and `404` on object reads - `inst-az-selector`
3. [ ] - `p1` - For background work, bind a scope narrowed to the work item's persisted `tenant_id`; never run an unscoped query - `inst-az-worker`
4. [ ] - `p1` - **IF** the action is `retry`, `release`, `execute`, `resolve`, hold `place` / `release` or a policy `write`, record the audit entry (actor, subject, request id) - `inst-az-audit`
5. [ ] - `p1` - **RETURN** the scope; repositories use `SecureConn` with it - `inst-az-return`

### 3.2 Record and settle an operation idempotency key

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-foundation-operation-key`

**Input**: a command carrying an `Idempotency-Key` (re-rate creation, policy and hold creation).

**Output**: the original receipt on a retry, a conflict on a different body, or a new operation.

1. [ ] - `p1` - Look up `bss_rating__operation (tenant_id, actor, key)` in the command's transaction - `inst-ok-lookup`
2. [ ] - `p1` - **IF** found with the same `request_digest`, **RETURN** the stored response - `inst-ok-replay`
3. [ ] - `p1` - **IF** found with a different digest, refuse `409 AlreadyExists` - `inst-ok-conflict`
4. [ ] - `p1` - Otherwise insert the key with the response in the same transaction as the command's effect; keep it 7 days - `inst-ok-insert`

### 3.3 Record and sweep exceptions

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-foundation-exceptions`

**Input**: a failure classified by a pipeline feature (`subject_kind`, `subject_ref`, `reason_code`).

**Output**: one open exception per `(tenant_id, subject_kind, subject_ref, reason_code)`.

1. [ ] - `p1` - Upsert the open exception, incrementing `attempts` and `last_attempt_at` - `inst-ex-upsert`
2. [ ] - `p1` - Run the exception sweeper every 15 minutes under the Cluster lock `bss-rating/exception-sweep`; it re-enqueues retryable subjects (for example `awaiting_attribution`, `invalidation_target_unknown`) - `inst-ex-sweep`
3. [ ] - `p1` - Mark an exception resolved with its resolution when its subject succeeds or an operator resolves it - `inst-ex-resolve`

### 3.4 Acquire and fence a lease

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-foundation-lease`

**Input**: a lease name (`bss-rating/usage-feed/{source_id}`, `bss-rating/scheduler/{shard}`, `bss-rating/recovery/{shard}`, `bss-rating/reconcile/{shard}`, …).

**Output**: a held lease with a fence, or no lease (the task waits).

1. [ ] - `p1` - Acquire through the Rating-local `LeaseProvider` port, backed by `DistributedLockApi` / `LeaderElectionApi` (R-27) - `inst-ls-acquire`
2. [ ] - `p1` - Renew before expiry; on loss, stop the task's work - `inst-ls-renew`
3. [ ] - `p1` - Carry the fence into every checkpoint write, which accepts it only if `fence ≥ stored fence`; a zombie holder's write fails its CAS - `inst-ls-fence`

Leases only save work; correctness stays with the database CAS, fences and unique keys (DESIGN §3.8). A transitional `bss-coord` adapter is allowed behind the same port only if R-27 approves it.

### 3.5 Gate event intake and keep V1 compatible

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-foundation-contract-gates`

**Input**: the configured event sources; a change to a Rating-owned contract.

**Output**: event consumers started only where enabled; contract changes accepted only if additive.

1. [ ] - `p1` - Start an event consumer for a source only when configuration enables it, which requires the owner's adopted contract (DESIGN §3.3 event table, T-D-71); every source is disabled by default and pull reads stay authoritative - `inst-cg-events`
2. [ ] - `p1` - Fail CI when a golden JSON fixture of a V1 DTO, REST response or canonical form (`wk1`, `ek1`, `rsnap1`) changes non-additively - `inst-cg-golden`
3. [ ] - `p1` - Map every SDK error to `CanonicalError` and every REST error to RFC 9457 problem details - `inst-cg-errors`

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-test-doubles-cc`
  — Integration contracts — Provider Test Doubles (normative) ([contract](#contract-11-4-12))

## 4. States (CDSL)

### 4.1 Inbox entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-foundation-inbox`

**States**: `accepted`, `applied`, `quarantined`.

**Initial State**: `accepted`.

**Transitions**:
1. [ ] - `p1` - **FROM** — **TO** `accepted` **WHEN** a new key is accepted - `inst-is-accept`
2. [ ] - `p1` - **FROM** `accepted` **TO** `applied` **WHEN** the owning feature applies it in the same transaction - `inst-is-apply`
3. [ ] - `p1` - **FROM** — **TO** `quarantined` **WHEN** the key exists with a different digest, or the schema version is unsupported - `inst-is-quarantine`
4. [ ] - `p1` - **FROM** `quarantined` **TO** `applied` **WHEN** an operator releases it with resolution `apply` - `inst-is-release`

## 5. Definitions of Done

### 5.1 Gear module, namespace and queues

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-foundation-module`

The system **MUST** provide the `cf-gears-bss-rating` module with the `stateful` lifecycle, the `bss_rating` namespace and its naming grammar (T-D-60), the first migration of every table of DESIGN §3.7, and the `rating.child_work`, `rating.rollup` and `rating.delivery` outbox queues under the `bss_rating__outbox` prefix, with `rating-core` kept free of I/O dependencies by a CI deny-list.

**Implements**: `cpt-cf-bss-rating-algo-foundation-lease`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `cpt-cf-bss-rating-tech-stack-main`, `cpt-cf-bss-rating-topology-main`; outbox tables; Cluster SDK through ClientHub.

### 5.2 Tenancy, authorization and audit

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-foundation-authz`

The system **MUST** register the GTS resource types of DESIGN §4.8, enforce the operation matrix for SDK, REST, consumers and workers alike, scope every repository call with a PDP-compiled `AccessScope`, answer cross-tenant object ids with `404`, and audit every privileged action.

**Implements**: `cpt-cf-bss-rating-algo-foundation-authorize`.

**Constraints**: `cpt-cf-bss-rating-constraint-inprocess-boundary-cc`, `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: every Scopable entity (`tenant_id`, `#[secure(...)]`); `PolicyEnforcer`; GTS types registry.

### 5.3 Inbox, exceptions and operation keys

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-foundation-inbox`

The system **MUST** accept every non-usage upstream fact through the inbox with digest-based duplicate absorption and quarantine, support audited release, record failures in the exception register with one open row per subject and reason, and settle `Idempotency-Key` commands through the operation store.

**Implements**: `cpt-cf-bss-rating-flow-foundation-inbox-accept`, `cpt-cf-bss-rating-flow-foundation-inbox-release`, `cpt-cf-bss-rating-algo-foundation-operation-key`, `cpt-cf-bss-rating-algo-foundation-exceptions`, `cpt-cf-bss-rating-state-foundation-inbox`.

**Constraints**: `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: API: `POST /bss-rating/v1/inbox/{id}:release`; `cpt-cf-bss-rating-dbtable-inbox`, `cpt-cf-bss-rating-dbtable-exception`, `bss_rating__operation`.

### 5.4 Contract gates and provider test doubles

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-foundation-contracts`

The system **MUST** ship `cf-gears-bss-rating-sdk` with golden DTO fixtures and the additive-only V1 rule, keep every event path disabled until its contract is adopted, and register the provider test doubles of [11 §4.12](#contract-11-4-12) behind the `test-providers` feature so every other feature is accepted against them; a double never stands in for a production contract.

**Implements**: `cpt-cf-bss-rating-algo-foundation-contract-gates`, `cpt-cf-bss-rating-algo-test-doubles-cc`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-extension-cc`, `cpt-cf-bss-rating-constraint-rating-contract-draft-cc`.

**Touches**: `cpt-cf-bss-rating-interface-events`; `rating-sdk`; ClientHub registrations.

Observability: `rating_inbox_quarantined_total{source}`, `rating_dependency_errors_total` and the `rating.inbox.accept` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A fact accepted twice with the same digest produces one inbox row and one applied effect; with a different digest it is quarantined and paged, and nothing is applied.
- [ ] A fact whose payload tenant is outside Rating's grant, or whose producer identity does not verify, is refused and writes nothing.
- [ ] Releasing an entry that is not quarantined returns `409 FailedPrecondition`; a release is audited with actor, subject and request id.
- [ ] A retried command with the same `Idempotency-Key` and body returns the original receipt; the same key with a different body returns `409 AlreadyExists`.
- [ ] A worker processing tenant A's item cannot read tenant B's rows; no repository call runs without a scope.
- [ ] A zombie lease holder's checkpoint write fails its CAS; losing a lease only wastes work.
- [ ] Every event source is disabled in the default configuration, and with every source disabled the pull paths of every feature still complete.
- [ ] A non-additive change to a V1 DTO fails the golden-fixture CI job.
- [ ] `rating-core` builds with no tokio, sea-orm, http or ClientHub dependency.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V23 | Tenant B's snapshot, run, policy or exception id requested by tenant A | `404` |
| V24 | `deliveries_since` with a tenant outside the caller's scope | `PermissionDenied` |

## 7. Detailed Behavior Contracts

**Contract namespace 11.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-11-4-12"></a>

<!-- contract:11-consumer-contracts:4.12 -->
### Integration contracts: Provider Test Doubles (normative)

**Contract**: `cpt-cf-bss-rating-algo-test-doubles-cc` (`p2`), defined in [§3 migrated procedures](#register-procedures).

Until providers exist, Rating ships ClientHub-registered fakes behind a `test-providers` feature,
each driven by the shared fixture vectors (Atlas exit criterion: one provider-side and one
consumer-side test per contract with the same fixture ids):

| Fake | Implements | Fixtures |
|---|---|---|
| `FakePricingCatalog` | `PricingReadV1::{resolve, price, current_revision}` over pricing's golden contract files (`pricing/tests/contract/*.json`) and seam fixtures (`pricing/tests/seam_fixtures/*.json`): signup and renewal bindings, uncovered cells, `ends_on`, refusals (T-D-73) | pricing goldens, F23–F25 band tables |
| `FakeDerivedUsageTypes` | the requested Products declaration read (R-31) over `bss_products_sdk::derived` declarations | cloudlet seam fixtures |
| `FakeUsageFeed` | the collector's documented `read_usage_feed` (cursor, `Oldest`, `until`, invalidation entries, `CursorBeyondRetention`) | F02, F03, F06, F26, F30 |
| `FakeSubscriptionsBilling` | Atlas C04 events and reads | F04, F17, F18, F28, F33, F35 |
| `FakeCoverage` | coverage declarations / inventory | F05, F06, F30 |
| `RecordingBilling` | consumes `BillableItemDeliveryV1`, applies C08 rules (stale revision ignored, equal-revision conflict quarantined) | F14, F15, F27, F32, V16 |
| `FakeClusterLocks` | `DistributedLockApi` / `LeaderElectionApi` with fence bumps and forced expiry | V09, zombie-writer cases |
| `FakeCollectorReconciliation` | per `(tenant, GTS type, day)` counts and sums for source-loss repair | V05, V06 |

The fakes also drive the state-transition and concurrency vectors of DESIGN §4.13 (V01–V49; V17 retired; the Billing golden rows of the money vectors run against a Billing test double until Billing exists); no
vector depends on an upstream contract being implemented. Fakes never stand in for a production
contract (DESIGN §4.10).

<!-- /contract -->
