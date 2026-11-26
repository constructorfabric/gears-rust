<!-- Created: 2026-11-18 by Constructor Tech -->

# Feature: Quota Resolver Plugin

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-featstatus-resolver-plugin-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-quota-enforcement-feature-resolver-plugin`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Plugin Operation](#plugin-operation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Quota State for the Resolver](#quota-state-for-the-resolver)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Plugin Registration and Trusted Operations](#plugin-registration-and-trusted-operations)
  - [Quota State Projection](#quota-state-projection)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context (optional)](#7-additional-context-optional)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Makes QE the backend of the quota resolver, the platform module through which other modules charge and read quotas.
QE implements the resolver's plugin contract `QuotaResolverPluginClient` inside its own gear, registers it as a quota
resolver plugin instance, and runs each call through the QE operations built over trusted admission.

### 1.2 Purpose

The resolver calls its plugin in process with a fixed tenant, fixed quota subjects, and no `SecurityContext`, and runs
no policy check of its own. The plugin runs the same debit, lease, return, and snapshot operations as QE's API, so
replay, evaluation, and storage behave the same on both paths, while QE's own API keeps its PDP check
([ADR-0009](../ADR/0009-cpt-cf-quota-enforcement-adr-resolver-plugin-path.md)).

**Scope**: plugin registration; conversion of resolver requests to QE requests and of QE results and errors to
resolver results; operations without a `SecurityContext`; quota state served from the snapshot read without the fields
only operators see.

**Out of scope**: trusted admission itself (foundation feature); return and the charge ledger (consumption-operations
feature); several metrics in one operation (`cpt-cf-quota-resolver-fr-multi-metric`, P2 in the resolver).

**Requirements**: `cpt-cf-quota-enforcement-fr-resolver-plugin`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-quota-enforcement-actor-quota-resolver` | Calls the plugin in process for debit, lease, return, and quota state |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) (`cpt-cf-quota-enforcement-component-resolver-plugin`)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) (§2.12)
- **ADR**: [ADR-0008 Quota resolver subjects](../ADR/0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md),
  [ADR-0009 Resolver plugin path](../ADR/0009-cpt-cf-quota-enforcement-adr-resolver-plugin-path.md),
  [ADR-0012 Metric catalogue](../ADR/0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md),
  [ADR-0013 Metric types](../ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md); quota resolver
  [ADR-0004 Metric types](../../../quota-resolver/docs/ADR/0004-cpt-cf-quota-resolver-adr-metric-types.md)
- **Quota resolver**: [DESIGN](../../../quota-resolver/docs/DESIGN.md) (§3.1 Domain Model, §3.2 Backend Plugin)
- **Dependencies**: `cpt-cf-quota-enforcement-feature-lease-operations`,
  `cpt-cf-quota-enforcement-feature-snapshot-reads`, plus transitively
  `cpt-cf-quota-enforcement-feature-consumption-operations` and `cpt-cf-quota-enforcement-feature-foundation`

## 2. Actor Flows (CDSL)

### Plugin Operation

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-resolver-plugin-call`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-resolver`

**Success Scenarios**:
- A debit or lease acquisition returns the QE Decision as the resolver's decision or lease outcome
- A commit, release, or return returns no result

**Error Scenarios**:
- Every QE canonical error, including `InvalidArgument` for a debit or lease whose subject type the metric does not
  admit, is returned as a plugin error; nothing is charged
- A debit, lease acquisition, commit, or return with more than one metric item (a metric and its amount):
  `InvalidArgument`
- A commit that names a metric type other than the lease's, or a return that names a metric type other than the
  charge's: QE's `METRIC_TYPE_MISMATCH` (`InvalidArgument`), passed to the resolver unchanged
- A lease token of another tenant is not found, because trusted admission scopes storage to the request's tenant

