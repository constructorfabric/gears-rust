<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Foundation (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Foundation

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-foundation-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-foundation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Execute a scoped mutation](#execute-a-scoped-mutation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [conditional-store](#conditional-store)
  - [replay-and-audit](#replay-and-audit)
  - [toolkit-outbox](#toolkit-outbox)
- [4. States (CDSL)](#4-states-cdsl)
  - [Foundation states](#foundation-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Fresh schema on both backends](#fresh-schema-on-both-backends)
  - [Scoped repositories](#scoped-repositories)
  - [Append-only audit](#append-only-audit)
  - [One replay store](#one-replay-store)
  - [Conditional object versions](#conditional-object-versions)
  - [Conditional approval Store](#conditional-approval-store)
  - [Atomic event writes](#atomic-event-writes)
  - [Toolkit delivery integration](#toolkit-delivery-integration)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 2c. Every checkbox is an implementation obligation, not an assertion about the legacy code.

This feature implements [slice 01](../design/01-foundation.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Provide the fresh schema, scoped repositories, conditional approval Store, canonical errors, replay, audit and live toolkit outbox infrastructure.

Requirements: `cpt-cf-bss-pricing-nfr-authz`, `cpt-cf-bss-pricing-nfr-audit`, `cpt-cf-bss-pricing-nfr-tenant-isolation`, `cpt-cf-bss-pricing-nfr-two-backends`, `cpt-cf-bss-pricing-nfr-idempotency-concurrency`.

Architecture: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-auditor`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 01](../design/01-foundation.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-433; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

## 2. Actor Flows (CDSL)

### Execute a scoped mutation

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-foundation`

1. [ ] - `p1` - Authenticate and derive AccessScope from PolicyEnforcer; require correlation and the operation preconditions. - `inst-foundation-flow-1`
2. [ ] - `p1` - Resolve client-key replay before any reservation or approval work; reject payload mismatch. - `inst-foundation-flow-2`
3. [ ] - `p1` - Run the owning domain mutation in a bounded transaction retry, cloning per-attempt inputs and retaining typed database errors. - `inst-foundation-flow-3`
4. [ ] - `p1` - Claim and answer replay with domain state, audit and outbox in the same transaction; return the new ETag after commit. - `inst-foundation-flow-4`
5. [ ] - `p1` - On ambiguous commit, reconcile the stored answer/object before compensating a reference; on definite failure roll back the attempt. - `inst-foundation-flow-5`

## 3. Processes / Business Logic (CDSL)

### conditional-store

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-foundation-conditional-store`

1. [ ] - `p1` - Load tenant-scoped unit and item versions. - `inst-foundation-conditional-store-1`
2. [ ] - `p1` - Acquire pending ownership only where pending_unit_id is null and the observed version matches; zero affected rows is PRICE_LOCKED_PENDING. - `inst-foundation-conditional-store-2`
3. [ ] - `p1` - Bump unit version conditionally; a lost race returns UNIT_CONTENDED without a second decision or apply. - `inst-foundation-conditional-store-3`
4. [ ] - `p1` - Clear locks only for the owning unit, following unit, entry-id, price-id, revision, promotion order. - `inst-foundation-conditional-store-4`

### replay-and-audit

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-foundation-replay-and-audit`

1. [ ] - `p1` - Look up tenant, concrete endpoint and client_key within the 24-hour retention and compare payload hash. - `inst-foundation-replay-and-audit-1`
2. [ ] - `p1` - Claim the key inside the mutation transaction; simultaneous claims cannot both mutate. - `inst-foundation-replay-and-audit-2`
3. [ ] - `p1` - Persist immutable attributed audit and response status/body with state; a committed UNIT_STALE refresh has a committed replay receipt too. - `inst-foundation-replay-and-audit-3`
4. [ ] - `p1` - Rollback leaves no partial claim, state, audit or success event. - `inst-foundation-replay-and-audit-4`

### toolkit-outbox

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-foundation-toolkit-outbox`

1. [ ] - `p1` - Install toolkit outbox migrations under bss_pricing_outbox in DatabaseCapability. - `inst-foundation-toolkit-outbox-1`
2. [ ] - `p1` - Encode domain payloads through TypedEvent using the Products envelope sink pattern. - `inst-foundation-toolkit-outbox-2`
3. [ ] - `p1` - Append through the caller transaction and dispatch only committed outbox rows. - `inst-foundation-toolkit-outbox-3`
4. [ ] - `p1` - Wire lifecycle-managed delivery and retry; failure leaves a durable record rather than an unrelayed pricing_outbox row. - `inst-foundation-toolkit-outbox-4`

## 4. States (CDSL)

### Foundation states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-foundation`

Replay moves absent → claimed → answered within the operation transaction, and rollback restores absence. Audit moves unsealed → sealed only through the record-preserving guard. Conditional version v → v+1 admits one writer. Outbox delivery never precedes commit.

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`.

### Fresh schema on both backends

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-tables-two-backends`

The new migration chain preserves keys, checks and chain indexes on SQLite and Postgres. Schema goldens pin the resulting shape, which DESIGN §3.7 states (the approval tables as bss_approval::ddl writes them), and two-writer integration tests verify it, without migrating deployed data.

Requirement: `cpt-cf-bss-pricing-nfr-two-backends`; PRD AC #24.

### Scoped repositories

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-scoped-repositories`

Repositories use the caller DBRunner and PolicyEnforcer-derived tenant scope. Approval children require the scoped unit; a raw or second connection cannot bypass the mutation transaction (spec §3 item 27).

Requirement: `cpt-cf-bss-pricing-nfr-tenant-isolation`; PRD AC #23.

### Append-only audit

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-audit-append-only`

Submission, terminal decisions and reference-loss acts persist attributed audit in the same transaction. Storage rejects deletion and record edits while allowing only the reserved seal transition (spec §3 item 27, §6).

Requirement: `cpt-cf-bss-pricing-nfr-audit`; PRD AC #22.

### One replay store

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-idempotency-key-store`

Required POST keys use tenant/endpoint/client_key, payload hash and 24-hour retention. Claims and saved answers share the mutation transaction, including committed UNIT_STALE responses; units have no second key (spec §2.2).

Requirement: `cpt-cf-bss-pricing-nfr-idempotency-concurrency`; PRD AC #25.

### Conditional object versions

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-if-match-version`

PATCH/PUT require the version in If-Match and return the updated ETag. The conditional write preserves state on stale tokens and is independent of price version_no or approval generation (spec §3 item 23).

Requirement: `cpt-cf-bss-pricing-nfr-idempotency-concurrency`; PRD AC #25.

### Conditional approval Store

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-unit-store`

The Store retains typed DbErr, conditional versions, ownership guards and generation-scoped decisions. Repository races are tested with two actual connections on both engines, without FOR UPDATE (spec §2.2, §6).

Requirement: `cpt-cf-bss-pricing-nfr-two-backends`; PRD AC #24.

### Atomic event writes

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-outbox-same-tx`

State, audit and required events commit together through the caller transaction. A failing outbox insert prevents successful state publication (spec §6–§7.3).

Requirement: `cpt-cf-bss-pricing-nfr-audit`; PRD AC #22.

### Toolkit delivery integration

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-outbox-toolkit`

Toolkit migrations and a lifecycle-managed dispatcher carry broker TypedEvent envelopes using the Products pattern. The old gear-authored pricing_outbox without a relay is retired by D-400.

Requirement: `cpt-cf-bss-pricing-nfr-audit`; PRD AC #22.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-tables-two-backends` | AC #24; `cpt-cf-bss-pricing-nfr-two-backends` | Given empty SQLite and Postgres databases, when the chain runs twice then both schemas remain correct; duplicate book/entry/approved-start keys are refused. |
| `cpt-cf-bss-pricing-dod-scoped-repositories` | AC #23; `cpt-cf-bss-pricing-nfr-tenant-isolation` | Given two tenants and a foreign child id, when a scoped repository reads or writes then no other tenant data is exposed or changed. |
| `cpt-cf-bss-pricing-dod-audit-append-only` | AC #22; `cpt-cf-bss-pricing-nfr-audit` | Given a successful act, when audit is read then actor/subject/correlation are present; an injected audit failure rolls back the act and direct deletion fails. |
| `cpt-cf-bss-pricing-dod-idempotency-key-store` | AC #25; `cpt-cf-bss-pricing-nfr-idempotency-concurrency` | Given a retained key, when the same request repeats then its response replays without mutation; another payload conflicts and simultaneous claims produce one act. |
| `cpt-cf-bss-pricing-dod-if-match-version` | AC #25; `cpt-cf-bss-pricing-nfr-idempotency-concurrency` | Given a current ETag, when one update succeeds then its successor token changes; a second update using the old token returns STALE_REVISION and omission fails preconditions. |
| `cpt-cf-bss-pricing-dod-unit-store` | AC #24; `cpt-cf-bss-pricing-nfr-two-backends` | Given two writers with one observed unit version, when they contend then only one applies; the loser cannot leave a partial vote or terminal audit. |
| `cpt-cf-bss-pricing-dod-outbox-same-tx` | AC #22; `cpt-cf-bss-pricing-nfr-audit` | Given an approval needing events, when outbox insertion fails then state and audit roll back; success persists all three. |
| `cpt-cf-bss-pricing-dod-outbox-toolkit` | AC #22; `cpt-cf-bss-pricing-nfr-audit` | Given a committed event and interrupted dispatch, when delivery resumes then its durable envelope is retried; an uncommitted transaction produces no delivery. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.
