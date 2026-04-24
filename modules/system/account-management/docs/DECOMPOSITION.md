# Decomposition: Account Management (AM)


<!-- toc -->

- [1. Overview](#1-overview)
  - [Decomposition strategy](#decomposition-strategy)
  - [Mutual exclusivity rationale](#mutual-exclusivity-rationale)
  - [Traceability promise](#traceability-promise)
  - [Implementation phasing](#implementation-phasing)
- [2. Entries](#2-entries)
  - [2.1 Platform Bootstrap - HIGH](#21-platform-bootstrap---high)
  - [2.2 Tenant Hierarchy Management - HIGH](#22-tenant-hierarchy-management---high)
  - [2.3 Tenant Type Enforcement - HIGH](#23-tenant-type-enforcement---high)
  - [2.4 Managed / Self-Managed Modes - HIGH](#24-managed--self-managed-modes---high)
  - [2.5 IdP User Operations Contract - HIGH](#25-idp-user-operations-contract---high)
  - [2.6 User Groups (via Resource Group delegation) - HIGH](#26-user-groups-via-resource-group-delegation---high)
  - [2.7 Tenant Metadata - MEDIUM](#27-tenant-metadata---medium)
  - [2.8 Errors & Observability - HIGH](#28-errors--observability---high)
  - [2.9 Tenant Resolver Plugin — defined in sub-system DECOMPOSITION](#29-tenant-resolver-plugin--defined-in-sub-system-decomposition)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-account-management-status-overall`
## 1. Overview

This document decomposes the Account Management (AM) module — covering
both the parent service (`modules/system/account-management/`) and the
co-located Tenant Resolver Plugin (`modules/system/account-management/src/tr_plugin/`,
specified under `docs/tr-plugin/`) — into a fixed set of **nine features**
that partition every inventoried PRD / DESIGN identifier exactly once.
Every functional requirement, non-functional requirement, design
principle, design constraint, DESIGN component, sequence diagram, data
entity, OpenAPI operation, JSON schema, and ADR enumerated in
`out/phase-01-inventory.md` is assigned to exactly one of the nine
features per `out/phase-02-feature-map.md` (100% coverage with mutual
exclusivity; see §1 "Mutual exclusivity rationale" below). The 9-feature
cut was driven by three pillars described next.

### Decomposition strategy

The 9 features reflect three independent grouping pillars applied to the
same inventory, yielding the same partition under all three lenses:

1. **Service-aligned grouping** (DESIGN §3.2). AM's DESIGN defines five
   in-service components — `AccountManagementModule`, `TenantService`,
   `ConversionService`, `MetadataService`, and `BootstrapService` — each
   with a coherent domain responsibility. Four of those map 1:1 to
   features (`platform-bootstrap` owns `AccountManagementModule` +
   `BootstrapService`; `tenant-hierarchy-management` owns `TenantService`;
   `managed-self-managed-modes` owns `ConversionService`;
   `tenant-metadata` owns `MetadataService`). The remaining functional
   surfaces that do not crystallize into a DESIGN component —
   tenant-type enforcement (a pre-write barrier invoked by
   `TenantService`), IdP user operations (a pluggable contract, not a
   component), user groups (delegated to the Resource Group module), and
   errors/observability (cross-cutting taxonomy + metrics) — become
   features in their own right because each has a distinct PRD FR group,
   a distinct public surface (or explicit no-surface rationale), and a
   distinct downstream contract.

2. **PRD §5 FR-group grouping**. PRD §5 already organizes the 42 parent
   FRs into eight thematic groups (§5.1 Platform Bootstrap, §5.2 Tenant
   Hierarchy, §5.3 Tenant Types, §5.4 Modes & Conversions, §5.5 IdP User
   Operations, §5.6 User Groups, §5.7 Tenant Metadata, §5.8 Errors &
   Observability). Each of those §5.x groups collapses exactly to one
   parent-module feature of the same name. No FR spans two groups, and
   the eighth group (Errors & Observability) naturally inherits the
   cross-cutting NFR / constraint residue from PRD §6 after Option-B
   redistribution (see Phase 2 feature-map §3.1 Revision History).

3. **Tenant Resolver Plugin isolation**. The tr-plugin has a separate
   PRD/DESIGN pair under `docs/tr-plugin/` because it is a deployable
   sub-surface with its own principles (query-facade, single-store,
   barrier-as-data), constraints (AM-storage-only, read-only role,
   no-wire-API), and OTel telemetry set. It is intentionally excluded
   from the eight parent features and rendered as a ninth,
   self-contained feature with the `cpt-cf-tr-plugin-feature-*` prefix.
   Although the plugin ships inside the `account-management` crate (its
   correctness relies on AM writer invariants beyond the two-table
   schema), the PRD/DESIGN/feature spec boundary is maintained to keep
   the read-only SDK contract reviewable and releasable independently.

All three lenses produce the same 9-feature partition, which is why the
feature count, slugs, priorities, and prefixes were locked in Phase 2
(`plan.toml [decisions]`) before the Phase 3-7 feature-entry authoring
began.

### Mutual exclusivity rationale

The Phase 2 coverage proof (`out/phase-02-feature-map.md` §2) shows
every inventoried ID owned by exactly one feature — zero shared IDs,
zero orphaned IDs. Cross-feature relationships that could superficially
imply sharing are deliberately modelled as **dependency edges** in §3
below rather than as co-ownership. Notable placement decisions:

- `tenant_closure` (including the `barrier` column) is owned by
  `tenant-hierarchy-management` per ADR-0007, even though
  `managed-self-managed-modes` writes into the `barrier` cell on
  conversion approval and `tenant-resolver-plugin` reads the column
  on every hot-path query — both are captured as dependencies, not
  shared ownership.
- `seq-create-child` (the `POST /tenants` end-to-end sequence) is
  owned by `tenant-hierarchy-management` because it is the
  tenant-creation control flow; the type-enforcement barrier and
  mode-selection sub-flow are consumed by it as dependencies, not as
  shared sequences.
- `component-module` (`AccountManagementModule`) is owned by
  `platform-bootstrap` because its sole runtime responsibility is
  module lifecycle / bootstrap orchestration. Routes it registers
  belong to other features and are owned there.
- NFRs were redistributed from `errors-observability` to the
  functional features that own their enforcement primitives
  (Phase 2 §3.1 Revision History, "Option B") — e.g.
  `nfr-context-validation-latency` moved to `tenant-resolver-plugin`
  because the hot-path read is served by the plugin;
  `nfr-authentication-context` moved to `idp-user-operations-contract`
  because the AuthN boundary is the IdP contract. Only genuinely
  cross-cutting policies (audit, compatibility, data-classification,
  reliability, ops-metrics-treatment) remain under
  `errors-observability`.

### Traceability promise

Every public identifier in the inventory has a single feature owner, and
each feature has a dedicated spec file at
`modules/system/account-management/docs/features/feature-{slug}.md` (for the 8
parent features) or
`modules/system/account-management/docs/tr-plugin/features/feature-{slug}.md`
(for the tr-plugin feature) where the deep FEATURE spec artifacts and
CODE-phase links will live. The traceability chain is therefore:

`PRD (capability) → PRD (FR/NFR) → DESIGN (principle/constraint/component/seq/entity/ADR) → DECOMPOSITION (feature-{slug}) → FEATURE spec → CODE marker`.

`cpt validate` (run in Phase 9) confirms every `cpt-cf-account-management-*`
and `cpt-cf-tr-plugin-*` ID referenced below is either defined in this
DECOMPOSITION (feature IDs + status-overall ID) or resolves to an
upstream PRD/DESIGN definition, with no broken references.

### Implementation phasing

The `plan.toml [[pr_strategy_bundles]]` table locks **Strategy C** —
three sequenced PRs that group features by functional and storage
affinity to keep each PR within the ~3k-to-5k change-line review
target (change-lines = additions + deletions + modifications;
estimates are pre-implementation forecasts, not hard contracts).

- **PR1 — Foundation + Core + Types** (~3,350 change-lines).
  Features: `errors-observability`, `platform-bootstrap`,
  `tenant-hierarchy-management`, `tenant-type-enforcement`. Ships the
  cross-cutting error taxonomy / metrics / policy surfaces, the root
  tenant bootstrap, the `tenants` + `tenant_closure` schema, and the
  GTS-backed parent/child type barrier. Depends on no earlier PR.
- **PR2 — Modes + IdP + User-Groups + Metadata** (~4,050 change-lines).
  Features: `managed-self-managed-modes`, `idp-user-operations-contract`,
  `user-groups`, `tenant-metadata`. Ships the dual-consent
  `ConversionRequest` state machine, the pluggable IdP user-operations
  contract, the Resource-Group delegation path for user groups, and
  the extensible tenant-metadata subsystem. Depends on PR1.
- **PR3 — Tenant Resolver Plugin** (~1,950 change-lines).
  Feature: `tenant-resolver-plugin`. Ships the read-only in-process
  SDK over `tenants` + `tenant_closure` with barrier-mode semantics
  and provisioning-row invisibility. Depends on PR1 (reads the
  hierarchy tables) and consumes barrier semantics defined in PR2,
  but is review-independent of PR2's write paths.

These are **change-line estimates for PR sizing**, not
implementation-order constraints beyond what the dependency DAG in §3
already dictates. A downstream sequencing decision could split PR2
further (e.g., metadata into its own PR) if actual change-line counts
exceed the 5k rejection threshold during implementation.

## 2. Entries

### 2.1 [Platform Bootstrap](./features/feature-platform-bootstrap.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-platform-bootstrap`

- **Purpose**: One-time initialization of the Account Management module on first platform start — creates the canonical root tenant, invokes the IdP tenant-provisioning contract for that root, and guarantees that platform upgrades or service restarts detect the existing root and proceed as a no-op. Bootstrap is the gate that publishes `status=active` on the root so every downstream tenant-consuming feature (hierarchy, modes, metadata, user operations, tenant-resolver plugin) has a foundation to build on.

- **Depends On**: `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Automatic creation of the initial root tenant when AM starts for the first time, with the deployment-configured root tenant type, completing bootstrap with the root visible in status `active`.
  - Invocation of the tenant-provisioning operation for the root tenant via the shared IdP integration contract, forwarding deployer-configured metadata so the IdP provider plugin can establish the tenant-to-IdP binding, and persisting any provisioning metadata the provider returns.
  - Idempotent behaviour across platform upgrade and AM restart: detect an existing root tenant and preserve it without duplication (bootstrap MUST be a no-op when the root already exists).
  - Ordering guarantee: wait for the IdP to be available before completing bootstrap, retry with backoff, and fail after a configurable timeout if the IdP is not ready.
  - Module-lifecycle plumbing for bootstrap orchestration: `AccountManagementModule` owns the ModKit `lifecycle(entry = ...)` entry point that invokes `BootstrapService` before the module signals ready; `BootstrapService` owns distributed-lock root creation and the IdP-wait loop.

- **Out of scope**:
  - Creation of the initial Platform Administrator user identity — the Platform Admin is pre-provisioned in the IdP during infrastructure setup; AM does not create this user (covered by the `idp-user-operations-contract` feature for all other user operations).
  - Validation of IdP binding sufficiency or identifier equality between AM's tenant UUID and the IdP's internal identifiers — that is the IdP provider's responsibility.
  - Creation of any non-root tenant — child tenant creation belongs to `tenant-hierarchy-management`.
  - Registration of GTS-backed types/schemas (tenant types, metadata schemas, user-group RG type) — those are performed by the respective functional features (`tenant-type-enforcement`, `tenant-metadata`, `user-groups`) during their own initialization paths; bootstrap's IdP wait does not gate them.
  - Error taxonomy and metrics definitions themselves — provided by the `errors-observability` foundation feature.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-root-tenant-creation`
  - [ ] `p1` - `cpt-cf-account-management-fr-root-tenant-idp-link`
  - [ ] `p1` - `cpt-cf-account-management-fr-bootstrap-idempotency`
  - [ ] `p1` - `cpt-cf-account-management-fr-bootstrap-ordering`

- **Design Principles Covered**:

  - `cpt-cf-account-management-principle-source-of-truth` (bootstrap establishes the root tenant as the canonical hierarchy anchor that all downstream consumers derive from)
  - `cpt-cf-account-management-principle-idp-agnostic` (bootstrap is the first invocation of the pluggable IdP contract; AM forwards `root_tenant_metadata` through without interpretation)

- **Design Constraints Covered**:

  - No design constraints are assigned in the Phase 2 feature-map.

- **Domain Model Entities**:
  - Tenant (the root tenant row — `parent_id IS NULL`, `depth = 0`, `tenant_type` from deployment configuration, terminal `status = active` after the `provisioning → active` transition)
  - TenantStatus (the `provisioning → active` lifecycle states traversed during bootstrap)
  - TenantMetadata (IdP provider-returned provisioning metadata persisted on the root tenant, when the provider returns any)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-account-management-component-module`
  - [ ] `p1` - `cpt-cf-account-management-component-bootstrap-service`

- **API**:
  - Internal module-lifecycle entry point only — no public REST endpoints are registered by this feature. `AccountManagementModule::init` wires `BootstrapService`, and `AccountManagementModule::lifecycle(entry = ...)` invokes `BootstrapService::run` during startup before the module signals ready. Bootstrap configuration (root tenant type, IdP wait timeout/backoff, strict-mode flags) is supplied via deployment configuration, not via a runtime API or CLI command.

- **Sequences**:

  - `cpt-cf-account-management-seq-bootstrap`

- **Data**:

  - [ ] `p1` - (no dbtables assigned in Phase 2 feature-map — bootstrap writes to `tenants` but ownership of that table belongs to `tenant-hierarchy-management`)

### 2.2 [Tenant Hierarchy Management](./features/feature-tenant-hierarchy-management.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-tenant-hierarchy-management`

- **Purpose**: Full lifecycle of tenants within the canonical tree owned by Account Management — create child tenants, read and list children, enforce the configurable advisory depth threshold, transition status between `active` and `suspended`, soft-delete (leaf-first, with retention window) and hard-delete, and transactionally maintain the platform-canonical `tenant_closure` table `(ancestor_id, descendant_id, barrier, descendant_status)` so every downstream reader (authorization, Tenant Resolver Plugin, replication consumers) observes tree and closure as one consistent state. Tenant-side IdP operations (provision on create, deprovision on hard-delete, and provision-failure handling) are first-class side effects of this feature's CRUD paths.

- **Depends On**: `cpt-cf-account-management-feature-platform-bootstrap`, `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Create child tenant: authenticated parent-tenant administrator creates a new child with an explicit `parent_id`, establishing the relationship immediately and finalising the tenant in `status = active` once the creation saga (insert-provisioning, IdP provision, finalise) completes successfully.
  - Read tenant detail by identifier within the caller's authorized scope.
  - List direct children of a tenant, paginated and status-filterable.
  - Update mutable tenant fields only (`name` and `active ↔ suspended` status transitions); immutable hierarchy-defining fields (`id`, `parent_id`, `tenant_type`, `self_managed`, `depth`) are rejected; `status=deleted` is rejected with `422` — soft-delete goes through the dedicated DELETE endpoint.
  - Tenant status change: administrator transitions between `active` and `suspended` without cascading to children; transition to `deleted` is not permitted via status change.
  - Soft-delete: non-root-only, requires zero non-deleted children and no remaining tenant-owned Resource Group associations; schedules hard-deletion after retention period; hard-delete runs leaf-first (`depth DESC`) and invokes IdP tenant-deprovisioning.
  - Configurable advisory hierarchy-depth threshold (default 10) with operator-visible warning signal (metric + structured log) when exceeded, plus an opt-in strict mode that rejects creation above the threshold with `tenant_depth_exceeded`.
  - Tenant closure ownership: AM owns the `tenant_closure` table with shape `(ancestor_id, descendant_id, barrier, descendant_status)`; closure rows exist only for SDK-visible statuses (`active`, `suspended`, `deleted`), never for transient `provisioning`; self-rows carry `barrier = 0`; all closure writes are transactional with the owning `tenants` write (activation, status change, hard-delete).
  - IdP tenant-side lifecycle hooks: `fr-idp-tenant-provision` invoked during tenant creation, `fr-idp-tenant-provision-failure` handling on provider errors, `fr-idp-tenant-deprovision` invoked during hard-delete — all through `IdpProviderPluginClient`; providers MUST NOT silently no-op on mutating operations.
  - Hierarchy integrity diagnostics: `TenantService::check_hierarchy_integrity()` internal SDK method + `am_hierarchy_integrity_violations` metric surface; remediation expectations for detected anomalies.
  - Production-scale operating envelope: closure-table sizing, depth threshold, and benchmark-backed deployment profiles for supported hierarchies.

- **Out of scope**:
  - Tenant-type parent-child validation (GTS `allowed_parent_types`, same-type nesting) — owned by `tenant-type-enforcement`, invoked by `TenantService` during create.
  - Managed vs self-managed mode selection, barrier semantics, and `ConversionRequest` dual-consent state machine — owned by `managed-self-managed-modes`; this feature only maintains the `barrier` column in `tenant_closure` as a transactional consequence of mode writes performed by that feature.
  - User-level IdP operations (provision/deprovision/query of users) — owned by `idp-user-operations-contract`. Tenant-side IdP operations (provision/deprovision of tenants) remain in this feature as hierarchy-op side-effects.
  - Tenant metadata CRUD, schemas, and inheritance resolution — owned by `tenant-metadata`; metadata is removed through the tenant-metadata feature's cascade-delete contract when a tenant row is removed, while the schema and resolution logic live in that feature.
  - User-group Resource Group type registration and lifecycle — owned by `user-groups`.
  - Read-only plugin query facade (`get_tenant`, `get_ancestors`, `get_descendants`, barrier-mode reductions) — owned by `tenant-resolver-plugin`, which reads AM-owned `tenants` and `tenant_closure` directly via a dedicated SecureConn read-only pool.
  - Cross-cutting error taxonomy, audit pipeline, reliability/SLA policy, data-classification policy — owned by `errors-observability`.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-create-child-tenant`
  - [ ] `p1` - `cpt-cf-account-management-fr-hierarchy-depth-limit`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-status-change`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-soft-delete`
  - [ ] `p1` - `cpt-cf-account-management-fr-children-query`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-read`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-update`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-closure`
  - [ ] `p1` - `cpt-cf-account-management-fr-idp-tenant-provision`
  - [ ] `p1` - `cpt-cf-account-management-fr-idp-tenant-provision-failure`
  - [ ] `p1` - `cpt-cf-account-management-fr-idp-tenant-deprovision`
  - [ ] `p1` - `cpt-cf-account-management-nfr-production-scale`
  - [ ] `p1` - `cpt-cf-account-management-nfr-data-lifecycle`
  - [ ] `p2` - `cpt-cf-account-management-nfr-data-quality`
  - [ ] `p2` - `cpt-cf-account-management-nfr-data-integrity-diagnostics`
  - [ ] `p2` - `cpt-cf-account-management-nfr-data-remediation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-account-management-principle-source-of-truth`
  - [ ] `p1` - `cpt-cf-account-management-principle-tree-invariant`

- **Design Constraints Covered**:

  - No design constraints are assigned in the Phase 2 feature-map.

- **Domain Model Entities**:
  - Tenant (canonical tenant node: `id`, `parent_id`, `tenant_type`, `self_managed`, `depth`, `name`, `status`)
  - TenantStatus (`provisioning`, `active`, `suspended`, `deleted`; transitions owned here, `provisioning` is transient and SDK-invisible)
  - TenantClosure (`(ancestor_id, descendant_id, barrier, descendant_status)` rows maintained transactionally; self-row invariant, coverage invariant, barrier materialization invariant, `descendant_status` denormalization invariant)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-account-management-component-tenant-service`

- **API**:
  - `POST /api/account-management/v1/tenants` (`createTenant`)
  - `GET /api/account-management/v1/tenants/{tenant_id}` (`getTenant`)
  - `PATCH /api/account-management/v1/tenants/{tenant_id}` (`updateTenant`)
  - `DELETE /api/account-management/v1/tenants/{tenant_id}` (`deleteTenant`)
  - `GET /api/account-management/v1/tenants/{tenant_id}/children` (`listChildren`)

- **Sequences**:

  - `cpt-cf-account-management-seq-create-child`

- **Data**:

  - `cpt-cf-account-management-dbtable-tenants`
  - `cpt-cf-account-management-dbtable-tenant-closure`
  - `cpt-cf-account-management-adr-resource-group-tenant-hierarchy-source`
  - `cpt-cf-account-management-adr-provisioning-excluded-from-closure`

### 2.3 [Tenant Type Enforcement](./features/feature-tenant-type-enforcement.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-tenant-type-enforcement`

- **Purpose**: Provides the in-service barrier that ensures only legal parent/child tenant-type relationships are persisted, evaluated against each tenant type's `allowed_parent_types` rules registered in GTS. The barrier is invoked by every hierarchy-mutating write path inside `TenantService` before any closure row is created and is re-evaluated whenever a mode conversion could change the structural trust boundary, so invalid topologies cannot be introduced either at creation or via post-creation transitions.

- **Depends On**: `cpt-cf-account-management-feature-tenant-hierarchy-management`, `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Evaluation of the GTS-registered type-compatibility matrix (`allowed_parent_types`) at the child-tenant create path.
  - Same-type nesting admitted when and only when the GTS type definition permits it, with acyclicity preserved by hierarchy invariants.
  - Pre-write barrier check that rejects illegal parent/child type pairings with a deterministic `validation` error before any `tenants` or `tenant_closure` row is written.
  - Re-evaluation of the type matrix prior to mode-conversion approval so that approval cannot persist an illegal topology that was already rejected at creation time.
  - Tenant-type definition envelope governed by the GTS `tenant_type.v1` schema.

- **Out of scope**:
  - Tenant creation, update, soft-delete, and closure maintenance (owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`).
  - The mode-conversion workflow itself, its state machine, and its REST surface (owned by `cpt-cf-account-management-feature-managed-self-managed-modes`).
  - Authoring or publishing tenant-type definitions into GTS (that is a deployment-seeding concern, not an AM runtime responsibility).
  - AuthZ policy evaluation and barrier enforcement on reads (handled by `PolicyEnforcer` / AuthZ Resolver / Tenant Resolver layers).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-type-enforcement`
  - [ ] `p1` - `cpt-cf-account-management-fr-tenant-type-nesting`

- **Design Principles Covered**:

  - none

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-account-management-constraint-gts-availability`

- **Domain Model Entities**:
  - TenantType
  - AllowedParentTypes (GTS-sourced compatibility matrix)

- **Design Components**:

  - none

- **API**:
  - none (enforcement is an internal pre-write barrier invoked by hierarchy writes; no dedicated REST surface)

- **Sequences**:

  - none

- **Data**:

  - [ ] `p1` - `gts://gts.x.core.am.tenant_type.v1~`

### 2.4 [Managed / Self-Managed Modes](./features/feature-managed-self-managed-modes.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-managed-self-managed-modes`

- **Purpose**: Delivers the managed vs self-managed tenant-mode model and the durable dual-consent conversion workflow that moves a tenant between those modes post-creation. Preserves the tenant-type-enforcement barrier across any conversion so the resulting topology remains legal, guarantees the single-pending-request invariant per tenant, and re-materializes the canonical `tenant_closure.barrier` column atomically with each approved mode flip so downstream isolation and metadata-inheritance semantics remain consistent at every commit.

- **Depends On**: `cpt-cf-account-management-feature-tenant-hierarchy-management`, `cpt-cf-account-management-feature-tenant-type-enforcement`, `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Mode selection at tenant creation — managed child creation and self-managed child creation — with creation-time self-managed declaration skipping the dual-consent workflow because the parent's explicit create call is the consent.
  - Post-creation managed/self-managed conversion workflow, including request initiation, counterparty decision, cancellation, rejection, expiry, and resolved-request retention; the detailed state machine is owned by the FEATURE artifact.
  - Parent-side inbound-discovery contract (`/tenants/{id}/child-conversions`) exposing only minimal conversion-request metadata across the self-managed barrier so the dual-consent workflow remains actionable without surfacing child tenant data.
  - Single-pending-request invariant for mode-conversion requests, surfaced as `409 conflict (pending_exists)` at the service layer.
  - Source-of-truth barrier-state update after approved mode changes so downstream isolation consumers observe the updated managed/self-managed topology.
  - Resolved-request retention window that keeps conversion history queryable on the default API surface before the tenant-retention cadence takes over.
  - Mixed-mode tenant trees — both modes coexisting in one hierarchy with `BarrierMode` enabling selective downstream barrier bypass for billing and administrative operations.
  - Barrier-aligned cross-tenant isolation: tenant A MUST NOT reach tenant B data across a self-managed barrier through any AM-owned access path.

- **Out of scope**:
  - Identity and authentication — `SecurityContext` validation, session, token issuance, and user/credential lifecycle (owned by the platform and the IdP contract).
  - Hierarchy mutations unrelated to mode transitions — tenant create/update/delete, closure table schema, and status (`active` ↔ `suspended`) transitions (owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`).
  - The tenant-type compatibility matrix itself — evaluation lives in `cpt-cf-account-management-feature-tenant-type-enforcement`; this feature only invokes it at approval time.
  - Downstream enforcement of `BarrierMode` on the hot read path — served by the Tenant Resolver Plugin; this feature is the source-of-truth writer, not the query-time enforcer.
  - Root-tenant mode conversion (explicitly forbidden — the root is never convertible).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-managed-tenant-creation`
  - [ ] `p1` - `cpt-cf-account-management-fr-self-managed-tenant-creation`
  - [ ] `p3` - `cpt-cf-account-management-fr-mode-conversion-approval`
  - [ ] `p3` - `cpt-cf-account-management-fr-mode-conversion-expiry`
  - [ ] `p3` - `cpt-cf-account-management-fr-mode-conversion-single-pending`
  - [ ] `p3` - `cpt-cf-account-management-fr-mode-conversion-consistent-apply`
  - [ ] `p3` - `cpt-cf-account-management-fr-conversion-creation-time-self-managed`
  - [ ] `p3` - `cpt-cf-account-management-fr-child-conversions-query`
  - [ ] `p3` - `cpt-cf-account-management-fr-conversion-cancel`
  - [ ] `p3` - `cpt-cf-account-management-fr-conversion-reject`
  - [ ] `p3` - `cpt-cf-account-management-fr-conversion-retention`
  - [ ] `p1` - `cpt-cf-account-management-nfr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-account-management-nfr-barrier-enforcement`
  - [ ] `p2` - `cpt-cf-account-management-nfr-tenant-model-versatility`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-account-management-principle-barrier-as-data`
  - `cpt-cf-account-management-adr-conversion-approval`

- **Design Constraints Covered**:

  - none

- **Domain Model Entities**:
  - ConversionRequest
  - TenantMode (managed / self-managed)
  - BarrierMode

- **Design Components**:

  - [ ] `p2` - `cpt-cf-account-management-component-conversion-service`

- **API**:
  - GET /tenants/{tenant_id}/conversions
  - POST /tenants/{tenant_id}/conversions
  - GET /tenants/{tenant_id}/conversions/{request_id}
  - PATCH /tenants/{tenant_id}/conversions/{request_id}
  - GET /tenants/{tenant_id}/child-conversions
  - POST /tenants/{tenant_id}/child-conversions
  - GET /tenants/{tenant_id}/child-conversions/{request_id}
  - PATCH /tenants/{tenant_id}/child-conversions/{request_id}

- **Sequences**:

  - `cpt-cf-account-management-seq-convert-dual-consent`

- **Data**:

  - `cpt-cf-account-management-dbtable-conversion-requests`

### 2.5 [IdP User Operations Contract](./features/feature-idp-user-operations-contract.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-idp-user-operations-contract`

- **Purpose**: Define the pluggable IdP user-operations contract that makes the configured IdP the source of truth for user identity and user-tenant binding, and expose that contract through Account Management's tenant-scoped user REST surface. The feature owns the `IdpProviderPluginClient` trait for user provisioning, deprovisioning, and tenant-scoped user query, together with the provisioning saga and compensation reaper that keep AM intent and IdP state aligned without AM ever becoming the system of record for user profiles or credentials. Concrete IdP adapter crates (Keycloak, Zitadel, Dex, etc.) are intentionally excluded — they conform to this contract but ship outside this module.

- **Depends On**: `cpt-cf-account-management-feature-tenant-hierarchy-management`, `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Pluggable `IdpProviderPluginClient` user-operations trait (contract surface) consumed by the user-service handlers and wired through ClientHub.
  - Tenant-scoped user provisioning: authenticated request → contract `provision_user` call → IdP becomes the SoT for the resulting user and its binding to the tenant.
  - Tenant-scoped user deprovisioning: contract `deprovision_user` call; already-absent IdP user is a successful no-op.
  - Tenant-scoped user query: list users in a tenant and point-existence checks used by other features (e.g. callers combine this with Resource Group membership operations).
  - IdP unavailability contract: user operations fail with `idp_unavailable` rather than serving stale data — AM holds no local user table, projection, or membership cache.
  - Three REST user operations layered on top of the contract (listed under API below).
  - User-identity schema reference (`gts://gts.x.core.am.user.v1~`) published for downstream consumers that need the user projection shape at tenant boundary.

- **Out of scope**:
  - Conforming IdP plugin implementations (e.g. Keycloak adapter, Zitadel adapter, Dex adapter) — these live in separate crates and are delivered outside this feature and this module.
  - Tenant-lifecycle IdP operations (tenant provisioning / deprovisioning and their failure contract) — those are side effects of tenant create/delete and are owned by `tenant-hierarchy-management`.
  - Token validation, session renewal, federation, credential policy, and MFA policy — inherited from the platform authorization architecture and the configured IdP provider, not owned here.
  - User-group orchestration, user-group membership, and nested user groups — owned by `user-groups` (which depends on this feature for user-existence checks).
  - Authorization policy evaluation for user-level operations — PEP / AuthZ Resolver concerns, not AM domain logic.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-idp-user-provision`
  - [ ] `p1` - `cpt-cf-account-management-fr-idp-user-deprovision`
  - [ ] `p1` - `cpt-cf-account-management-fr-idp-user-query`
  - [ ] `p1` - `cpt-cf-account-management-nfr-authentication-context`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-account-management-principle-idp-agnostic`
  - `cpt-cf-account-management-adr-idp-contract-separation`
  - `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`
  - `cpt-cf-account-management-adr-idp-user-tenant-binding`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-account-management-constraint-no-user-storage`
  - [ ] `p1` - `cpt-cf-account-management-constraint-legacy-integration`

- **Domain Model Entities**:
  - `User` (IdP-owned projection referenced via the published `gts://gts.x.core.am.user.v1~` schema — AM never persists this entity locally)
  - `UserTenantBinding` (logical relationship owned by the IdP — AM stores no local binding table; the binding is verified via the contract at operation time)
  - `TenantId` (value object consumed at the contract boundary to scope every user operation to a tenant)

- **Design Components**:

  - None.

- **API**:
  - GET /tenants/{tenant_id}/users (`listUsers`)
  - POST /tenants/{tenant_id}/users (`createUser`)
  - DELETE /tenants/{tenant_id}/users/{user_id} (`deleteUser`)

- **Sequences**:

  - None.

- **Data**:

  - None. Per `cpt-cf-account-management-constraint-no-user-storage`, this feature owns no AM-side tables — user identity state lives in the IdP; the published user projection schema is `gts://gts.x.core.am.user.v1~`.

### 2.6 [User Groups (via Resource Group delegation)](./features/feature-user-groups.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-user-groups`

- **Purpose**: Orchestrate user groups entirely through delegation to the Resource Group module. Account Management registers the chained user-group type schema `gts.x.core.rg.type.v1~x.core.am.user_group.v1~` during module initialization, triggers cascade cleanup of user groups during tenant hard-deletion, and exposes AM's user-query surface so callers can combine user existence checks with Resource Group's membership operations. Account Management deliberately does not proxy CRUD or membership calls and owns no user-group tables — all group hierarchy, membership storage, cycle detection, and tenant-scoped isolation are performed by Resource Group.

- **Depends On**: `cpt-cf-account-management-feature-tenant-hierarchy-management`, `cpt-cf-account-management-feature-idp-user-operations-contract`, `cpt-cf-account-management-feature-errors-observability`

- **Scope**:
  - Chained RG type-schema registration during AM module init for `gts.x.core.rg.type.v1~x.core.am.user_group.v1~`, with `allowed_memberships = [gts.x.core.am.user.v1~]` and `allowed_parents` permitting self-nesting for nested user groups.
  - Account-Management-side cascade cleanup trigger during tenant hard-deletion so Resource Group can remove the tenant's user-group subtree before the tenant row is deleted.
  - Exposure of AM's tenant-scoped user-query capability (from feature 5) as the valid user set that callers combine with Resource Group membership operations.
  - Documented delegation contract: consumers call `ResourceGroupClient` directly for group and membership operations; AM does not proxy.

- **Out of scope**:
  - Resource Group storage: the `user_group_*` tables and any other user-group persistence are OWNED by the Resource Group module, not by account-management.
  - The Resource Group engine itself (generic RG CRUD, type registry machinery, RG cascade engine, forest invariants, cycle detection, tenant-scoped isolation enforcement) — account-management only DELEGATES to it.
  - REST endpoints for group create/update/delete, membership add/remove, or nested-group traversal — none live in the AM OpenAPI surface; callers use Resource Group's API directly.
  - User identity operations (provisioning, deprovisioning, existence checks) — owned by `idp-user-operations-contract`.
  - Tenant lifecycle, tenant hierarchy, and tenant-closure ownership — owned by `tenant-hierarchy-management`.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-user-group-rg-type`
  - [ ] `p1` - `cpt-cf-account-management-fr-user-group-lifecycle`
  - [ ] `p1` - `cpt-cf-account-management-fr-user-group-membership`
  - [ ] `p1` - `cpt-cf-account-management-fr-nested-user-groups`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-account-management-principle-delegation-to-rg`

- **Design Constraints Covered**:

  - None.

- **Domain Model Entities**:
  - `UserGroup` (delegated view/adapter — delegated to Resource Group; not owned here)
  - `UserGroupMembership` (delegated adapter — delegated to Resource Group; not owned here)
  - Chained type-schema reference `gts://gts.x.core.rg.type.v1~x.core.am.user_group.v1~` (registered by AM, schema body published for RG-side validation; AM stores no instance rows)

- **Design Components**:

  - None.

- **API**:
  - No AM-facing REST endpoints for user groups; consumers call `ResourceGroupClient` directly per the Delegation-to-RG principle.

- **Sequences**:

  - None.

- **Data**:

  - _RG-owned: `user_group_*`, `user_group_membership_*` — not owned by account-management. No account-management-owned tables for this feature._

### 2.7 [Tenant Metadata](./features/feature-tenant-metadata.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-account-management-feature-tenant-metadata`

- **Purpose**: Provides the extensible tenant-metadata subsystem: GTS-registered metadata schemas, tenant-scoped CRUD, a direct-on-tenant listing surface, and barrier-aware effective-value resolution driven by each schema's `inheritance_policy` trait. The feature encapsulates `MetadataService`, the `tenant_metadata` storage table, and the `/tenants/{tenant_id}/metadata` REST family so that new metadata categories (branding, billing contacts, future tenant attributes) can be introduced by registering GTS schemas without AM code changes. Inheritance resolution walks ancestors via `parent_id` and stops at self-managed barriers, preserving the core isolation invariant without requiring `BarrierMode::Ignore` for metadata reads.

- **Depends On**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` (owns `tenants` + `tenant_closure`, `parent_id` walk-up primitives, and the paginated children surface; tenant removal triggers the tenant-metadata cascade-delete contract).
  - `cpt-cf-account-management-feature-managed-self-managed-modes` (owns the `self_managed` flag / barrier column whose value is the stop condition for the inheritance walk-up).
  - `cpt-cf-account-management-feature-errors-observability` (consumes the Problem Details taxonomy — `metadata_schema_not_registered`, `metadata_entry_not_found`, `validation`, `cross_tenant_denied` — and the `metadata resolution` metric family).

- **Scope**:
  - `MetadataService` component: metadata CRUD keyed by `(tenant_id, schema_uuid)`, direct-on-tenant listing (`list_for_tenant`), and hierarchy-aware effective-value resolution (`resolve`).
  - GTS-registered metadata schemas with the `inheritance_policy` trait (`override_only` | `inherit`, default `override_only`) resolved from `x-gts-traits` via the same GTS-traits path tenant types use — no service-local policy table.
  - Deterministic UUIDv5 derivation of `schema_uuid` from the public `schema_id`, plus reverse-hydration of `schema_uuid` back to `schema_id` when building response payloads.
  - Hierarchy walk-up resolution via `parent_id` that stops at the nearest self-managed ancestor (barrier-stop) and skips `suspended` tenants without stopping; empty resolution is the normal terminal state of the walk (not a `not_found`).
  - REST surface `/api/account-management/v1/tenants/{tenant_id}/metadata` (list + per-schema GET/PUT/DELETE + `/resolved` read) with tenant-scope filtering applied by the platform layer, so self-managed barriers apply without AM-specific logic on list.
  - Per-schema AuthZ: `schema_id` is carried in the AuthZ request so policy authors can scope metadata permissions by category.
  - Distinct 404 sub-codes for unregistered schema vs. missing tenant entry (`metadata_schema_not_registered` vs. `metadata_entry_not_found`).
  - Cascade deletion of all tenant metadata entries when the tenant row is removed.

- **Out of scope**:
  - Tenant-hierarchy traversal primitives and ownership of `tenants` / `tenant_closure` — owned by `tenant-hierarchy-management`.
  - Barrier state and `self_managed` flag writes / mode conversion — owned by `managed-self-managed-modes`.
  - GTS types-registry availability and tenant-type schema registration — owned by `tenant-type-enforcement`.
  - Problem Details envelope shape, RFC 9457 formatting, and the authoritative error-code taxonomy — owned by `errors-observability`.
  - Metadata-schema authoring or interpretation of value semantics — schemas are GTS-registered; `MetadataService` treats values as opaque GTS-validated payloads.
  - Materialized inheritance views, DB-level CHECK/trigger enforcement, or reconciliation jobs — inheritance is derived on every read (per ADR-0002).

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-account-management-fr-tenant-metadata-schema`
  - [ ] `p2` - `cpt-cf-account-management-fr-tenant-metadata-crud`
  - [ ] `p2` - `cpt-cf-account-management-fr-tenant-metadata-api`
  - [ ] `p2` - `cpt-cf-account-management-fr-tenant-metadata-list`
  - [ ] `p2` - `cpt-cf-account-management-fr-tenant-metadata-permissions`

- **Design Principles Covered**:

  - None (no principle rows assigned to `tenant-metadata` in the Phase-2 feature-map; the feature inherits the "Source-of-Truth" and "Tree Invariant" principles transitively from `tenant-hierarchy-management`).

- **Design Constraints Covered**:

  - `cpt-cf-account-management-adr-metadata-inheritance` (ADR-0002 — barrier-aware walk-up: inheritance is enforced application-only inside `MetadataService::resolve`; no DB trigger/materialized column; direct reads return only directly-written values).

- **Domain Model Entities**:
  - `TenantMetadataEntry` — the `(tenant_id, schema_uuid, value)` row stored in `tenant_metadata`; `schema_uuid` is a UUIDv5 derived from the public GTS `schema_id`.
  - `MetadataSchema` (external, GTS-registered) — referenced by public `schema_id`; carries `x-gts-traits.inheritance_policy` (`override_only` | `inherit`) consumed by `MetadataService`.
  - `ResolvedMetadataValue` — the effective value returned by `/resolved`; for `inherit` schemas, the result of walking `parent_id` ancestors and stopping at self-managed barriers (may be empty, which is the normal terminal state of the walk).

- **Design Components**:

  - [ ] `p2` - `cpt-cf-account-management-component-metadata-service`

- **API**:
  - `GET /api/account-management/v1/tenants/{tenant_id}/metadata` (`listTenantMetadata`) — paginated list of direct-on-tenant metadata entries (does not walk ancestors).
  - `GET /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` (`getTenantMetadata`) — read a single direct entry; 404 distinguishes `metadata_schema_not_registered` vs. `metadata_entry_not_found`.
  - `PUT /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` (`putTenantMetadata`) — upsert validated against the registered GTS schema.
  - `DELETE /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` (`deleteTenantMetadata`) — remove a direct entry (does not affect ancestor values).
  - `GET /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}/resolved` (`resolveTenantMetadata`) — effective-value resolution; applies the schema's `inheritance_policy` trait with barrier-stop at self-managed ancestors.

- **Sequences**:

  - `cpt-cf-account-management-seq-resolve-metadata`

- **Data**:

  - `cpt-cf-account-management-dbtable-tenant-metadata` (tenant-scoped metadata storage; the table stores only directly-written values, per ADR-0002).
  - [ ] `p2` - `gts://gts.x.core.am.tenant_metadata.v1~` (base envelope schema with `x-gts-traits-schema.inheritance_policy`; derived metadata schemas are GTS-registered at runtime).

### 2.8 [Errors & Observability](./features/feature-errors-observability.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-account-management-feature-errors-observability`

- **Purpose**: Cross-cutting foundation that all other AM features consume: the RFC 9457 Problem Details envelope, the stable public error-code taxonomy, and the domain-specific observability metric catalog. Standardizes how AM surfaces failures (so clients and operators react consistently across tenant models and IdP providers) and how AM exposes domain signals (dependency health, metadata resolution, bootstrap lifecycle, tenant-retention work, conversion lifecycle, hierarchy-depth threshold exceedance, cross-tenant denials). Also carries the cross-cutting audit, compatibility, data-classification, reliability, security-context, versioning, and data-handling policies that other features must uphold.

- **Depends On**: None (foundation feature — every other feature depends on this one transitively; it has no feature-level upstream).

- **Scope**:
  - Stable public error categories per PRD §5.8: `validation`, `not_found`, `conflict`, `cross_tenant_denied`, `idp_unavailable`, `idp_unsupported_operation`, `service_unavailable`, `internal` (8 public categories; ≥ 6 required by acceptance).
  - Authoritative HTTP mapping and public `sub_code` identifiers per DESIGN §3.8 (e.g., `invalid_tenant_type`, `type_not_allowed`, `tenant_depth_exceeded`, `tenant_has_children`, `tenant_has_resources`, `root_tenant_cannot_delete`, `pending_exists`, `invalid_actor_for_transition`, `already_resolved`, `root_tenant_cannot_convert`, `metadata_schema_not_registered`, `metadata_entry_not_found`).
  - RFC 9457 Problem Details envelope: OpenAPI `Problem` schema defines the authoritative response shape consumed by every feature.
  - Observability metric families (7 required): dependency health, metadata resolution, bootstrap lifecycle, tenant-retention, conversion lifecycle, hierarchy-depth threshold exceedance, cross-tenant denials.
  - Platform-aligned metric naming and exposure conventions; boundary between platform-provided and module-internal metrics kept implementation-side.
  - Ops-metrics treatment: dashboard / alerting integration policy covering which domain metrics back SLO/alert rules, naming alignment with the platform metric catalog, and the contract for on-call escalation paths sourced from this feature's metric families (see footnote [a]).
  - Cross-cutting policy surfaces: audit-trail completeness (`actor=system` for bootstrap completion, conversion expiry, provisioning-reaper, hard-delete/tenant-deprovision cleanup), SecurityContext requirement at every entry point, no in-module AuthZ evaluation, path-based API versioning + SDK/IdP contract-stability discipline, data-classification baseline (Internal/Confidential for hierarchy/mode data; PII-adjacent for opaque identity refs; per-schema for metadata), platform reliability SLA (99.9% uptime, RPO ≤ 1h, RTO ≤ 15m; IdP-outage degradation rules), and vendor/licensing hygiene.

- **Out of scope**:
  - Feature-specific error emission — each owning feature maps its own failure modes onto the taxonomy defined here (e.g., tenant-hierarchy emits `tenant_has_children`, modes emits `pending_exists`, metadata emits `metadata_schema_not_registered`).
  - Metric consumption, dashboards, alert-rule authoring — downstream of this feature.
  - Audit storage, retention, tamper resistance, and security-monitoring integration — inherited platform controls; this feature only emits and classifies events.
  - Token validation, session renewal, federation, MFA — inherited from platform AuthN; AM only requires that every entry point carry a validated `SecurityContext`.
  - Any domain data persistence — this feature owns no tables and no domain entities.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-account-management-fr-deterministic-errors`
  - [ ] `p1` - `cpt-cf-account-management-fr-observability-metrics`
  - [ ] `p1` - `cpt-cf-account-management-nfr-audit-completeness`
  - [ ] `p1` - `cpt-cf-account-management-nfr-compatibility`
  - [ ] `p2` - `cpt-cf-account-management-nfr-data-classification`
  - [ ] `p1` - `cpt-cf-account-management-nfr-reliability`
  - [ ] `p2` - `cpt-cf-account-management-nfr-ops-metrics-treatment`

- **Design Principles Covered**:

  - None (no principle rows assigned to `errors-observability` in the Phase-2 feature-map; the feature is a cross-cutting taxonomy/telemetry surface, not a domain principle).

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-account-management-constraint-security-context` (SecurityContext required at every entry point).
  - [ ] `p1` - `cpt-cf-account-management-constraint-no-authz-eval` (AM is not a PEP; authorization evaluation is delegated to the platform AuthZ layer).
  - [ ] `p1` - `cpt-cf-account-management-constraint-versioning-policy` (path-based REST versioning; SDK/IdP contract stability).
  - [ ] `p2` - `cpt-cf-account-management-constraint-data-handling` (data-classification baselines and regulatory-compliance posture).
  - [ ] `p2` - `cpt-cf-account-management-constraint-vendor-licensing` (supply-chain / vendor-licensing hygiene for AM dependencies).

- **Domain Model Entities**:
  - cross-cutting: no domain entities.

- **Design Components**:

  - None (no component rows assigned to `errors-observability` in the Phase-2 feature-map; error semantics and metrics are horizontal concerns emitted by every component rather than owned by a dedicated one).

- **API**:
  - None (this feature defines the Problem Details envelope and metric catalog that other features' endpoints and emitters consume; it owns no REST path or CLI command of its own).

- **Sequences**:

  - None (no sequence rows assigned to `errors-observability` in the Phase-2 feature-map; error/metric flows are emitted inline by the sequences owned by each functional feature).

- **Data**:

  - None (foundation feature — no dbtable and no GTS schema are owned here; per acceptance rules Feature 8 MUST NOT own a dbtable).

> [a] **Reconciliation note**: Phase 2's feature-map (§1.2) assigns
> `cpt-cf-account-management-nfr-ops-metrics-treatment` to
> `errors-observability`, but the Phase 6 feature-entry draft
> (`out/phase-06-features-7-8.md`) omitted it from the "Requirements
> Covered" list. Dashboard/alerting integration is naturally part of
> this feature's observability scope (per Option B intent), so the NFR
> has been restored to §2.8 during Phase 8 assembly. The Scope bullet
> "Ops-metrics treatment" above documents the integration contract.
> This keeps the Phase 2 grand total of 151 inventoried IDs covered
> with no gaps.

### 2.9 Tenant Resolver Plugin — defined in sub-system DECOMPOSITION

Feature `cpt-cf-tr-plugin-feature-tenant-resolver-plugin` is the sole feature of the `cf-tr-plugin` sub-system and is therefore **defined authoritatively** in the child [tr-plugin DECOMPOSITION](./tr-plugin/DECOMPOSITION.md), not here. This parent DECOMPOSITION references it only in §3 Feature Dependencies as a cross-system leaf on the `tenant-hierarchy-management` branch, preserving the whole-module dependency view.

**Why split**: the Cypilot registry (`.cypilot/config/artifacts.toml`) models `tr-plugin` as a distinct sub-system (`systems.autodetect.children` block under `cf`). Per registry semantics, feature IDs carrying the `cpt-cf-tr-plugin-*` prefix must be defined within the sub-system's own artifact tree to keep the parent system's autodetect scan consistent. Merging both namespaces in a single DECOMPOSITION file triggers a registry-level validation error ("Inconsistent systems in IDs"). The split satisfies the registry without losing the whole-system dependency view, which is re-assembled in §3 below.

**Where to find it**: [`tr-plugin/DECOMPOSITION.md`](./tr-plugin/DECOMPOSITION.md) §2.1 contains the full feature entry (Purpose, Depends On, Scope, Out of scope, Requirements Covered, Design Principles/Constraints Covered, Domain Model Entities, Design Components, API, Sequences, Data). The hard dependency is `cpt-cf-account-management-feature-tenant-hierarchy-management` (hierarchy owner of `tenants` + `tenant_closure`); informational upstream influences come from `managed-self-managed-modes` (barrier semantics) and `errors-observability` (error/telemetry conventions).

**Legacy boilerplate suppressed** (removed Purpose/Scope/Out-of-scope body that lived inline here in the pre-split draft — every field is canonically located in the sub-system DECOMPOSITION; editing this file will NOT update the feature contract).

---

## 3. Feature Dependencies

```text
cpt-cf-account-management-feature-errors-observability        (foundation — no deps)
    ↓
cpt-cf-account-management-feature-platform-bootstrap          (deps: errors-observability)
    ↓
cpt-cf-account-management-feature-tenant-hierarchy-management (deps: platform-bootstrap, errors-observability)
    ↓
    ├─→ cpt-cf-account-management-feature-tenant-type-enforcement
    │       (deps: tenant-hierarchy-management, errors-observability)
    │       ↓
    │       └─→ cpt-cf-account-management-feature-managed-self-managed-modes
    │               (deps: tenant-hierarchy-management, tenant-type-enforcement, errors-observability)
    │                   ↓
    │                   └─→ cpt-cf-account-management-feature-tenant-metadata
    │                           (deps: tenant-hierarchy-management, managed-self-managed-modes, errors-observability)
    ├─→ cpt-cf-tr-plugin-feature-tenant-resolver-plugin
    │       (deps: tenant-hierarchy-management)
    ├─→ cpt-cf-account-management-feature-idp-user-operations-contract
    │       (deps: tenant-hierarchy-management, errors-observability)
    │       ↓
    │       └─→ cpt-cf-account-management-feature-user-groups
    │               (deps: idp-user-operations-contract, tenant-hierarchy-management, errors-observability)
```

**Dependency Rationale**:

- `cpt-cf-account-management-feature-errors-observability` is the
  **foundation**: it defines the RFC 9457 Problem Details envelope, the
  stable public error-code taxonomy, and the domain-specific metric
  catalog that every other feature emits into. It also carries the
  cross-cutting SecurityContext-at-entry-point, no-in-module-AuthZ,
  versioning, audit, data-classification, reliability, and
  ops-metrics-treatment policies. No feature-level upstream — transitively
  depended on by all eight other features.

- `cpt-cf-account-management-feature-platform-bootstrap` requires
  `errors-observability`: bootstrap emits audit (`actor=system` for
  bootstrap completion), metrics (bootstrap lifecycle family), and
  deterministic error codes (e.g. `idp_unavailable` during the IdP
  wait). Bootstrap has no other feature-level dependency because it
  authors the root-tenant row that all hierarchy-consuming features
  subsequently read.

- `cpt-cf-account-management-feature-tenant-hierarchy-management`
  requires `platform-bootstrap` (every child tenant is created under an
  existing root published by bootstrap) and `errors-observability`
  (hierarchy emits `tenant_has_children`, `tenant_depth_exceeded`,
  audit events for status transitions and hard-delete/IdP-deprovision
  cleanup, and hierarchy-depth / retention metric families).

- `cpt-cf-account-management-feature-tenant-type-enforcement` requires
  `tenant-hierarchy-management` (the type barrier is invoked inline by
  `TenantService` before any `tenants` or `tenant_closure` row is
  written) and `errors-observability` (emits deterministic `validation`
  errors with `invalid_tenant_type` / `type_not_allowed` sub-codes).

- `cpt-cf-account-management-feature-managed-self-managed-modes`
  requires `tenant-hierarchy-management` (writes the `barrier` column
  on the hierarchy-owned `tenant_closure` table during approval;
  reads the canonical tenant row to flip `tenants.self_managed`),
  `tenant-type-enforcement` (re-evaluates the type-compatibility
  matrix prior to approval so the resulting topology stays legal), and
  `errors-observability` (emits `pending_exists`,
  `invalid_actor_for_transition`, `already_resolved`,
  `root_tenant_cannot_convert`, plus the conversion-lifecycle metric
  family).

- `cpt-cf-account-management-feature-idp-user-operations-contract`
  requires `tenant-hierarchy-management` (every user operation is
  tenant-scoped by `tenant_id`, which must resolve to an existing AM
  tenant row) and `errors-observability` (emits `idp_unavailable`,
  `idp_unsupported_operation`, and the dependency-health metric
  family).

- `cpt-cf-account-management-feature-user-groups` requires
  `idp-user-operations-contract` (callers combine AM's user-query
  surface with Resource-Group membership ops — user existence is
  checked via the IdP contract), `tenant-hierarchy-management` (the
  tenant row anchors the group subtree and hard-delete triggers
  cascade cleanup), and `errors-observability` (cross-cutting error
  mapping for the RG delegation path).

- `cpt-cf-account-management-feature-tenant-metadata` requires
  `tenant-hierarchy-management` (tenant removal triggers metadata
  cleanup; the inheritance walk-up reuses
  `parent_id`), `managed-self-managed-modes` (the inheritance walk-up
  terminates at self-managed barriers — the stop condition reads the
  `self_managed` flag written by the modes feature), and
  `errors-observability` (emits
  `metadata_schema_not_registered` vs `metadata_entry_not_found` 404
  sub-codes and the metadata-resolution metric family). Note:
  `tenant-type-enforcement` is mentioned only as informational context
  in the §2.7 scope because metadata reuses the same GTS-traits
  resolution pattern; the Phase-2 feature-map authoritative edge set
  lists only the three hard edges above, so that GTS coupling is a
  code-sharing detail rather than a cross-feature dependency.

- `cpt-cf-tr-plugin-feature-tenant-resolver-plugin` requires
  `tenant-hierarchy-management` as its only hard data-level dependency:
  it reads the canonical `tenants` + `tenant_closure` tables, including
  the denormalized `barrier` and `descendant_status` columns. The plugin
  also consumes informational semantics from `managed-self-managed-modes`
  (meaning of the barrier column) and `errors-observability` (error and
  telemetry conventions), but those are not hard DAG edges in either this
  parent view or the child `tr-plugin/DECOMPOSITION.md`. The plugin is a
  **leaf** in the DAG with no downstream consumers inside this module.

**Parallel-development opportunities** (siblings under the same parent
with no cross-edges, derivable from the DAG):

- `tenant-type-enforcement`, `idp-user-operations-contract`, and
  `tenant-resolver-plugin` are direct children of
  `tenant-hierarchy-management` with no hard cross-edge between them —
  they can be developed in parallel after the hierarchy feature lands
  (and after their shared `errors-observability` dependency where
  applicable).
- `tenant-metadata` is a direct child of `managed-self-managed-modes`
  and can proceed after modes lands and its other hard dependencies —
  hierarchy + errors-obs — are satisfied.
- `user-groups` sits on its own branch through
  `idp-user-operations-contract` and is independent of the
  type/modes/metadata/TRP sub-DAG; it can proceed in parallel with
  those once hierarchy + IdP-user contract are ready.

**Foundation features (no dependencies)**: `errors-observability` only.
(`platform-bootstrap` depends on `errors-observability` per Phase 2 §4
authoritative edge list.)

**Leaf features (no downstream consumers inside this module)**:
`user-groups`, `tenant-metadata`, `tenant-resolver-plugin`.

**Cycle check**: no cycles — the DAG is acyclic by construction
(each edge goes strictly down through PR1 → PR2 → PR3 feature bundles
per Strategy C, with the only cross-bundle edges being the PR1→PR2 and
PR1→PR3 data-ownership edges anchored in
`tenant-hierarchy-management`).
