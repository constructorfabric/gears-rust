# Decomposition: Tenant Resolver Plugin (tr-plugin)

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Tenant Resolver Plugin - HIGH](#21-tenant-resolver-plugin---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This DECOMPOSITION covers the Tenant Resolver Plugin (`tr-plugin`) sub-system, a read-only, in-process query facade that runs inside the parent `account-management` crate and serves barrier-aware hierarchy reads over the parent module's canonical `(tenants, tenant_closure)` storage. It is a separate SDLC artifact from the parent [account-management DECOMPOSITION](../DECOMPOSITION.md) because the `cf-tr-plugin` namespace is a distinct Cypilot sub-system (see `.cypilot/config/artifacts.toml` children.autodetect block): the tr-plugin owns its own PRD, DESIGN, ADR, and schema under `modules/system/account-management/docs/tr-plugin/`, and per the registry contract must define its own feature IDs in its own DECOMPOSITION file rather than in the parent's.

**Scope**: one feature — `tenant-resolver-plugin` — that encapsulates the complete tr-plugin sub-system. The parent DECOMPOSITION references this feature in its §3 Feature Dependencies DAG (as a cross-system edge from `cpt-cf-account-management-feature-tenant-hierarchy-management`) but does not define it.

**Traceability**: this artifact is registered with `traceability = "DOCS-ONLY"` (per the children.artifacts block in `.cypilot/config/artifacts.toml`); downstream FEATURE generation will add code markers.

## 2. Entries

### 2.1 [Tenant Resolver Plugin](./features/feature-tenant-resolver-plugin.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-tr-plugin-feature-tenant-resolver-plugin`

- **Purpose**: The Tenant Resolver Plugin (TRP) exposes a read-only, in-process SDK over the parent account-management module's canonical `(tenants, tenant_closure)` storage, answering every hot-path hierarchy question (`get_tenant`, `get_root_tenant`, `get_tenants`, `get_ancestors`, `get_descendants`, `is_ancestor`) in single-digit milliseconds. It enforces barrier semantics as a single SQL predicate on AM's platform-canonical `tenant_closure.barrier` column (barrier-as-data) and structurally excludes provisioning-state rows from the SDK surface. It owns no closure writes, no process-local hierarchy cache, and no public wire API — every consistency, versioning, and visibility guarantee is inherited transactionally from AM.

- **Depends On**: `cpt-cf-account-management-feature-tenant-hierarchy-management` (hard dependency — reads the canonical `tenants` + `tenant_closure` pair this feature owns, including the denormalized `barrier` and `descendant_status` columns). Informational upstream context also flows from `cpt-cf-account-management-feature-managed-self-managed-modes` (defines the semantics encoded in the `barrier` column) and `cpt-cf-account-management-feature-errors-observability` (error taxonomy, telemetry conventions), but hierarchy-management is the sole data-level hard dependency.

- **Scope**:
  - 6 SDK methods exposed by `TenantResolverPluginClient`: `get_tenant`, `get_root` (`get_root_tenant`), `get_tenants`, `get_ancestors`, `get_descendants`, `is_ancestor`
  - Barrier-aware reads over AM-owned `tenant_closure` using the canonical single-predicate form on the `barrier` column (`BarrierMode::Respect` vs. `BarrierMode::Ignore`)
  - Dedicated read-only DB role for the plugin: `SELECT`-only grants on AM's `tenants` and `tenant_closure`, with no mutation privileges on any AM-owned object (privilege asserted at plugin startup and in CI)
  - Provisioning-row invisibility filter enforced uniformly across every SDK method — `cpt-cf-tr-plugin-fr-provisioning-invisibility` — so transient `tenants.status = 'provisioning'` rows can never appear in any SDK response regardless of caller-supplied status filter
  - Deterministic ordering: direct-parent-first for ancestors (driven by AM's `tenants.depth` column with `tenants.id` tie-break), SDK pre-order for descendants (bounded by `max_depth` and siblings ordered by `id`), with no application-layer hierarchy walk
  - Plugin registration with the Tenant Resolver gateway via `ClientHub` using the plugin's GTS instance identifier as scope (`cpt-cf-tr-plugin-fr-plugin-api`)
  - OpenTelemetry telemetry set covering Performance / Reliability / Security / Versatility vectors (`cpt-cf-tr-plugin-fr-observability`, `cpt-cf-tr-plugin-nfr-observability`, `cpt-cf-tr-plugin-nfr-audit-trail`)
  - Bounded lazy reverse-lookup cache for `tenant_type_uuid -> tenant_type` (non-hierarchy data; does not cache visibility decisions)

- **Out of scope**:
  - **No closure writes** — the parent module (`cpt-cf-account-management-feature-tenant-hierarchy-management`) owns every write to `tenants` and `tenant_closure`, including the `barrier` and `descendant_status` columns (ADR-001 closure-ownership decision). The plugin holds only `SELECT` grants.
  - **No REST/gRPC wire API** — the plugin is strictly in-process behind the Tenant Resolver gateway; `cpt-cf-tr-plugin-constraint-no-wire-api` is a first-class constraint. The gateway owns all network-facing contracts.
  - **No process-local caching of hierarchy data** — no in-memory cache of tenants, ancestors, descendants, or closure rows. Consistency is a transactional property of AM's writes, not of a plugin cache invalidation scheme. Only the non-hierarchy `tenant_type_uuid` reverse-lookup cache is allowed.
  - Authorization decisions (AuthZ Resolver / gateway concern — whether a caller may use `BarrierMode::Ignore` or observe `suspended`/`deleted` tenants is not enforced here)
  - Tenant CRUD, mode change, status change, type validation (all AM-owned)
  - Multi-region reads and read-replica routing (v1 is single-region, primary-only; revisit per deployment profile)
  - Standalone-plugin reusability against non-AM storage — TRP ships inside the `account-management` crate at `modules/system/account-management/src/tr_plugin/` because its correctness relies on AM-writer invariants beyond the two-table schema

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-tr-plugin-fr-plugin-api`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-get-tenant`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-get-root-tenant`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-get-tenants`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-get-ancestors`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-get-descendants`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-is-ancestor`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-barrier-semantics`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-status-filtering`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-provisioning-invisibility`
  - [ ] `p1` - `cpt-cf-tr-plugin-fr-observability`
  - [ ] `p1` - `cpt-cf-tr-plugin-nfr-query-latency`
  - [ ] `p1` - `cpt-cf-tr-plugin-nfr-subtree-latency`
  - [ ] `p1` - `cpt-cf-tr-plugin-nfr-closure-consistency`
  - [ ] `p1` - `cpt-cf-tr-plugin-nfr-tenant-isolation`
  - [ ] `p2` - `cpt-cf-tr-plugin-nfr-audit-trail`
  - [ ] `p2` - `cpt-cf-tr-plugin-nfr-observability`
  - Informational cross-system NFR — parent-namespace `cpt-cf-account-management-nfr-context-validation-latency` (PRD §6.1). Per Phase 2 feature-map §3.1 Option-B redistribution, the hot-path context-validation latency SLO is served by this plugin's reads over AM storage; the NFR remains *defined* in the parent account-management system (authoritative source) but is *implemented/tested* here.

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-tr-plugin-principle-query-facade`
  - [ ] `p1` - `cpt-cf-tr-plugin-principle-sdk-source-of-truth`
  - [ ] `p1` - `cpt-cf-tr-plugin-principle-barrier-as-data`
  - [ ] `p1` - `cpt-cf-tr-plugin-principle-single-store`
  - [ ] `p1` - ADR reference: `cpt-cf-tr-plugin-adr-p1-tenant-hierarchy-closure-ownership` ([ADR-001](./ADR/ADR-001-tenant-hierarchy-closure-ownership.md)) — records the barrier-as-data / closure-ownership decision (AM owns the canonical `tenants` + `tenant_closure` pair including the `barrier` and `descendant_status` columns; the plugin is a pure query facade that reads AM-owned storage via a read-only DB role)

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-am-storage-only`
  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-read-only-role`
  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-no-am-client`
  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-security-context-passthrough`
  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-no-wire-api`
  - [ ] `p1` - `cpt-cf-tr-plugin-constraint-versioning-policy`
  - [ ] `p3` - `cpt-cf-tr-plugin-constraint-scope-exclusions`
  - Statelessness (no in-memory hierarchy cache) is encoded via the `cpt-cf-tr-plugin-principle-single-store` principle and enforced operationally by the absence of any hierarchy-cache component in DESIGN §3.2.

- **Domain Model Entities**:
  - `Tenant` (read-only view of AM's `tenants` row, projected to `TenantInfo` / `TenantRef` for SDK responses)
  - `TenantClosure` (read-only view of AM's platform-canonical `tenant_closure` table — `(ancestor_id, descendant_id, barrier, descendant_status)`)
  - `TenantType` (reverse-lookup from `tenant_type_uuid` via Types Registry; bounded lazy cache)
  - `BarrierMode` (SDK enum — `Respect` / `Ignore`; mapped to a single predicate on the canonical `barrier` column)
  - `TenantStatus` (SDK-visible domain: `Active` / `Suspended` / `Deleted`; provisioning is excluded by construction)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-tr-plugin-component-plugin-impl` (sole plugin component — implements the `TenantResolverPluginClient` SDK trait, owns query construction, barrier/status predicate application, provisioning-row exclusion, and ordering guarantees; registers with the Tenant Resolver gateway via `ClientHub`)

- **API**:
  - **In-process SDK only (no REST/gRPC wire API)** — the plugin is an in-process Rust module behind the Tenant Resolver gateway; the gateway owns every network-facing contract (`cpt-cf-tr-plugin-constraint-no-wire-api`). SDK methods exposed:
  - `get_tenant(tenant_id) -> TenantInfo | TenantResolverError`
  - `get_root_tenant() -> TenantInfo | TenantResolverError`
  - `get_tenants(ids, GetTenantsOptions) -> Vec<TenantInfo>`
  - `get_ancestors(tenant_id, BarrierMode) -> GetAncestorsResponse`
  - `get_descendants(tenant_id, GetDescendantsOptions { barrier_mode, max_depth, status_filter }) -> GetDescendantsResponse`
  - `is_ancestor(ancestor_id, descendant_id, BarrierMode) -> bool`
  - Plugin registration interface: `cpt-cf-tr-plugin-interface-plugin-client` (SDK trait) and `cpt-cf-tr-plugin-interface-plugin-client-contract` (ClientHub/gateway wiring)
  - AM-owned schema contract consumed read-only: `cpt-cf-tr-plugin-interface-am-schema`
  - External contracts: `cpt-cf-tr-plugin-contract-am-read-only-role`, `cpt-cf-tr-plugin-contract-types-registry-reverse-lookup`

- **Sequences**:

  - [ ] `p1` - `cpt-cf-tr-plugin-seq-get-tenant`
  - [ ] `p1` - `cpt-cf-tr-plugin-seq-get-root-tenant`
  - [ ] `p1` - `cpt-cf-tr-plugin-seq-ancestor-query`
  - [ ] `p1` - `cpt-cf-tr-plugin-seq-descendant-query`
  - [ ] `p1` - `cpt-cf-tr-plugin-seq-is-ancestor`

- **Data**:

  - [ ] `p1` - `tenants` (AM-owned table; consumed via the dedicated read-only DB role — existence probes, bulk-by-ids, ancestor hydration JOINs; provisioning rows filtered out in the query-builder as defense-in-depth)
  - [ ] `p1` - `tenant_closure` (AM-owned table with the platform-canonical schema `(ancestor_id, descendant_id, barrier, descendant_status)`; consumed via the dedicated read-only DB role — all barrier and status semantics reduce to single predicates on this table)
  - [ ] `p3` - `cpt-cf-tr-plugin-db-schema` (DESIGN §3.7 — read-only schema & index coverage reference; no schema authored by this feature)
  - [ ] `p1` - Plugin descriptor schema: `gts://gts.x.core.modkit.plugin.v1~x.core.tenant_resolver.plugin.v1~` ([`tr_plugin.v1.schema.json`](./schemas/tr_plugin.v1.schema.json))
  - **No dbtables authored by this feature** — `tenants` and `tenant_closure` are owned and migrated by `cpt-cf-account-management-feature-tenant-hierarchy-management`.

---

## 3. Feature Dependencies

```text
cpt-cf-account-management-feature-tenant-hierarchy-management  (cross-system, parent AM — owner of tenants + tenant_closure)
    ↓
cpt-cf-tr-plugin-feature-tenant-resolver-plugin                (this sub-system — leaf in the overall DAG)
```

**Dependency rationale**:

- `cpt-cf-tr-plugin-feature-tenant-resolver-plugin` depends on the parent account-management system's `tenant-hierarchy-management` feature, which owns the canonical `tenants` + `tenant_closure` tables the plugin reads. This is the only hard cross-system edge.
- Informational upstream influences (not hard dependencies) flow from the parent's `managed-self-managed-modes` (defines barrier semantics) and `errors-observability` (error taxonomy, telemetry conventions); these are referenced in §2.1's *Depends On* block but do not add new DAG nodes here since they do not produce artifacts this plugin reads directly.
- The plugin is a DAG leaf — no downstream consumers inside this sub-system.
- Cycle check: trivial (1 internal feature, 1 cross-system edge).

**Cross-system reference**: the parent [account-management DECOMPOSITION](../DECOMPOSITION.md) §3 references this feature as a leaf on the `cpt-cf-account-management-feature-tenant-hierarchy-management` branch to preserve the whole-system dependency view. The definitive feature entry lives here.