**Steps**:
1. [x] - `p1` - The resolver calls `debit`, `acquire_lease`, `commit_lease`, `release_lease`, `return_amount`, or `quota_state` with the tenant, the quota subjects `{type, id}`, and the rest of the request - `inst-rpl-request`
2. [x] - `p1` - Convert the request: a resolver request carries **metric items**, each a metric object `{type, id?, metadata}` and an amount; `type` is the **metric type** (the GTS type that the metric's owner registers), `id` is optional and names the one thing the amount is counted for, and `metadata` holds the attributes of the operation that policies read (ADR-0013); a QE operation has one metric, so for a debit and a lease acquisition the plugin passes the metric object of the single item as the QE request's `metric` and the item's amount as its amount; a commit and a return carry no metric object: each carries amounts named by metric type id, and the plugin passes the single amount with its metric type id to the QE commit or return of the lease or charge that the request names, where QE compares the id with the metric type of the lease or charge; subjects, lease duration, and idempotency keys pass unchanged, since QE and the resolver share the subject types and the metric types; a commit without an amount commits the full held amount; a request with no metric item, with more than one, or with an amount list that is present but does not hold exactly one amount is `InvalidArgument` and nothing runs - `inst-rpl-convert`
3. [x] - `p1` - Run the QE operation built over trusted admission (`cpt-cf-quota-enforcement-flow-authorized-admission`) without a `SecurityContext` (the plugin receives none and builds none); the operation log records as the actor the platform identity of the resolver's caller, which the request carries and QE does not authorize by - `inst-rpl-run`
4. [x] - `p1` - Convert the result: a Decision becomes the resolver's decision (allowed or denied, the denial reason, diagnostics); the resolver's reason is one of its closed set: a QE denial with the reason `NO_APPLICABLE_QUOTA` becomes `no_applicable_quota`, and every other QE denial becomes `quota_exceeded`; QE's own reason text, which a `cel` Policy can write, and the ids of the violated Quotas go into the diagnostics; `AcquireLeaseOutcome::Acquired { token, expires_at }` becomes the lease token and expiry; a quota state page follows `cpt-cf-quota-enforcement-algo-resolver-quota-state` - `inst-rpl-result`
5. [x] - `p1` - **RETURN** the result, or the canonical error as a plugin error. Its category and reason are unchanged; a field it names is named as the resolver sent it (`items[0].metric.type`, `amounts[0].metric_type`, `metrics[i].type`), and a rejection for a subject names the subject field of the request, as the resolver's plugin contract requires: a field under `subjects` for a debit or a lease acquisition, under `subject` for a quota state read. The resolver's callers retry `ServiceUnavailable` and `Aborted`, and QE uses `Aborted` only when a retry with the same key can pass (lock contention); a key that is in use with another payload is `AlreadyExists` (`IDEMPOTENCY_PAYLOAD_MISMATCH`) - `inst-rpl-return`

## 3. Processes / Business Logic (CDSL)

### Quota State for the Resolver

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-resolver-quota-state`

**Input**: the tenant, one quota subject, the metrics (each a metric type id with optional request `metadata`), and paging

**Output**: one page of the resolver's quota state

**Steps**:
1. [x] - `p1` - Run the snapshot read for the subject and each metric in trusted mode (`cpt-cf-quota-enforcement-flow-snapshot-read`), passing the request `metadata` of a metric when the resolver's request carries it, so that only the Quotas that apply to such a request are read; paging passes through, and the bounds of the snapshot read apply: more metrics than its configured maximum of targets, or a page size above its configured page size, is `InvalidArgument` (the resolver sends 1 to 100 metrics, so an operator who lowers the maximum of targets below 100 refuses requests that the resolver accepts); a metric that does not admit the subject's type yields only the tenant's Quotas, not an error, and Quotas outside their validity window are left out; a metric that QE's catalogue of metric types holds as unavailable fails the read with canonical `FailedPrecondition` (ADR-0012) - `inst-rqs-read`
2. [x] - `p1` - Map each Quota Snapshot to the resolver's quota state: quota id, subject, metric, cap or none, consumed and remaining amounts, and, for a consumption Quota, the current period with its reset time - `inst-rqs-map`
3. [x] - `p1` - Leave out the enforcement mode and the validity window: the resolver's tenant plane shows quota state to any authenticated end user, and these fields are for operators. Return the Quota metadata, absent when it is empty: its conditions say which requests the Quota applies to - `inst-rqs-omit`
4. [x] - `p1` - **RETURN** the page - `inst-rqs-return`

## 4. States (CDSL)

Not applicable: the plugin keeps no state; leases and charges keep their states in the features that own them.

## 5. Definitions of Done

### Plugin Registration and Trusted Operations

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-resolver-plugin`

The system **MUST** implement `QuotaResolverPluginClient` in the QE gear and register it in ClientHub and
types-registry as an instance of the quota resolver plugin type
`gts.cf.toolkit.plugins.plugin.v1~cf.core.quota_resolver.plugin.v1~`. Each call **MUST** run the QE operation built
over trusted admission, without a `SecurityContext`. The trusted operations **MUST** be reachable only
through the plugin client, and QE's REST and in-process API **MUST** keep PDP admission. The trust boundary is the
process: any module in the same process can obtain the plugin client and is trusted like a caller of the resolver's
platform plane (its API for trusted platform services). One request never spans tenants; the caller names the tenant.

**Implements**:
- `cpt-cf-quota-enforcement-flow-resolver-plugin-call`

**Constraints**: `cpt-cf-quota-enforcement-constraint-toolkit`

**Touches**:
- API: `QuotaResolverPluginClient` (quota resolver SDK)
- Entities: none of its own

### Quota State Projection

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-resolver-quota-state`

The system **MUST** serve the resolver's quota state from the snapshot read and **MUST NOT** return the
enforcement mode or the validity window. It **MUST** return the Quota metadata.

**Implements**:
- `cpt-cf-quota-enforcement-algo-resolver-quota-state`

**Constraints**: `cpt-cf-quota-enforcement-constraint-security-context`

**Touches**:
- API: `QuotaResolverPluginClient::quota_state`
- Entities: `QuotaSnapshot`

## 6. Acceptance Criteria

- [ ] The resolver finds the QE plugin through types-registry and ClientHub and reaches no other QE entry point
- [ ] A debit, lease acquisition, commit, release, and return through the plugin make no PDP call, reach only the
  request's tenant, and record the platform identity of the resolver's caller as the actor in the operation log; the
  same operations through QE's API still call the PDP
- [ ] A lease acquisition through the plugin returns the token and its expiry; a commit without an amount commits the
  held amount
- [ ] A quota state page carries the Quota metadata and no enforcement mode or validity window
- [ ] On a debit or lease, a subject type the metric does not admit, and two subjects of one type, return
  `InvalidArgument` as a plugin error
- [ ] A quota state read for a subject type the metric does not admit returns the tenant's Quotas only; Quotas outside
  their validity window are not returned
- [ ] A debit and a lease acquisition pass the metric object and the amount of the single metric item of the
  request, unchanged; a commit and a return pass the single amount of the request with its metric type
  id; a request with several metric items or amounts returns `InvalidArgument`; a commit for a metric type other
  than the lease's, or a return for a metric type other than the charge's, returns QE's `METRIC_TYPE_MISMATCH`
- [ ] A QE denial with `NO_APPLICABLE_QUOTA` reaches the resolver's caller as `no_applicable_quota`, and every other
  QE denial as `quota_exceeded`, with QE's reason text and the violated Quota ids in the diagnostics

## 7. Additional Context (optional)

- **Test layering**: request and result conversion get unit tests; the trusted path, tenant confinement, and the
  quota state projection get integration tests through the resolver's plugin contract.
- **Non-applicable review domains**: UX/accessibility is not applicable; there is no user-facing surface of its own.
