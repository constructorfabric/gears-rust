# Feature: Tenant Resolver Plugin

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Get Tenant](#get-tenant)
  - [Get Root Tenant](#get-root-tenant)
  - [Get Tenants](#get-tenants)
  - [Get Ancestors](#get-ancestors)
  - [Get Descendants](#get-descendants)
  - [Is Ancestor](#is-ancestor)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Barrier Predicate Construction](#barrier-predicate-construction)
  - [Provisioning Invisibility Filter](#provisioning-invisibility-filter)
  - [Tenant Type Reverse-Lookup](#tenant-type-reverse-lookup)
  - [Descendant Bounded Pre-Order](#descendant-bounded-pre-order)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [SDK Method Contract Surface](#sdk-method-contract-surface)
  - [Barrier-as-Data Single Predicate](#barrier-as-data-single-predicate)
  - [Provisioning Row Invisibility](#provisioning-row-invisibility)
  - [Read-Only Database Role Enforcement](#read-only-database-role-enforcement)
  - [No Wire API Exposure](#no-wire-api-exposure)
  - [No Process-Local Hierarchy Cache](#no-process-local-hierarchy-cache)
  - [Deterministic Query-Time Ordering](#deterministic-query-time-ordering)
  - [ClientHub Registration via GTS Scope](#clienthub-registration-via-gts-scope)
  - [Closure-Consistency Inheritance](#closure-consistency-inheritance)
  - [SecurityContext Pass-Through](#securitycontext-pass-through)
  - [Observability Surface Coverage](#observability-surface-coverage)
  - [Bounded Reverse-Lookup Cache](#bounded-reverse-lookup-cache)
  - [Error-Taxonomy Delegation](#error-taxonomy-delegation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-featstatus-tenant-resolver-plugin`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-tr-plugin-feature-tenant-resolver-plugin`

## 1. Feature Context

### 1.1 Overview

The Tenant Resolver Plugin (TRP) is the single, read-only, in-process implementation of the `TenantResolverPluginClient` SDK trait behind the Tenant Resolver gateway, exposing six hot-path hierarchy SDK methods — `get_tenant`, `get_root_tenant`, `get_tenants`, `get_ancestors`, `get_descendants`, `is_ancestor` — over the parent account-management module's canonical `tenants` + `tenant_closure` storage via a dedicated read-only database role. Barrier semantics are answered by a single-predicate lookup on the AM-owned `tenant_closure.barrier` column (barrier-as-data), and the internal `provisioning` tenant status is structurally invisible to every SDK response regardless of caller-supplied status filters. Closure ownership is asymmetric: AM writes the canonical `(tenants, tenant_closure)` pair transactionally; the plugin holds only `SELECT` grants and never mutates any AM-owned object.

### 1.2 Purpose

This feature realizes the sole feature entry in the `cf-tr-plugin` sub-system DECOMPOSITION (§2.1) as a pure query facade: it carries every SDK-visible hierarchy read off AM's write path, enforces barrier and status semantics as canonical SQL predicates against AM-owned rows, and inherits consistency, versioning, and visibility guarantees transactionally from AM's writer. The purpose is to give hot-path authorization traffic (ancestor chains, subtree membership, root discovery) a deterministic, side-effect-free read interface with single-digit-millisecond latency, while preserving the closure-ownership boundary documented in ADR-001 (AM owns every write to `tenants`, `tenant_closure`, `barrier`, and `descendant_status`; the plugin reads and projects). Keeping the plugin stateless on the hierarchy surface — the only allowed cache is a bounded lazy reverse-lookup from `tenant_type_uuid` to the public chained `tenant_type` — keeps correctness auditable by construction: a respected-barrier leak or a stale hierarchy row can only be a property of AM's canonical data, never of a plugin cache or a plugin-local recomputation.

**Requirements**: `cpt-cf-tr-plugin-fr-plugin-api`, `cpt-cf-tr-plugin-fr-get-tenant`, `cpt-cf-tr-plugin-fr-get-root-tenant`, `cpt-cf-tr-plugin-fr-get-tenants`, `cpt-cf-tr-plugin-fr-get-ancestors`, `cpt-cf-tr-plugin-fr-get-descendants`, `cpt-cf-tr-plugin-fr-is-ancestor`, `cpt-cf-tr-plugin-fr-barrier-semantics`, `cpt-cf-tr-plugin-fr-status-filtering`, `cpt-cf-tr-plugin-fr-provisioning-invisibility`, `cpt-cf-tr-plugin-fr-observability`, `cpt-cf-tr-plugin-nfr-query-latency`, `cpt-cf-tr-plugin-nfr-subtree-latency`, `cpt-cf-tr-plugin-nfr-closure-consistency`, `cpt-cf-tr-plugin-nfr-tenant-isolation`, `cpt-cf-tr-plugin-nfr-audit-trail`, `cpt-cf-tr-plugin-nfr-observability`

**Principles**: `cpt-cf-tr-plugin-principle-query-facade`, `cpt-cf-tr-plugin-principle-sdk-source-of-truth`, `cpt-cf-tr-plugin-principle-barrier-as-data`, `cpt-cf-tr-plugin-principle-single-store`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-tr-plugin-actor-tenant-resolver-gateway` | In-process delegator that receives every platform call on the Tenant Resolver SDK and routes it to this plugin via `ClientHub`; the sole direct caller of every SDK method in §2. |
| `cpt-cf-tr-plugin-actor-authz-resolver` | Upstream AuthZ Resolver plugin that drives the hot-path traffic for `get_ancestors`, `get_descendants`, and `is_ancestor` during policy evaluation; reaches the plugin transitively through the gateway. |
| `cpt-cf-tr-plugin-actor-pep` | Policy Enforcement Point consumer of subtree-membership reads for query compilation; reaches the plugin transitively through the gateway when the deployment does not read AM's `tenant_closure` directly. |
| `cpt-cf-tr-plugin-actor-account-management` | Source-of-truth tenant service; owns every write to `tenants`, `tenant_closure`, `barrier`, and `descendant_status`. Not a caller of this feature — its role is upstream writer whose transactional guarantees the plugin inherits. |
| `cpt-cf-tr-plugin-actor-operator` | Platform Operator who provisions and rotates the plugin's read-only database role, sizes the connection pool, and owns observability thresholds; triggers startup-time and CI-time privilege assertions rather than SDK calls. |
| `cpt-cf-tr-plugin-actor-platform-telemetry` | Consumer of OpenTelemetry metrics, traces, and structured logs emitted by every SDK call; receives output only and never invokes SDK methods. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §2 Actors (actor roster); §3.1 Core Boundary (gateway-delegated in-process boundary); §4.1 In Scope / §4.2 Out of Scope; §5.1 SDK Contract Implementation (`fr-plugin-api`, `fr-get-tenant`, `fr-get-root-tenant`, `fr-get-tenants`, `fr-get-ancestors`, `fr-get-descendants`, `fr-is-ancestor`); §5.2 Barrier and Status Semantics (`fr-barrier-semantics`, `fr-status-filtering`, `fr-provisioning-invisibility`); §5.3 Observability (`fr-observability`); §6.1 Query Latency (`nfr-query-latency`); §6.2 Subtree Query Latency (`nfr-subtree-latency`); §6.3 Closure Consistency (`nfr-closure-consistency`); §6.4 Tenant Isolation (`nfr-tenant-isolation`); §6.5 Audit Trail (`nfr-audit-trail`); §6.6 Observability Coverage (`nfr-observability`); §7.2 External Integration Contracts (`contract-am-read-only-role`, `contract-types-registry-reverse-lookup`); §8 Use Cases (`usecase-get-root-tenant`, `usecase-get-tenant`, `usecase-ancestor-query`, `usecase-descendant-query`, `usecase-is-ancestor`, `usecase-barrier-respect`).
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 Design Principles (`principle-query-facade`, `principle-sdk-source-of-truth`, `principle-barrier-as-data`, `principle-single-store`); §2.2 Constraints (`constraint-am-storage-only`, `constraint-read-only-role`, `constraint-no-am-client`, `constraint-security-context-passthrough`, `constraint-no-wire-api`, `constraint-versioning-policy`, `constraint-scope-exclusions`); §3.2 Component Model — PluginImpl (`component-plugin-impl`); §3.3 API Contracts (`interface-plugin-client` — SDK trait, `interface-plugin-client-contract` — ClientHub/gateway wiring, `interface-am-schema`); §3.6 Interactions & Sequences (`seq-get-tenant`, `seq-get-root-tenant`, `seq-ancestor-query`, `seq-descendant-query`, `seq-is-ancestor`); §3.7 Database Schemas & Tables — read-only index coverage reference (`db-schema`, no plugin-owned DDL); §3.8 Error Codes Reference (SDK-owned `TenantResolverError::TenantNotFound` / `TenantResolverError::Internal`).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.1 Tenant Resolver Plugin (this feature's sole scope block).
- **ADR**: [ADR-001 — Tenant Hierarchy Closure Ownership](../ADR/ADR-001-tenant-hierarchy-closure-ownership.md) (`cpt-cf-tr-plugin-adr-p1-tenant-hierarchy-closure-ownership`) — closure-ownership decision that anchors the read-only plugin boundary.
- **AM-side integration anchors** (consumed read-only; defined upstream): `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`, and AM's closure-maintenance algorithm (transactional `tenants` + `tenant_closure` writes that maintain `barrier` and `descendant_status`) under `cpt-cf-account-management-feature-tenant-hierarchy-management`.
- **Cross-system NFR implemented here**: `cpt-cf-account-management-nfr-context-validation-latency` — the hot-path context-validation latency SLO (authoritative definition lives in the parent account-management system per PRD §6.1; implemented and measured by this feature's reads over AM storage per DECOMPOSITION §2.1, Phase 2 feature-map §3.1 Option-B redistribution).
- **Dependencies**:
  - Hard — `cpt-cf-account-management-feature-tenant-hierarchy-management` (authoritative owner of `tenants`, `tenant_closure`, and the denormalized `barrier` + `descendant_status` columns; every SDK read in §2 projects rows from this feature's canonical storage).
  - Informational upstream — `cpt-cf-account-management-feature-managed-self-managed-modes` (source of truth for the semantics encoded in the `barrier` column; not read directly by this feature).
  - Informational upstream — `cpt-cf-account-management-feature-errors-observability` (error taxonomy and telemetry conventions inherited by this feature; canonical sub-codes are referenced by name only, with envelope and transport mapping delegated).

## 2. Actor Flows (CDSL)

### Get Tenant

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `TenantResolverPluginClient::get_tenant(tenant_id)` via `ClientHub` and the plugin returns a `TenantInfo` for any `tenants` row in `active`, `suspended`, or `deleted` status; provisioning rows are invisible by construction per DESIGN §3.6 `cpt-cf-tr-plugin-seq-get-tenant`. Ordering and barrier mode do not apply.

**Error Scenarios**:

- Tenant identifier absent from `tenants` or the matched row is provisioning — plugin returns the canonical `not_found` sub-code (mapped by `cpt-cf-account-management-feature-errors-observability` to `TenantResolverError::TenantNotFound`).
- Database connection failure, query timeout, or tenant-type reverse-hydration failure — plugin returns the canonical `service_unavailable` sub-code (mapped to `TenantResolverError::Internal`); the gateway decides retry.

**Steps**:

1. [ ] - `p1` - Receive the `get_tenant(tenant_id)` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span per `constraint-security-context-passthrough` - `inst-flow-get-tenant-receive`
2. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with an absent caller `status_filter` to obtain the effective status-visibility predicate - `inst-flow-get-tenant-provisioning`
3. [ ] - `p1` - Query AM `tenants` by primary key via the dedicated read-only database role, applying the effective provisioning-invisibility predicate - `inst-flow-get-tenant-lookup`
4. [ ] - `p1` - **IF** no row is returned (absent or provisioning) - `inst-flow-get-tenant-absent-branch`
   1. [ ] - `p1` - **RETURN** the canonical `not_found` sub-code to the gateway - `inst-flow-get-tenant-return-not-found`
5. [ ] - `p1` - **ELSE** row is returned - `inst-flow-get-tenant-hit-branch`
   1. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-tenant-type-reverse-lookup` with the row's `tenant_type_uuid` to resolve the public chained `tenant_type` identifier - `inst-flow-get-tenant-hydrate-type`
   2. [ ] - `p1` - Project the AM row onto `TenantInfo` and **RETURN** it to the gateway - `inst-flow-get-tenant-return-info`
6. [ ] - `p1` - **IF** any read step raised a transient DB or Types Registry failure - `inst-flow-get-tenant-error-branch`
   1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-tenant-return-unavailable`

### Get Root Tenant

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `get_root_tenant()` and the plugin returns the unique non-provisioning root tenant (the single row where `parent_id` is null and the provisioning filter admits the row) as `TenantInfo`, per DESIGN §3.6 `cpt-cf-tr-plugin-seq-get-root-tenant`.

**Error Scenarios**:

- No non-provisioning root present (including the bootstrap window when the sole root candidate is still provisioning) or multiple root rows present — plugin returns the canonical `service_unavailable` sub-code (mapped to `TenantResolverError::Internal`); the single-root invariant is not recoverable by caller retry alone.
- Database connection failure or query timeout — plugin returns the canonical `service_unavailable` sub-code.

**Steps**:

1. [ ] - `p1` - Receive the `get_root_tenant()` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span - `inst-flow-get-root-tenant-receive`
2. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with an absent caller `status_filter` to obtain the effective provisioning-exclusion predicate for the root candidate - `inst-flow-get-root-tenant-provisioning`
3. [ ] - `p1` - Query AM `tenants` for the unique root candidate (root-marker predicate `parent_id is null`) via the read-only role, applying the effective provisioning-exclusion predicate - `inst-flow-get-root-tenant-lookup`
4. [ ] - `p1` - **IF** no row is returned (no non-provisioning root present, including bootstrap window) - `inst-flow-get-root-tenant-none-branch`
   1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-root-tenant-return-internal-none`
5. [ ] - `p1` - **ELSE IF** more than one root candidate is returned (single-root invariant violated) - `inst-flow-get-root-tenant-multiple-branch`
   1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-root-tenant-return-internal-multiple`
6. [ ] - `p1` - **ELSE** exactly one non-provisioning root row is returned - `inst-flow-get-root-tenant-unique-branch`
   1. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-tenant-type-reverse-lookup` with the row's `tenant_type_uuid` - `inst-flow-get-root-tenant-hydrate-type`
   2. [ ] - `p1` - Project the AM row onto `TenantInfo` and **RETURN** it to the gateway - `inst-flow-get-root-tenant-return-info`
7. [ ] - `p1` - **IF** any read step raised a transient DB or Types Registry failure - `inst-flow-get-root-tenant-error-branch`
   1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-root-tenant-return-unavailable`

### Get Tenants

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `get_tenants(ids, GetTenantsOptions)`; the plugin deduplicates the identifier set, applies the caller-supplied SDK-visible status filter (empty set means all three visible statuses), drops provisioning rows unconditionally per `fr-provisioning-invisibility`, and returns a `Vec<TenantInfo>` where absent or provisioning identifiers are silently dropped. Response order is not required to match input order.

**Error Scenarios**:

- Caller supplies a malformed `GetTenantsOptions` payload (e.g., a status value outside the SDK-visible domain) — plugin returns the canonical `validation` sub-code.
- Database connection failure or query timeout — plugin returns the canonical `service_unavailable` sub-code.

**Steps**:

1. [ ] - `p1` - Receive the `get_tenants(ids, options)` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span - `inst-flow-get-tenants-receive`
2. [ ] - `p1` - **IF** the caller-supplied `options` payload is malformed (unknown status value outside the SDK-visible domain) - `inst-flow-get-tenants-validate-options`
   1. [ ] - `p1` - **RETURN** the canonical `validation` sub-code to the gateway - `inst-flow-get-tenants-return-validation`
3. [ ] - `p1` - Deduplicate the caller-supplied identifier set - `inst-flow-get-tenants-dedupe`
4. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with the caller-supplied `options.status` to obtain the effective status-filter predicate - `inst-flow-get-tenants-provisioning`
5. [ ] - `p1` - Query AM `tenants` for the deduplicated identifier set via the read-only role, applying the effective status-filter predicate - `inst-flow-get-tenants-lookup`
6. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-tenant-type-reverse-lookup` for each returned row with an uncached `tenant_type_uuid` in a single batched pass - `inst-flow-get-tenants-hydrate-types`
7. [ ] - `p1` - Project each returned AM row onto `TenantInfo`; silently drop identifiers that did not match (absent or filtered out) - `inst-flow-get-tenants-project`
8. [ ] - `p1` - **RETURN** the resulting `Vec<TenantInfo>` to the gateway - `inst-flow-get-tenants-return-vec`
9. [ ] - `p1` - **IF** any read step raised a transient DB or Types Registry failure - `inst-flow-get-tenants-error-branch`
   1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-tenants-return-unavailable`

### Get Ancestors

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `get_ancestors(tenant_id, BarrierMode)`; the plugin confirms the starting tenant is visible, then returns a `GetAncestorsResponse` whose `tenant` field hydrates the starting tenant as `TenantRef` and whose `ancestors` field lists the strict-ancestor chain in deterministic direct-parent-first order (root last), with a stable tie-break for ancestors at the same hierarchy level. Under `BarrierMode::Respect` the single-predicate barrier filter applies per `principle-barrier-as-data`; under `BarrierMode::Ignore` the full chain is returned and the bypass is recorded by telemetry. Behavior follows DESIGN §3.6 `cpt-cf-tr-plugin-seq-ancestor-query`.

**Error Scenarios**:

- Starting tenant absent from `tenants` or in provisioning status — plugin returns the canonical `not_found` sub-code.
- Database connection failure, query timeout, or tenant-type reverse-hydration failure — plugin returns the canonical `service_unavailable` sub-code.

**Steps**:

1. [ ] - `p1` - Receive the `get_ancestors(tenant_id, barrier_mode)` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span - `inst-flow-get-ancestors-receive`
2. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with an absent caller `status_filter` to obtain the effective provisioning-exclusion predicate - `inst-flow-get-ancestors-provisioning`
3. [ ] - `p1` - Probe existence of the starting tenant in AM `tenants` via the read-only role, applying the effective provisioning-exclusion predicate - `inst-flow-get-ancestors-existence`
4. [ ] - `p1` - **IF** no matching row is returned (absent or provisioning starting tenant) - `inst-flow-get-ancestors-absent-branch`
   1. [ ] - `p1` - **RETURN** the canonical `not_found` sub-code to the gateway - `inst-flow-get-ancestors-return-not-found`
5. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-barrier-predicate-construction` with the caller-supplied `barrier_mode` to obtain the barrier predicate fragment - `inst-flow-get-ancestors-barrier`
6. [ ] - `p1` - Read strict ancestors from AM `tenant_closure` joined to AM `tenants` on the ancestor side via the read-only role, applying the barrier predicate fragment and the provisioning-exclusion predicate on the joined `tenants` row, ordered by the AM-owned depth column descending with the tenant identifier as tie-break - `inst-flow-get-ancestors-read-chain`
7. [ ] - `p1` - **IF** the resolved `barrier_mode` is `BarrierMode::Ignore` - `inst-flow-get-ancestors-bypass-branch`
   1. [ ] - `p1` - Increment the barrier-bypass telemetry instrument for operator audit per `nfr-audit-trail` - `inst-flow-get-ancestors-record-bypass`
8. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-tenant-type-reverse-lookup` for each ancestor row and the starting tenant row with uncached `tenant_type_uuid` values in a single batched pass - `inst-flow-get-ancestors-hydrate-types`
9. [ ] - `p1` - Project the starting tenant onto `TenantRef` and each ancestor row onto `TenantRef`, preserving the direct-parent-first order - `inst-flow-get-ancestors-project`
10. [ ] - `p1` - **RETURN** the assembled `GetAncestorsResponse` to the gateway - `inst-flow-get-ancestors-return-response`
11. [ ] - `p1` - **IF** any read step raised a transient DB or Types Registry failure - `inst-flow-get-ancestors-error-branch`
    1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-ancestors-return-unavailable`

### Get Descendants

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `get_descendants(tenant_id, GetDescendantsOptions { barrier_mode, max_depth, status_filter })`; the plugin confirms the starting tenant is visible, then returns a `GetDescendantsResponse` whose `tenant` field hydrates the starting tenant and whose `descendants` field lists the bounded subtree in SDK pre-order (parent before that parent's descendants; siblings ordered by tenant identifier), bounded by `max_depth`. The caller-supplied `status_filter` applies to descendants only; the starting tenant is never filtered. Barrier enforcement under `BarrierMode::Respect` is a single-predicate lookup on the closure row, and `BarrierMode::Ignore` bypass is recorded by telemetry. Behavior follows DESIGN §3.6 `cpt-cf-tr-plugin-seq-descendant-query`.

**Error Scenarios**:

- Starting tenant absent from `tenants` or in provisioning status — plugin returns the canonical `not_found` sub-code.
- Caller supplies a malformed `GetDescendantsOptions` payload (e.g., negative `max_depth`, or a status value outside the SDK-visible domain) — plugin returns the canonical `validation` sub-code.
- Database connection failure, query timeout, or tenant-type reverse-hydration failure — plugin returns the canonical `service_unavailable` sub-code.

**Steps**:

1. [ ] - `p1` - Receive the `get_descendants(tenant_id, options)` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span - `inst-flow-get-descendants-receive`
2. [ ] - `p1` - **IF** the caller-supplied `options` payload is malformed (non-numeric depth bound, negative bound, or status value outside the SDK-visible domain) - `inst-flow-get-descendants-validate-options`
   1. [ ] - `p1` - **RETURN** the canonical `validation` sub-code to the gateway - `inst-flow-get-descendants-return-validation`
3. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with the caller-supplied `options.status_filter` to obtain the effective status-filter predicate, structurally excluding provisioning rows regardless of caller intent - `inst-flow-get-descendants-provisioning`
4. [ ] - `p1` - Probe existence of the starting tenant in AM `tenants` via the read-only role, applying the provisioning-exclusion predicate without applying the caller-supplied status filter to the starting tenant - `inst-flow-get-descendants-existence`
5. [ ] - `p1` - **IF** no matching row is returned (absent or provisioning starting tenant) - `inst-flow-get-descendants-absent-branch`
   1. [ ] - `p1` - **RETURN** the canonical `not_found` sub-code to the gateway - `inst-flow-get-descendants-return-not-found`
6. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-barrier-predicate-construction` with the caller-supplied `options.barrier_mode` to obtain the barrier predicate fragment - `inst-flow-get-descendants-barrier`
7. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-descendant-bounded-preorder` with the starting tenant identifier, the caller-supplied `max_depth`, the effective descendant status-filter predicate, and the barrier predicate fragment to obtain the ordered descendant projection - `inst-flow-get-descendants-invoke-preorder`
8. [ ] - `p1` - **IF** the resolved `barrier_mode` is `BarrierMode::Ignore` - `inst-flow-get-descendants-bypass-branch`
   1. [ ] - `p1` - Increment the barrier-bypass telemetry instrument for operator audit per `nfr-audit-trail` - `inst-flow-get-descendants-record-bypass`
9. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-tenant-type-reverse-lookup` for each descendant row and the starting tenant row with uncached `tenant_type_uuid` values in a single batched pass - `inst-flow-get-descendants-hydrate-types`
10. [ ] - `p1` - Project the starting tenant onto `TenantRef`, then project each descendant row onto `TenantRef` preserving the pre-order returned by the algorithm, excluding the starting tenant from the `descendants` list - `inst-flow-get-descendants-project`
11. [ ] - `p1` - **RETURN** the assembled `GetDescendantsResponse` to the gateway - `inst-flow-get-descendants-return-response`
12. [ ] - `p1` - **IF** any read step raised a transient DB or Types Registry failure - `inst-flow-get-descendants-error-branch`
    1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-get-descendants-return-unavailable`

### Is Ancestor

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Actor**: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`

**Success Scenarios**:

- Gateway invokes `is_ancestor(ancestor_id, descendant_id, BarrierMode)`; the plugin confirms both identifiers resolve to visible tenants, then returns `true` when the descendant is a strict descendant of the ancestor under the requested barrier mode and `false` otherwise. Self-reference (`ancestor_id == descendant_id`) returns `false` after the visibility check. Behavior follows DESIGN §3.6 `cpt-cf-tr-plugin-seq-is-ancestor`.

**Error Scenarios**:

- Either identifier absent from `tenants` or the matching row is in provisioning status — plugin returns the canonical `not_found` sub-code.
- Database connection failure or query timeout — plugin returns the canonical `service_unavailable` sub-code.

**Steps**:

1. [ ] - `p1` - Receive the `is_ancestor(ancestor_id, descendant_id, barrier_mode)` SDK call from the gateway via `ClientHub` and propagate the caller `SecurityContext` and OpenTelemetry trace context onto the database span - `inst-flow-is-ancestor-receive`
2. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-provisioning-invisibility-filter` with an absent caller `status_filter` to obtain the effective provisioning-exclusion predicate - `inst-flow-is-ancestor-provisioning`
3. [ ] - `p1` - Probe existence of both identifiers in AM `tenants` via the read-only role in a single batched read, applying the effective provisioning-exclusion predicate - `inst-flow-is-ancestor-existence`
4. [ ] - `p1` - **IF** either row is absent (absent or provisioning) - `inst-flow-is-ancestor-absent-branch`
   1. [ ] - `p1` - **RETURN** the canonical `not_found` sub-code to the gateway - `inst-flow-is-ancestor-return-not-found`
5. [ ] - `p1` - **IF** `ancestor_id` equals `descendant_id` (self-reference) - `inst-flow-is-ancestor-self-branch`
   1. [ ] - `p1` - **RETURN** `false` to the gateway per the SDK's strict-descendant contract - `inst-flow-is-ancestor-return-self-false`
6. [ ] - `p1` - Invoke `algo-tenant-resolver-plugin-barrier-predicate-construction` with the caller-supplied `barrier_mode` to obtain the barrier predicate fragment - `inst-flow-is-ancestor-barrier`
7. [ ] - `p1` - Probe strict-ancestor existence on AM `tenant_closure` via the read-only role for the `(ancestor_id, descendant_id)` pair, applying the barrier predicate fragment - `inst-flow-is-ancestor-closure-probe`
8. [ ] - `p1` - **IF** the resolved `barrier_mode` is `BarrierMode::Ignore` - `inst-flow-is-ancestor-bypass-branch`
   1. [ ] - `p1` - Increment the barrier-bypass telemetry instrument for operator audit per `nfr-audit-trail` - `inst-flow-is-ancestor-record-bypass`
9. [ ] - `p1` - **RETURN** the boolean result to the gateway - `inst-flow-is-ancestor-return-bool`
10. [ ] - `p1` - **IF** any read step raised a transient DB failure - `inst-flow-is-ancestor-error-branch`
    1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the gateway - `inst-flow-is-ancestor-return-unavailable`

## 3. Processes / Business Logic (CDSL)

### Barrier Predicate Construction

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-barrier-predicate-construction`

**Input**: `barrier_mode` (SDK `BarrierMode` — `Respect` or `Ignore`).

**Output**: barrier predicate fragment applied to AM `tenant_closure` reads by every barrier-aware SDK method — single-predicate form per `principle-barrier-as-data`: `BarrierMode::Respect` produces the fragment `tenant_closure.barrier = 0`; `BarrierMode::Ignore` omits the barrier predicate entirely.

**Steps**:

1. [ ] - `p1` - Receive the caller-supplied `barrier_mode` from the invoking flow step - `inst-algo-barrier-receive-mode`
2. [ ] - `p1` - **IF** `barrier_mode` is `BarrierMode::Respect` - `inst-algo-barrier-respect-branch`
   1. [ ] - `p1` - **RETURN** the single-predicate fragment that asserts the canonical `tenant_closure.barrier` column equals `0` (barrier-clear), with no additional per-row evaluation or application-layer walk - `inst-algo-barrier-return-respect`
3. [ ] - `p1` - **ELSE** `barrier_mode` is `BarrierMode::Ignore` - `inst-algo-barrier-ignore-branch`
   1. [ ] - `p1` - **RETURN** an empty fragment so the calling flow omits the barrier predicate entirely; the `BarrierMode::Ignore` bypass is recorded on the dedicated telemetry instrument by the caller per `nfr-audit-trail` - `inst-algo-barrier-return-ignore`

### Provisioning Invisibility Filter

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-provisioning-invisibility-filter`

**Input**: caller-supplied `status_filter` (optional SDK-visible `TenantStatus` set — subset of `Active` / `Suspended` / `Deleted`; may be absent, empty, or non-empty).

**Output**: effective status-filter predicate applied uniformly across every SDK method; the predicate structurally excludes `tenants.status = 'provisioning'` rows from every read, regardless of the caller-supplied input. The caller-supplied `status_filter` CANNOT re-enable provisioning rows — provisioning is not a caller-selectable status, and any attempt to include it is silently ignored at this boundary so no downstream query can observe a provisioning row.

**Steps**:

1. [ ] - `p1` - Receive the optional caller-supplied `status_filter` from the invoking flow step - `inst-algo-provisioning-receive-filter`
2. [ ] - `p1` - Discard any `provisioning` value that appears in the caller-supplied `status_filter`, because provisioning is not a caller-selectable status at the SDK boundary per `fr-provisioning-invisibility` - `inst-algo-provisioning-strip-caller-provisioning`
3. [ ] - `p1` - **IF** the caller-supplied `status_filter` is absent or empty (caller requests all SDK-visible statuses) - `inst-algo-provisioning-absent-branch`
   1. [ ] - `p1` - **RETURN** the unconditional provisioning-exclusion predicate that asserts the AM `tenants.status` column is not `provisioning` (equivalent to the SDK-visible domain `Active` ∪ `Suspended` ∪ `Deleted`) - `inst-algo-provisioning-return-exclusion`
4. [ ] - `p1` - **ELSE** the caller-supplied `status_filter` lists an explicit subset of SDK-visible statuses - `inst-algo-provisioning-explicit-branch`
   1. [ ] - `p1` - **RETURN** the conjunction of the caller's SDK-visible status set and the unconditional provisioning-exclusion predicate, so provisioning rows remain excluded even when the caller lists every SDK-visible status - `inst-algo-provisioning-return-conjunction`

### Tenant Type Reverse-Lookup

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-tenant-type-reverse-lookup`

**Input**: `tenant_type_uuid` (AM-stored UUIDv5 surrogate observed on a `tenants` row, or a batch of such UUIDs).

**Output**: public chained `tenant_type` identifier for each UUID (for example, the `gts.x.core.am.tenant_type.v1~` envelope tail), or the canonical `service_unavailable` sub-code when Types Registry cannot resolve a UUID — the plugin MUST NOT return raw UUIDs in place of the public `tenant_type` field per `contract-types-registry-reverse-lookup`.

**Steps**:

1. [ ] - `p1` - Receive the input `tenant_type_uuid` (single or batch) from the invoking flow step - `inst-algo-tenant-type-receive`
2. [ ] - `p1` - Look up each UUID in the bounded lazy process-local reverse-lookup cache keyed by `tenant_type_uuid` per DESIGN §3.2 configuration `tenant_type_cache_max_entries` - `inst-algo-tenant-type-cache-probe`
3. [ ] - `p1` - **IF** every UUID is present in cache (no misses) - `inst-algo-tenant-type-all-hit-branch`
   1. [ ] - `p1` - **RETURN** the cached public chained `tenant_type` identifiers to the caller - `inst-algo-tenant-type-return-hit`
4. [ ] - `p1` - **ELSE** one or more UUIDs are cache misses - `inst-algo-tenant-type-miss-branch`
   1. [ ] - `p1` - Resolve the missing UUIDs via Types Registry through the platform module boundary (batched resolution when more than one UUID is missing) per `contract-types-registry-reverse-lookup` - `inst-algo-tenant-type-resolve`
   2. [ ] - `p1` - **IF** Types Registry cannot resolve one or more UUIDs - `inst-algo-tenant-type-unresolved-branch`
      1. [ ] - `p1` - **RETURN** the canonical `service_unavailable` sub-code to the caller without writing a raw UUID into the SDK projection - `inst-algo-tenant-type-return-unavailable`
   3. [ ] - `p1` - Populate the bounded cache with the newly resolved mappings, evicting the oldest entries when the bound is reached - `inst-algo-tenant-type-cache-populate`
   4. [ ] - `p1` - **RETURN** the combined cache-hit and newly resolved public chained `tenant_type` identifiers to the caller - `inst-algo-tenant-type-return-combined`

### Descendant Bounded Pre-Order

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-descendant-bounded-preorder`

**Input**: `tenant_id` (starting tenant identifier, already confirmed visible by the caller), `max_depth` (optional bound on traversal depth from the starting tenant; unbounded when absent), effective `status_filter` predicate (produced by `algo-tenant-resolver-plugin-provisioning-invisibility-filter` over the caller-supplied `status_filter` and restricted to `tenant_closure.descendant_status`), barrier predicate fragment (produced by `algo-tenant-resolver-plugin-barrier-predicate-construction`).

**Output**: ordered projection of AM `tenants` rows representing the descendant subtree rooted at `tenant_id`, presented in SDK pre-order (parent before that parent's descendants; siblings ordered deterministically by tenant identifier), bounded by `max_depth`, excluding the starting tenant itself — suitable for projection onto SDK `TenantRef`.

**Steps**:

1. [ ] - `p1` - Receive the starting tenant identifier, the optional `max_depth` bound, the effective status-filter predicate, and the barrier predicate fragment from the invoking flow step - `inst-algo-preorder-receive-inputs`
2. [ ] - `p1` - Issue a single bounded recursive subtree read against AM `tenants` rooted at the starting tenant identifier via the read-only role — the walk traverses the parent-link relation in AM `tenants`, ordering siblings by the tenant identifier and bounding recursion by the supplied `max_depth` bound (unbounded when absent) - `inst-algo-preorder-walk-tenants`
3. [ ] - `p1` - Join the walk to AM `tenant_closure` on the `(ancestor_id, descendant_id)` pair anchored at the starting tenant, applying the barrier predicate fragment and the effective descendant-status predicate on the closure row in the same query — closure-driven provisioning invisibility is structural (closure contains no provisioning rows per AM's closure contract) and the defense-in-depth status-filter predicate on the joined `tenants` row prevents any stray leak - `inst-algo-preorder-join-closure`
4. [ ] - `p1` - Emit the matched rows in the SDK's documented pre-order (parent before that parent's descendants; siblings ordered by tenant identifier) directly from the single closure-join query, so the plugin performs no application-layer walk — ordering is a property of the query result, not of plugin-side post-processing - `inst-algo-preorder-emit-preorder`
5. [ ] - `p1` - Exclude the starting tenant from the emitted descendants list per SDK contract; the caller hydrates the starting tenant separately as `response.tenant` - `inst-algo-preorder-exclude-starting`
6. [ ] - `p1` - **RETURN** the ordered descendant projection to the caller for downstream `tenant_type` reverse-hydration and `TenantRef` projection - `inst-algo-preorder-return-projection`

## 4. States (CDSL)

**Not applicable.** The Tenant Resolver Plugin owns no entity, holds no lifecycle, and performs no transitions: it is a pure read-only query facade per `cpt-cf-tr-plugin-principle-query-facade` over the parent account-management module's canonical `tenants` and `tenant_closure` storage per `cpt-cf-tr-plugin-principle-single-store`, and every externally visible tenant attribute is a projection of AM's rows per `cpt-cf-tr-plugin-principle-sdk-source-of-truth`. The SDK's `BarrierMode` value (`Respect` or `Ignore`) is a per-call input parameter, not a state — it selects whether the single `tenant_closure.barrier` predicate is appended to the closure read, and no plugin-owned memory persists across calls. The SDK-visible `TenantStatus` values (`Active`, `Suspended`, `Deleted`) are read-only projections of AM's `tenants.status` column; AM's `feature-tenant-hierarchy-management` owns the tenant state machine and every write path that transitions rows between `provisioning`, `active`, `suspended`, and `deleted`, as anchored by `cpt-cf-tr-plugin-adr-p1-tenant-hierarchy-closure-ownership`. Provisioning-row invisibility is likewise not a state transition but a query-time structural filter asserted by `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-provisioning-invisibility-filter` on every read. Because the plugin persists no hierarchy cache and performs no post-query application-layer reshaping, there is no plugin-local lifecycle to model; any state machine asserted here would duplicate and risk drifting from AM's canonical lifecycle.

## 5. Definitions of Done

### SDK Method Contract Surface

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-sdk-method-contract`

The plugin **MUST** implement exactly the six hot-path SDK methods declared in DECOMPOSITION §2.1 `Requirements Covered` (`get_tenant`, `get_root_tenant`, `get_tenants`, `get_ancestors`, `get_descendants`, `is_ancestor`) with the signatures and return-type projections fixed by DESIGN §3.3 `interface-plugin-client`, and **MUST NOT** expose any additional SDK method, convenience wrapper, or internal helper on the `TenantResolverPluginClient` trait. Every return value **MUST** project AM-owned rows onto the SDK's `TenantInfo`, `TenantRef`, `GetAncestorsResponse`, or `GetDescendantsResponse` shape — the plugin **MUST NOT** invent SDK-visible fields, and **MUST NOT** leak raw database column names, raw `tenant_type_uuid` values, or provisioning rows into any response.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-principle-sdk-source-of-truth`

**Touches**:

- Entities: `TenantInfo`, `TenantRef`, `GetAncestorsResponse`, `GetDescendantsResponse` (SDK projection shapes; not plugin-owned)
- Data: `cpt-cf-account-management-dbtable-tenants` (read-only)
- Sibling integration: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway` (sole SDK caller via `ClientHub`)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability` (catalog owner of all `code` and `sub_code` identifiers and their HTTP status mapping)

### Barrier-as-Data Single Predicate

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-barrier-as-data-single-predicate`

Every barrier-aware SDK method **MUST** enforce `BarrierMode::Respect` by appending a single structural predicate on the AM-owned `tenant_closure.barrier` column to the closure read, and **MUST** enforce `BarrierMode::Ignore` by omitting the barrier predicate entirely with no additional traversal. The plugin **MUST NOT** filter, walk, or re-evaluate barriers in application code, **MUST NOT** cache prior barrier evaluations, and **MUST NOT** materialise an alternative in-process representation of the closure: the barrier decision is a property of the AM-owned closure row and is evaluated inside the database. Barrier-bypass invocations (`BarrierMode::Ignore`) **MUST** be recorded on a dedicated telemetry instrument for operator audit; the telemetry instrument itself does not affect the query semantics.

**Implements**:

- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-barrier-predicate-construction`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-principle-barrier-as-data`

**Touches**:

- Entities: `BarrierMode` (SDK input enum)
- Data: `cpt-cf-account-management-dbtable-tenant-closure` (`barrier` column, read-only)
- Sibling integration: `cpt-cf-account-management-feature-managed-self-managed-modes` (informational upstream; writes `barrier` column)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### Provisioning Row Invisibility

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-provisioning-invisibility`

Every SDK method **MUST** structurally exclude rows whose AM-owned `tenants.status` is `provisioning` via an unconditional query-time predicate produced by `algo-tenant-resolver-plugin-provisioning-invisibility-filter`. Any `provisioning` value that appears in a caller-supplied `status_filter` **MUST** be silently stripped at the algorithm boundary before the final query predicate is constructed; no caller input path **MAY** re-enable visibility of provisioning rows. The plugin **MUST NOT** implement provisioning invisibility by post-query application-layer filtering, because a post-query filter would permit a provisioning row to transiently cross the process boundary and surface in telemetry payloads, violating `cpt-cf-tr-plugin-fr-provisioning-invisibility`.

**Implements**:

- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-provisioning-invisibility-filter`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Touches**:

- Entities: `TenantStatus` (SDK-visible subset `Active` / `Suspended` / `Deleted`)
- Data: `cpt-cf-account-management-dbtable-tenants` (`status` column, read-only)
- Sibling integration: `cpt-cf-account-management-feature-tenant-hierarchy-management` (owns the tenant-status lifecycle upstream)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### Read-Only Database Role Enforcement

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-read-only-role-enforcement`

The plugin **MUST** connect to the account-management database exclusively through a dedicated role that holds `SELECT` privileges only on `tenants` and `tenant_closure` (and whatever read-only auxiliary objects DESIGN §3.7 enumerates for coverage indexes), and **MUST NOT** be granted any mutation privilege on any AM-owned object. A startup-time privilege assertion **MUST** verify that the configured role carries no `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `GRANT`, or DDL privilege on AM-owned schemas and **MUST** fail plugin bootstrap when an excess privilege is detected. CI **MUST** enforce the same privilege shape on every deployable artifact so that a misconfigured role cannot silently ship.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-constraint-read-only-role`, `cpt-cf-tr-plugin-constraint-am-storage-only`, `cpt-cf-tr-plugin-constraint-no-am-client`

**Touches**:

- Entities: Plugin runtime configuration (DB role identifier, connection parameters)
- Data: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure` (read-only)
- Sibling integration: `cpt-cf-tr-plugin-actor-operator` (provisions and rotates the role); `cpt-cf-account-management-feature-tenant-hierarchy-management` (upstream writer; sole mutation authority)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### No Wire API Exposure

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-no-wire-api`

The plugin **MUST NOT** expose any REST, gRPC, or other out-of-process transport; it is invoked strictly in-process through the `TenantResolverPluginClient` trait behind the Tenant Resolver gateway via `ClientHub`. The account-management OpenAPI specification **MUST NOT** list any plugin-owned endpoint, and the plugin **MUST NOT** open a listening socket on behalf of its SDK surface. Platform consumers that need hierarchy reads **MUST** reach the plugin transitively through the gateway; direct over-the-wire invocation of plugin methods is a contract violation enforced by the gateway boundary and by the absence of a transport layer in the plugin binary.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-constraint-no-wire-api`, `cpt-cf-tr-plugin-constraint-scope-exclusions`

**Touches**:

- Entities: `TenantResolverPluginClient` (SDK trait)
- Data: (none — surface is in-process only)
- Sibling integration: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway` (sole in-process caller)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### No Process-Local Hierarchy Cache

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-no-hierarchy-cache`

The plugin **MUST NOT** maintain any process-local cache of tenants, ancestors, descendants, closure rows, or barrier decisions. The only permitted in-memory cache is the bounded lazy `tenant_type_uuid` to public chained `tenant_type` reverse-lookup cache specified in DESIGN §3.2 (non-hierarchy data, populated on miss via Types Registry). Every hierarchy read **MUST** go to AM's canonical `tenants` and `tenant_closure` rows through the read-only role on every invocation, so that a stale or leaked projection can only be a property of AM's canonical data — never of a plugin-local cache — making the read surface auditable by construction per `principle-single-store`.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-principle-single-store`, `cpt-cf-tr-plugin-principle-sdk-source-of-truth`

**Touches**:

- Entities: Plugin runtime (process memory)
- Data: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure` (re-read on every call)
- Sibling integration: `cpt-cf-account-management-feature-tenant-hierarchy-management` (canonical writer — sole source of hierarchy truth)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### Deterministic Query-Time Ordering

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-deterministic-ordering`

`get_ancestors` **MUST** return the strict-ancestor chain in direct-parent-first order (depth descending from the starting tenant's parent down to the root), with the tenant identifier as a stable tie-break for ancestors at the same depth. `get_descendants` **MUST** return the bounded subtree in the SDK's documented pre-order (parent before its own descendants; siblings ordered deterministically by tenant identifier), bounded by the caller-supplied `max_depth`. Ordering **MUST** be enforced at query time (via `ORDER BY` clauses on the AM-owned depth column for ancestors and via the closure-join's pre-order projection for descendants); the plugin **MUST NOT** re-sort, reshape, or re-walk the result in application code, because application-layer ordering risks drift from the documented SDK contract and is not measurable against the single-query latency SLOs.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-descendant-bounded-preorder`

**Touches**:

- Entities: `GetAncestorsResponse`, `GetDescendantsResponse` (SDK projection shapes)
- Data: `cpt-cf-account-management-dbtable-tenants` (depth column, tenant identifier), `cpt-cf-account-management-dbtable-tenant-closure` (ancestor-descendant join)
- Sibling integration: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway`
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### ClientHub Registration via GTS Scope

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-clienthub-registration`

The plugin **MUST** register with the Tenant Resolver gateway via `ClientHub` under its GTS instance identifier as the registration scope, as required by `cpt-cf-tr-plugin-fr-plugin-api`, and the registration call **MUST** complete before the plugin accepts any SDK invocation. Registration failures (duplicate scope, gateway unavailability, malformed identifier) **MUST** surface through the `feature-errors-observability` envelope with the canonical `service_unavailable` sub-code, and the plugin **MUST NOT** retry registration silently in a manner that masks a persistent configuration error from the platform operator. Re-registration on gateway restart is an operator concern; the plugin itself **MUST** expose readiness only after a successful registration handshake.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Touches**:

- Entities: Plugin instance identity (GTS scope)
- Data: (none — registration is a control-plane handshake, not a data read)
- Sibling integration: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway` (registration target); `cpt-cf-tr-plugin-actor-operator` (owns GTS scope provisioning)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability` (surfaces `service_unavailable` on registration failure by name only; envelope owned upstream)

### Closure-Consistency Inheritance

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-closure-consistency-inheritance`

The plugin **MUST** inherit closure consistency transactionally from AM's writer per `cpt-cf-tr-plugin-nfr-closure-consistency`: `tenants` and `tenant_closure` rows (including the `barrier` and `descendant_status` columns) are written together inside AM's single commit, and the plugin's reads therefore observe a consistent snapshot without any cross-row reconciliation on the read side. The plugin **MUST NOT** attempt to recompute closure rows, repair barrier drift, or run integrity checks — any such remediation is exclusively owned by `cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check` and related AM administrative flows. When a read observes a row shape that would only be possible under a closure-invariant violation, the plugin **MUST** surface the canonical `service_unavailable` sub-code (envelope owned by `feature-errors-observability`) rather than patching the row, so the anomaly stays visible to operators.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-adr-p1-tenant-hierarchy-closure-ownership`, `cpt-cf-tr-plugin-principle-single-store`

**Touches**:

- Entities: `Tenant`, `TenantClosure` (AM-owned; read-only here)
- Data: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `cpt-cf-account-management-feature-tenant-hierarchy-management` (sole writer; owns `cpt-cf-account-management-algo-tenant-hierarchy-management-hierarchy-integrity-check`)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### SecurityContext Pass-Through

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-security-context-passthrough`

Every SDK method **MUST** accept the caller's `SecurityContext` from the gateway and propagate it, together with the OpenTelemetry trace context, onto every database span and telemetry record emitted during the invocation. The plugin **MUST NOT** make authorization decisions of its own, **MUST NOT** mutate the `SecurityContext`, and **MUST NOT** short-circuit a call based on caller identity — authorization is an upstream concern owned by the gateway and by the AuthZ Resolver. The `SecurityContext` is therefore a pass-through value for observability and downstream correlation, not an input to any plugin-side policy evaluation.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Constraints**: `cpt-cf-tr-plugin-constraint-security-context-passthrough`

**Touches**:

- Entities: `SecurityContext` (platform-owned value object)
- Data: (none — identity is propagated, not persisted)
- Sibling integration: `cpt-cf-tr-plugin-actor-tenant-resolver-gateway` (provides `SecurityContext`); `cpt-cf-tr-plugin-actor-authz-resolver` (upstream policy evaluator); `cpt-cf-tr-plugin-actor-pep` (upstream enforcement point)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability`

### Observability Surface Coverage

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-observability-surface`

The plugin **MUST** emit OpenTelemetry spans, metrics, and structured logs across the Performance, Reliability, Security, and Versatility vectors enumerated in DESIGN §3.2 and anchored by `cpt-cf-tr-plugin-nfr-observability`, covering at minimum per-method latency histograms, error-rate counters, barrier-bypass counters, and reverse-lookup cache hit/miss metrics. Audit events required by `cpt-cf-tr-plugin-nfr-audit-trail` **MUST** be emitted through the platform audit envelope — the plugin **MUST NOT** invent a private audit sink. Telemetry records **MUST** carry the caller's propagated `SecurityContext` and trace context, and **MUST NOT** embed provisioning rows, raw `tenant_type_uuid` values, or any PII beyond what the SDK already returns.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`

**Touches**:

- Entities: Telemetry span, metric instrument, audit event
- Data: (none — emission targets external telemetry pipelines; provisioning rows excluded by construction)
- Sibling integration: `cpt-cf-tr-plugin-actor-platform-telemetry` (consumer); `cpt-cf-tr-plugin-actor-operator` (threshold owner)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability` (audit envelope owner; canonical sub-codes `not_found`, `service_unavailable`, `validation`, `cross_tenant_denied` cited by name only in emitted records)

### Bounded Reverse-Lookup Cache

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-reverse-lookup-cache-bounded`

The plugin's `tenant_type_uuid` to public chained `tenant_type` reverse-lookup cache **MUST** be bounded in size per the `tenant_type_cache_max_entries` configuration documented in DESIGN §3.2, **MUST** evict the oldest entries when the bound is reached, and **MUST** resolve a cache miss through Types Registry per `cpt-cf-tr-plugin-contract-types-registry-reverse-lookup` before populating the new mapping. When Types Registry cannot resolve a `tenant_type_uuid`, the plugin **MUST** surface the canonical `service_unavailable` sub-code and **MUST NOT** fall back to emitting the raw UUID in the SDK's `tenant_type` field. The cache **MUST NOT** hold any hierarchy row, any barrier decision, or any visibility projection — it carries only the non-hierarchy type-name mapping.

**Implements**:

- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-tenant-type-reverse-lookup`

**Constraints**: `cpt-cf-tr-plugin-principle-single-store`

**Touches**:

- Entities: `TenantType` (public chained identifier); reverse-lookup cache (plugin-owned, bounded)
- Data: (none in AM — cache populates via Types Registry, not via `tenants` / `tenant_closure`)
- Sibling integration: Types Registry (platform module; see DESIGN §3.3 `interface-types-registry`)
- Error taxonomy: delegated to `cpt-cf-account-management-feature-errors-observability` (surfaces `service_unavailable` on unresolved UUID by name only)

### Error-Taxonomy Delegation

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-dod-tenant-resolver-plugin-error-taxonomy-delegation`

The plugin **MUST** delegate every public error-envelope concern — the category `code`, the `sub_code` identifier set, and the HTTP status mapping — to `cpt-cf-account-management-feature-errors-observability`, which is the canonical catalog owner. The plugin itself **MUST** name exactly the four sub-codes it can surface (`not_found`, `service_unavailable`, `validation`, `cross_tenant_denied`) by their canonical spellings and **MUST NOT** introduce new public sub-codes, redefine existing sub-code semantics, or override the HTTP status returned by the envelope. Validation of caller-supplied `GetTenantsOptions` and `GetDescendantsOptions` payloads surfaces `validation`; absent or provisioning starting-tenant identifiers surface `not_found`; database or Types Registry transient failures surface `service_unavailable`; cross-tenant denials surface `cross_tenant_denied` by reference to the upstream envelope — the plugin never constructs its own Problem body.

**Implements**:

- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-root-tenant`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-tenants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-ancestors`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-get-descendants`
- `cpt-cf-tr-plugin-flow-tenant-resolver-plugin-is-ancestor`
- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-provisioning-invisibility-filter`
- `cpt-cf-tr-plugin-algo-tenant-resolver-plugin-tenant-type-reverse-lookup`

**Touches**:

- Entities: Canonical sub-codes (`not_found`, `service_unavailable`, `validation`, `cross_tenant_denied`) referenced by name only
- Data: (none — error surfaces carry no AM row)
- Sibling integration: `cpt-cf-account-management-feature-errors-observability` (canonical catalog owner of `code`, `sub_code`, HTTP status mapping, and Problem envelope shape)
- Error taxonomy: delegated in full to `cpt-cf-account-management-feature-errors-observability`

## 6. Acceptance Criteria

- [ ] `TenantResolverPluginClient` exposes exactly the six SDK methods `get_tenant`, `get_root_tenant`, `get_tenants`, `get_ancestors`, `get_descendants`, `is_ancestor` with the signatures and return projections fixed by DESIGN §3.3 `interface-plugin-client`; no additional public method, convenience wrapper, or helper appears on the trait and no return value carries a raw `tenant_type_uuid`, a raw database column, or a provisioning row. Fingerprints `dod-tenant-resolver-plugin-sdk-method-contract`.
- [ ] Invoking `get_ancestors`, `get_descendants`, or `is_ancestor` with `BarrierMode::Respect` emits a single structural predicate on `tenant_closure.barrier` (the barrier-as-data form), while invoking the same methods with `BarrierMode::Ignore` emits no barrier predicate; the outcome is captured through query-plan inspection or a recorded query fixture, and `BarrierMode::Ignore` invocations increment the dedicated barrier-bypass telemetry instrument. Fingerprints `dod-tenant-resolver-plugin-barrier-as-data-single-predicate`.
- [ ] Inserting a synthetic `tenants` row with `status = 'provisioning'` and invoking each of the six SDK methods confirms the row is absent from every response, including invocations that supply `provisioning` in `GetDescendantsOptions.status_filter` or `GetTenantsOptions`, because the algorithm strips `provisioning` from caller input before constructing the query predicate. Fingerprints `dod-tenant-resolver-plugin-provisioning-invisibility`.
- [ ] Plugin startup and the CI privilege-shape check both assert that the configured database role holds `SELECT`-only grants on `tenants` and `tenant_closure` (plus the read-only coverage objects in DESIGN §3.7) and zero `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `GRANT`, or DDL privileges on AM-owned schemas; any attempted mutation through the plugin role is rejected with a database privilege error and plugin bootstrap fails when excess privilege is detected. Fingerprints `dod-tenant-resolver-plugin-read-only-role-enforcement`.
- [ ] The plugin crate exports zero REST or gRPC handler, opens zero listening socket for its SDK surface, and the account-management OpenAPI specification contains zero plugin-owned endpoint; the sole invocation path is the in-process `TenantResolverPluginClient` trait reached through the Tenant Resolver gateway via `ClientHub`. Fingerprints `dod-tenant-resolver-plugin-no-wire-api`.
- [ ] Source and runtime inspection confirm the plugin holds zero process-local cache of tenants, ancestors, descendants, closure rows, or barrier decisions; the only in-memory cache instance is the bounded lazy `tenant_type_uuid` reverse-lookup cache from `algo-tenant-resolver-plugin-tenant-type-reverse-lookup`, and every hierarchy read re-reads AM rows through the read-only role. Fingerprints `dod-tenant-resolver-plugin-no-hierarchy-cache`.
- [ ] `get_ancestors(tenant_id, BarrierMode::Respect)` returns rows in direct-parent-first order driven by `tenants.depth` DESC (starting from the direct parent down toward the root) with `tenants.id` ASC as tie-break, with ordering produced inside the database query (no application-layer sort); the same ordering holds for `BarrierMode::Ignore` invocations. Fingerprints `dod-tenant-resolver-plugin-deterministic-ordering`.
- [ ] `get_descendants(tenant_id, GetDescendantsOptions { max_depth = N, … })` returns SDK pre-order with siblings ordered by `tenants.id` ASC and the result size bounded by `max_depth`; the ordering and bound are produced at query time per `algo-tenant-resolver-plugin-descendant-bounded-preorder`. Fingerprints `dod-tenant-resolver-plugin-deterministic-ordering`.
- [ ] At module initialization the plugin registers with the Tenant Resolver gateway through `ClientHub` using its GTS instance identifier as the registration scope, and an observable registration event is emitted on the platform-telemetry channel; failure to register surfaces the canonical `service_unavailable` sub-code on every subsequent SDK call until registration succeeds. Fingerprints `dod-tenant-resolver-plugin-clienthub-registration`.
- [ ] A concurrent AM write to `tenants` or `tenant_closure` is visible to the plugin only after AM's transaction commits (read-committed or stricter isolation); the plugin performs zero reconciliation, repair, or retry on stale rows, and any invariant violation detected at query time surfaces the canonical `service_unavailable` sub-code rather than a plugin-side fix-up. Fingerprints `dod-tenant-resolver-plugin-closure-consistency-inheritance`.
- [ ] Every SDK invocation propagates the caller-supplied `SecurityContext` onto the emitted OpenTelemetry database span and every downstream span; the plugin performs zero authorization decisions, zero policy evaluation, and zero identity translation — those remain exclusively with the AuthZ Resolver and the PolicyEnforcement Point upstream. Fingerprints `dod-tenant-resolver-plugin-security-context-passthrough`.
- [ ] The plugin emits OpenTelemetry spans and metrics covering the Performance, Reliability, Security, and Versatility vectors per DESIGN §3.2 observability contract, and every audit event required by `cpt-cf-tr-plugin-nfr-audit-trail` is written through the platform audit envelope owned by `cpt-cf-account-management-feature-errors-observability`; no emitted payload carries PII beyond the stable `SecurityContext` identifiers. Fingerprints `dod-tenant-resolver-plugin-observability-surface`.
- [ ] The `tenant_type_uuid → tenant_type` reverse-lookup cache enforces the DESIGN-anchored size bound on insert, a cache miss triggers a Types Registry lookup that populates the cache, and an unresolved UUID surfaces the canonical `service_unavailable` sub-code; the cache stores zero hierarchy data, zero visibility decisions, and zero closure rows. Fingerprints `dod-tenant-resolver-plugin-reverse-lookup-cache-bounded`.
- [ ] Every public error path surfaces one of exactly four canonical sub-codes spelled `not_found`, `service_unavailable`, `validation`, `cross_tenant_denied`, by reference to the envelope owned by `cpt-cf-account-management-feature-errors-observability`; the plugin introduces zero new public sub-code, redefines zero existing sub-code, and constructs zero Problem body on its own. Fingerprints `dod-tenant-resolver-plugin-error-taxonomy-delegation`.

## 7. Deliberate Omissions

- **Tenant CRUD, closure writes, and `barrier` / `descendant_status` column maintenance** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.1 Out-of-scope, anchored by `cpt-cf-tr-plugin-adr-p1-tenant-hierarchy-closure-ownership`). The plugin holds `SELECT` grants only on `tenants` and `tenant_closure`, so every mutation including closure-row maintenance is the parent hierarchy-management feature's responsibility.
- **Barrier state lifecycle, `self_managed` flag writes, and managed/self-managed mode conversion** — *Owned by `cpt-cf-account-management-feature-managed-self-managed-modes`* (DECOMPOSITION §2.1 Depends-On). The plugin reads the materialized `tenant_closure.barrier` column through a single predicate; it does not drive the lifecycle that produces that column's values.
- **Authorization decisions, policy evaluation, and the right to invoke `BarrierMode::Ignore`** — *Owned by the AuthZ Resolver, the Policy Enforcement Point, and the Tenant Resolver gateway* (DECOMPOSITION §2.1 Out-of-scope). Whether a caller may bypass barriers or observe non-active tenants is evaluated upstream of the plugin; the plugin executes the query verbatim and records the bypass on telemetry.
- **REST, gRPC, or any other out-of-process transport surface** — *Forbidden by `cpt-cf-tr-plugin-constraint-no-wire-api`* (DECOMPOSITION §2.1 Out-of-scope). The plugin ships as an in-process Rust module behind the Tenant Resolver gateway; the gateway owns every network-facing contract.
- **Process-local caching of tenants, ancestors, descendants, closure rows, or barrier decisions** — *Forbidden by `cpt-cf-tr-plugin-principle-single-store`* and `cpt-cf-tr-plugin-principle-sdk-source-of-truth` (DECOMPOSITION §2.1 Out-of-scope). Only the bounded lazy `tenant_type_uuid` reverse-lookup cache is permitted; every hierarchy read re-reads AM rows.
- **Multi-region reads, read-replica routing, and cross-region latency budgeting** — *Out of v1 scope* (DECOMPOSITION §2.1 Out-of-scope). v1 ships as single-region primary-only; multi-region topology and replica routing are deferred to a future deployment-profile revision.
- **Standalone-plugin reusability against non-AM storage** — *Out of v1 scope* (DECOMPOSITION §2.1 Out-of-scope). TRP ships inside the `account-management` crate at `modules/system/account-management/src/tr_plugin/` because its correctness relies on AM-writer invariants beyond the two-table schema; a generalized plugin variant is not in this feature.
- **Cross-cutting error taxonomy, audit pipeline, metric catalog, and Problem envelope construction** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.1 Depends-On). Canonical sub-codes `not_found`, `service_unavailable`, `validation`, and `cross_tenant_denied` are catalogued there; this feature emits them by name only and never constructs a Problem body locally.
- **Tenant state machine and tenant-status lifecycle transitions** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.1 Out-of-scope; §4 States). The plugin projects `tenants.status` read-only onto the SDK-visible subset `Active` / `Suspended` / `Deleted`; lifecycle transitions between `provisioning`, `active`, `suspended`, and `deleted` are the parent feature's write-path responsibility.
- **User-facing UX, accessibility, and human-facing error messaging** — *Not applicable*: the plugin is an in-process SDK behind the Tenant Resolver gateway with zero human-facing surface; every caller is a platform component (gateway, AuthZ Resolver, Policy Enforcement Point). Human-readable error presentation, localization, and accessibility obligations attach to upstream REST/UI surfaces, not to this feature.
- **Regulatory compliance scope (GDPR, SOC2, HIPAA, etc.) and PII handling** — *Not applicable at this feature's surface*: the plugin stores no user data, performs no authorization, and propagates the caller's `SecurityContext` identifiers read-only onto telemetry spans without transformation. Compliance obligations for tenant data attach to AM's writer (`cpt-cf-account-management-feature-tenant-hierarchy-management`) and to the audit/observability pipeline owned by `cpt-cf-account-management-feature-errors-observability`; this feature emits no PII beyond what the SDK already returns.
