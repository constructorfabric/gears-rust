# Feature: Errors & Observability


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Error Surface](#error-surface)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Error-to-Problem Mapping](#error-to-problem-mapping)
  - [Metric Emission](#metric-emission)
  - [Audit Event Emission](#audit-event-emission)
  - [SecurityContext Gate](#securitycontext-gate)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Error Taxonomy and RFC 9457 Envelope](#error-taxonomy-and-rfc-9457-envelope)
  - [Observability Metric Catalog](#observability-metric-catalog)
  - [Audit Contract and `actor=system` Emission](#audit-contract-and-actorsystem-emission)
  - [SecurityContext Gate at Every Entry Point](#securitycontext-gate-at-every-entry-point)
  - [Versioning Discipline](#versioning-discipline)
  - [Data Classification Baseline](#data-classification-baseline)
  - [Reliability Inheritance](#reliability-inheritance)
  - [Ops-Metrics Treatment](#ops-metrics-treatment)
  - [Vendor and Licensing Hygiene](#vendor-and-licensing-hygiene)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-errors-observability`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-errors-observability`
## 1. Feature Context

### 1.1 Overview

Cross-cutting foundation feature that every other AM feature consumes: the RFC 9457 Problem Details envelope, the stable public error-code taxonomy, the domain-specific observability metric catalog, and the audit / SecurityContext / versioning / data-classification / reliability policies those features must honour. This FEATURE defines *contracts and catalogs*; individual emit points live in the feature that owns each code path.

### 1.2 Purpose

Standardizes how AM surfaces failures (so clients and operators react consistently across tenant models and IdP providers) and how AM exposes domain signals (dependency health, metadata resolution, bootstrap lifecycle, tenant-retention work, conversion lifecycle, hierarchy-depth threshold exceedance, cross-tenant denials). Also carries the cross-cutting audit-completeness, compatibility, data-classification, reliability, security-context, and versioning policies other features must uphold.

**Requirements**: `cpt-cf-account-management-fr-deterministic-errors`, `cpt-cf-account-management-fr-observability-metrics`, `cpt-cf-account-management-nfr-audit-completeness`, `cpt-cf-account-management-nfr-compatibility`, `cpt-cf-account-management-nfr-data-classification`, `cpt-cf-account-management-nfr-reliability`, `cpt-cf-account-management-nfr-ops-metrics-treatment`

**Principles**: None. Matches DECOMPOSITION §2.8 — no principle rows were assigned to `errors-observability` in the Phase-2 feature-map because this feature is a cross-cutting taxonomy / telemetry surface rather than a domain principle.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Client representative for the error-surface flow: receives Problem Details envelopes in response to failing API requests; observes domain metrics indirectly through operator dashboards. |
| `cpt-cf-account-management-actor-platform-admin` | Operator for the metric and audit surfaces: consumes the metric catalog through platform observability tooling, reads audit events through the platform audit sink, and owns alert-rule authoring against the metric families defined here. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.8 Deterministic Error Semantics + §5.9 Observability Metrics + §6.4 Audit Trail Completeness + §6.7 API and SDK Compatibility + §6.9 Data Classification + §6.10 Reliability
- **Design**: [DESIGN.md](../DESIGN.md) §3.8 Error Codes Reference + §4.2 Security Architecture + §4.1 Applicability and Delegations
- **OpenAPI**: [account-management-v1.yaml](../account-management-v1.yaml) — authoritative `Problem` schema defining the response envelope
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.8 Errors & Observability
- **Dependencies**: None (foundation feature — every other feature depends on this one transitively)

## 2. Actor Flows (CDSL)

One generic flow models how a domain failure surfaces from any AM feature's code path to the client through the envelope defined here. Feature-specific failure modes (e.g., `tenant_has_children`, `pending_exists`, `metadata_schema_not_registered`) are emitted by their owning features; this flow shows the shared classification / envelope / emission path.

### Error Surface

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-errors-observability-error-surface`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Domain failure classified, mapped to an RFC 9457 Problem envelope, and returned to the client with the correct HTTP status plus stable non-null `code` and `sub_code` fields.
- Cross-tenant denial surfaces as `cross_tenant_denied` (HTTP 403) without leaking the existence or attributes of the target resource beyond the stable sub-code.
- IdP contract failure surfaces as `idp_unavailable` (HTTP 503) with deterministic retry semantics per PRD §6.10.

**Error Scenarios**:

- Entry point reached without a valid `SecurityContext`: request is rejected by the SecurityContext gate before any domain logic runs, short-circuiting with a platform-standard auth error (not re-classified by this feature).
- Unexpected (unclassified) domain error: falls through to `internal` (HTTP 500) with a generic public body while the detailed diagnostic goes only to the audit trail.

**Steps**:

> At every REST handler, SDK boundary, and inter-module ClientHub contract, the SecurityContext gate **MUST** run before any domain logic: a missing or invalid context short-circuits with the platform-standard auth error, so domain errors are never raised, classified, or mapped for unauthenticated callers.

1. [ ] - `p1` - Validate caller's `SecurityContext` via `algo-security-context-gate` at the entry point (REST handler, SDK boundary, or inter-module ClientHub contract) before any domain logic executes - `inst-flow-errsurf-securitycontext-gate`
2. [ ] - `p1` - Feature code path raises a domain error (e.g., `TenantHasChildren`, `PendingExists`, `IdPUnavailable`) - `inst-flow-errsurf-raise`
3. [ ] - `p1` - Classify the domain error and map to Problem Details envelope via `algo-error-to-problem-mapping` - `inst-flow-errsurf-classify-and-map`
4. [ ] - `p1` - Emit domain metric via `algo-metric-emission` using the appropriate metric family for the failure mode (e.g., `dependency_health`, `hierarchy_depth_exceedance`, `cross_tenant_denial`) - `inst-flow-errsurf-metric-emit`
5. [ ] - `p1` - **IF** the failure is a state-changing or `actor=system`-eligible condition (per `nfr-audit-completeness`) - `inst-flow-errsurf-audit-branch`
   1. [ ] - `p1` - Emit audit event via `algo-audit-emission` with correct actor attribution (tenant identity or `actor=system`) - `inst-flow-errsurf-audit-emit`
6. [ ] - `p1` - **RETURN** Problem envelope with HTTP status from the category→status mapping and stable non-null `code` + `sub_code` fields - `inst-flow-errsurf-return`

## 3. Processes / Business Logic (CDSL)

### Error-to-Problem Mapping

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping`

**Input**: Domain error instance (class or kind, plus feature-specific diagnostic fields)

**Output**: RFC 9457 Problem Details envelope — `{ type, title, status, code, sub_code, detail?, instance? }` — where `code` is one of the 8 public categories and `sub_code` is a non-null stable public discriminator from DESIGN §3.8 / OpenAPI. When no finer feature-specific discriminator applies, `sub_code` uses the matching category discriminator (`validation`, `not_found`, `service_unavailable`, `internal`, etc.).

**Steps**:

> This algorithm enumerates the 8 public categories from PRD §5.8 exhaustively. Any domain error not matched by steps 2–8 **MUST** fall through to `internal` via step 9 to preserve public-contract stability; the unmatched diagnostic detail is preserved in the audit trail, not the public Problem body.

1. [ ] - `p1` - Identify the domain error's kind from its type or diagnostic tag - `inst-algo-etp-identify-kind`
2. [ ] - `p1` - **IF** kind maps to `validation` (invalid input, schema validation failure, missing required field, `invalid_tenant_type`, `root_tenant_cannot_delete`, `root_tenant_cannot_convert`) - `inst-algo-etp-validation`
   1. [ ] - `p1` - **RETURN** Problem with `code=validation`, HTTP 422, populated `sub_code` per DESIGN §3.8 - `inst-algo-etp-validation-return`
3. [ ] - `p1` - **IF** kind maps to `not_found` (tenant, group, user, metadata schema or entry not found; distinguished by `metadata_schema_not_registered` vs `metadata_entry_not_found` sub-codes) - `inst-algo-etp-not-found`
   1. [ ] - `p1` - **RETURN** Problem with `code=not_found`, HTTP 404, populated `sub_code` per DESIGN §3.8 - `inst-algo-etp-not-found-return`
4. [ ] - `p1` - **IF** kind maps to `conflict` (`type_not_allowed`, `tenant_depth_exceeded`, `tenant_has_children`, `tenant_has_resources`, `pending_exists`, `invalid_actor_for_transition`, `already_resolved`) - `inst-algo-etp-conflict`
   1. [ ] - `p1` - **RETURN** Problem with `code=conflict`, HTTP 409, populated `sub_code` per DESIGN §3.8 - `inst-algo-etp-conflict-return`
5. [ ] - `p1` - **IF** kind maps to `cross_tenant_denied` (barrier violation, unauthorized cross-tenant access, non-platform-admin attempting root-tenant-scoped operations) - `inst-algo-etp-xtd`
   1. [ ] - `p1` - **RETURN** Problem with `code=cross_tenant_denied`, HTTP 403; body **MUST NOT** leak target-resource attributes beyond the stable sub-code - `inst-algo-etp-xtd-return`
6. [ ] - `p1` - **IF** kind maps to `idp_unavailable` (IdP contract call failed or timed out) - `inst-algo-etp-idp-unavail`
   1. [ ] - `p1` - **RETURN** Problem with `code=idp_unavailable`, HTTP 503, `sub_code=idp_unavailable` - `inst-algo-etp-idp-unavail-return`
7. [ ] - `p1` - **IF** kind maps to `idp_unsupported_operation` (IdP implementation does not support the requested administrative operation) - `inst-algo-etp-idp-unsup`
   1. [ ] - `p1` - **RETURN** Problem with `code=idp_unsupported_operation`, HTTP 501, `sub_code=idp_unsupported_operation` - `inst-algo-etp-idp-unsup-return`
8. [ ] - `p1` - **IF** kind maps to `service_unavailable` (AM DB unavailable, Resource Group unavailable during deletion validation/cleanup, GTS unavailable where the caller delegates classification here, AuthZ/PEP unavailable, or AM itself temporarily unavailable) - `inst-algo-etp-svc-unavail`
   1. [ ] - `p1` - **RETURN** Problem with `code=service_unavailable`, HTTP 503, `sub_code=service_unavailable` - `inst-algo-etp-svc-unavail-return`
9. [ ] - `p1` - **ELSE** unclassified domain error (fallthrough to preserve contract stability) - `inst-algo-etp-fallthrough`
   1. [ ] - `p1` - Record the full diagnostic detail in the audit trail (not the public Problem body) via `algo-audit-emission` - `inst-algo-etp-fallthrough-audit`
   2. [ ] - `p1` - **RETURN** Problem with `code=internal`, HTTP 500, generic `sub_code=internal`; public body **MUST NOT** disclose diagnostic internals - `inst-algo-etp-fallthrough-return`

### Metric Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-metric-emission`

**Input**: Metric family identifier (one of the 7 AM metric families from PRD §5.9), metric kind (counter, gauge, histogram), and labeled dimension set

**Output**: Metric sample emitted through the platform observability plumbing; no return value to the caller

**Steps**:

1. [ ] - `p1` - Resolve the metric family to its platform-aligned canonical name per the metric catalog; the authoritative name-alignment contract is owned by `dod-ops-metrics-treatment` (concrete names are deployment-specific and may carry prefixes like `am.bootstrap_lifecycle` or unprefixed forms like `bootstrap.*` depending on the platform observability convention) - `inst-algo-metric-resolve-family`
2. [ ] - `p1` - Validate that the supplied labels are members of the family's declared label set (cardinality guardrail) - `inst-algo-metric-validate-labels`
3. [ ] - `p1` - **IF** any label value would introduce unbounded cardinality (e.g., raw tenant UUID on a wide-label metric) - `inst-algo-metric-cardinality-guard`
   1. [ ] - `p1` - Truncate or hash the offending label per the family's cardinality policy - `inst-algo-metric-cardinality-truncate`
4. [ ] - `p1` - Emit the metric sample through the platform meter provider with the validated label set - `inst-algo-metric-emit-sample`
5. [ ] - `p1` - **RETURN** — metric emission is fire-and-forget; callers MUST NOT block on emission - `inst-algo-metric-return`

### Audit Event Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-audit-emission`

**Input**: Audit event kind, actor attribution (`actor=<tenant-scoped-identity>` or `actor=system` for module-owned background transitions), tenant identity, and structured payload

**Output**: Audit record persisted through the platform audit sink; no return value to the caller

**Steps**:

1. [ ] - `p1` - **IF** caller has a valid `SecurityContext` - `inst-algo-audit-actor-from-ctx`
   1. [ ] - `p1` - Set `actor` to the `SecurityContext`'s tenant-scoped identity - `inst-algo-audit-actor-tenant-scoped`
2. [ ] - `p1` - **ELSE IF** event kind is one of the AM-owned background transitions enumerated in `nfr-audit-completeness` (bootstrap completion, conversion expiry, provisioning-reaper compensation, hard-delete / tenant-deprovision cleanup) - `inst-algo-audit-actor-system-eligible`
   1. [ ] - `p1` - Set `actor=system` - `inst-algo-audit-actor-system-set`
3. [ ] - `p1` - **ELSE** caller-less event whose kind is not in the `actor=system` allow-list - `inst-algo-audit-actor-unauthorized`
   1. [ ] - `p1` - **RETURN** — short-circuit to the platform-standard authentication-error path (`algo-security-context-gate` step 2.1); do **not** emit an audit record under `actor=system`, and do **not** fabricate a tenant-scoped identity - `inst-algo-audit-actor-short-circuit`
4. [ ] - `p1` - **IF** kind is a state-changing AM-owned transition (tenant create / status change / mode conversion / metadata write / hard-delete) - `inst-algo-audit-state-changing`
   1. [ ] - `p1` - Construct the audit record with `actor`, tenant identity, change details, and event kind per platform audit schema - `inst-algo-audit-construct-state`
5. [ ] - `p1` - **IF** kind is a `cross_tenant_denied` or `idp_unavailable` failure surfaced through the error-surface flow - `inst-algo-audit-failure`
   1. [ ] - `p1` - Construct the audit record carrying the full diagnostic detail suppressed from the public Problem body - `inst-algo-audit-construct-failure`
6. [ ] - `p1` - Emit through the platform audit sink; AM does **not** own storage, retention, tamper resistance, or security-monitoring integration (those are inherited platform controls per DESIGN §4.1) - `inst-algo-audit-emit`
7. [ ] - `p1` - **RETURN** — audit emission is fire-and-forget from the caller's perspective; delivery durability is a platform SLA - `inst-algo-audit-return`

### SecurityContext Gate

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-security-context-gate`

**Input**: Inbound request or inter-module invocation at an AM entry point (REST handler, SDK boundary, or ClientHub contract)

**Output**: Either authorization to proceed into domain logic, or a short-circuit platform-standard auth rejection

**Steps**:

1. [ ] - `p1` - Inspect request for an attached platform-provided `SecurityContext` - `inst-algo-sctx-inspect`
2. [ ] - `p1` - **IF** `SecurityContext` is absent or malformed (bootstrap / background-job paths are exempt and attach `actor=system` explicitly) - `inst-algo-sctx-missing`
   1. [ ] - `p1` - Short-circuit with the platform-standard authentication error (delegated to platform AuthN per `constraint-no-authz-eval` — AM does **not** mint its own auth error codes) - `inst-algo-sctx-short-circuit`
3. [ ] - `p1` - **ELSE** propagate the `SecurityContext` into downstream domain logic; `actor` derivation for audit events and policy evaluation flows from this context - `inst-algo-sctx-propagate`

## 4. States (CDSL)

**Not applicable.** This feature owns no entity with a lifecycle. Error taxonomy, metric catalog, and audit contract are declarative catalogs — they have no runtime state transitions. Every entity whose lifecycle intersects error handling (tenant, conversion request, metadata entry) has its state machine documented in the feature that owns it.

## 5. Definitions of Done

### Error Taxonomy and RFC 9457 Envelope

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-error-taxonomy-and-envelope`

The module **MUST** expose exactly the 8 stable public error categories from PRD §5.8 (`validation`, `not_found`, `conflict`, `cross_tenant_denied`, `idp_unavailable`, `idp_unsupported_operation`, `service_unavailable`, `internal`) and the stable public `sub_code` identifiers enumerated in DESIGN §3.8. Every failure response **MUST** carry the RFC 9457 Problem Details envelope with `code`, non-null `sub_code`, and the HTTP status mandated by the category→status mapping. Unclassified domain errors **MUST** fall through to `code=internal` and `sub_code=internal` rather than leaking new public codes.

**Implements**:

- `cpt-cf-account-management-flow-errors-observability-error-surface`
- `cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping`

**Touches**:

- Contract: RFC 9457 Problem schema in `account-management-v1.yaml`
- Modules: every feature's error boundary; this DoD is consumed transitively

### Observability Metric Catalog

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-metric-catalog`

The module **MUST** export the 7 domain-specific metric families required by PRD §5.9 (dependency health, metadata resolution, bootstrap lifecycle, tenant-retention, conversion lifecycle, hierarchy-depth threshold exceedance, cross-tenant denials). Metric names **MUST** align with platform observability naming conventions; label sets **MUST** be documented and cardinality-guarded so no metric exposes unbounded per-tenant or per-user dimensions without an explicit hashing policy.

| Family ID | Canonical family name | Kind(s) | Allowed labels | Cardinality guard | SLO / alert class | Runbook linkage |
|-----------|-----------------------|---------|----------------|-------------------|-------------------|-----------------|
| `dependency_health` | `am.dependency_health` | counter, histogram | `target`, `op`, `outcome`, `error_class` | no raw tenant/user IDs; provider-specific errors bucketed by `error_class` | Alerting: IdP failure rate, RG cleanup failures, GTS/AuthZ availability | Platform on-call runbook: `account-management/dependency-health` |
| `metadata_resolution` | `am.metadata_resolution` | counter, histogram | `operation`, `outcome`, `inheritance_policy` | `schema_id` omitted unless explicitly hashed by platform policy | Informational by default; alert only on sustained error-rate threshold | Platform on-call runbook: `account-management/metadata-resolution` |
| `bootstrap_lifecycle` | `am.bootstrap_lifecycle` | counter, histogram | `phase`, `classification`, `outcome` | no tenant ID label; root tenant is implicit | Alerting: bootstrap not-ready state and IdP-wait timeout | Platform on-call runbook: `account-management/bootstrap` |
| `tenant_retention` | `am.tenant_retention` | counter, gauge, histogram | `job`, `outcome`, `failure_class` | no raw tenant ID; backlog counts only | Alerting: provisioning reaper activity and background cleanup failures | Platform on-call runbook: `account-management/tenant-retention` |
| `conversion_lifecycle` | `am.conversion_lifecycle` | counter, histogram | `transition`, `initiator_side`, `outcome` | no request ID, tenant ID, or user ID labels | Informational by default; alert on stuck/expired backlog if platform policy enables | Platform on-call runbook: `account-management/conversions` |
| `hierarchy_depth_exceedance` | `am.hierarchy_depth_exceedance` | counter, gauge | `mode`, `threshold`, `outcome` | threshold values bucketed; no tenant/parent IDs | Alerting: integrity-check violations and repeated hard-limit rejects | Platform on-call runbook: `account-management/hierarchy-integrity` |
| `cross_tenant_denial` | `am.cross_tenant_denial` | counter | `operation`, `barrier_mode`, `reason` | no subject or target tenant/user IDs | Security alert candidate; routed through platform security/on-call policy | Platform on-call runbook: `account-management/cross-tenant-denials` |

**Implements**:

- `cpt-cf-account-management-algo-errors-observability-metric-emission`

**Touches**:

- Platform observability pipeline (meter provider, metric registry)

### Audit Contract and `actor=system` Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-audit-contract`

The module **MUST** emit platform audit records for every AM-owned state-changing operation with actor identity and tenant identity preserved, and **MUST** emit `actor=system` records for the AM-owned background transitions enumerated in `nfr-audit-completeness` (bootstrap completion, conversion expiry, provisioning-reaper compensation, hard-delete / tenant-deprovision cleanup). Audit storage, retention, and tamper resistance are inherited platform controls and are **not** owned by AM.

**Implements**:

- `cpt-cf-account-management-algo-errors-observability-audit-emission`

**Touches**:

- Platform audit sink (inherited control)
- Modules: bootstrap feature, conversion feature, retention jobs, hard-delete job

### SecurityContext Gate at Every Entry Point

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-security-context-gate`

Every AM entry point (REST handler, SDK boundary, inter-module ClientHub contract) **MUST** require or propagate a validated platform `SecurityContext` before dispatching into domain logic. Bootstrap and internally-owned background jobs are exempt and **MUST** attach `actor=system` explicitly. AM **MUST NOT** validate bearer tokens, mint session credentials, or perform AuthZ evaluation — those are platform concerns inherited per DESIGN §4.2.

**Implements**:

- `cpt-cf-account-management-algo-errors-observability-security-context-gate`

**Constraints**: `cpt-cf-account-management-constraint-security-context`, `cpt-cf-account-management-constraint-no-authz-eval`

**Touches**:

- REST handler layer, SDK boundary, ClientHub contract boundary

### Versioning Discipline

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-versioning-discipline`

Published REST APIs **MUST** use path-based versioning (`/v1/...`). SDK client and IdP integration contracts are stable interfaces — breaking changes **MUST** follow platform versioning policy and require a new contract version with a migration path. Source-of-truth tenant data consumed by Tenant Resolver, AuthZ Resolver, or Billing **MUST** remain backward-compatible within a minor release or publish a coordinated migration path.

**Implements**:

- Contract policy surface (no direct algorithm implementation — enforced by review gates, contract tests, and SemVer discipline across contract artifacts)

**Constraints**: `cpt-cf-account-management-constraint-versioning-policy`

**Touches**:

- `account-management-v1.yaml` OpenAPI spec, SDK contract crate, IdP integration trait

### Data Classification Baseline

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-errors-observability-data-classification`

AM persistence **MUST** classify tenant hierarchy and tenant-mode data as Internal / Confidential; IdP-issued opaque identity references in audit records as PII-adjacent and platform-protected; extensible metadata per the classification declared by each registered GTS schema. AM **MUST NOT** store authentication credentials or IdP profile PII outside platform audit infrastructure. Data residency, DSAR orchestration, retention-policy administration, and privacy-by-default controls are inherited platform obligations per DESIGN §4.1.

**Implements**:

- Data-handling policy surface (no direct algorithm — enforced by schema registration gates, audit review, and platform privacy orchestration)

**Constraints**: `cpt-cf-account-management-constraint-data-handling`

**Touches**:

- Tenant metadata storage, audit trail payload shape, IdP binding persistence

### Reliability Inheritance

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-reliability-inheritance`

AM **MUST** inherit the platform core infrastructure SLA (target 99.9% uptime). During IdP outages, AM **MUST** continue serving tenant reads, child listing, status reads, and metadata resolution from AM-owned data while failing only the IdP-dependent operations with `idp_unavailable`. Platform recovery targets RPO ≤ 1 hour and RTO ≤ 15 minutes are inherited. Tenant creation remains intentionally non-idempotent across ambiguous external failures per PRD §6.10.

**Implements**:

- Operational contract — enforced through SLO definitions, degradation routing in the error-surface flow, and IdP-outage behavioral tests

**Touches**:

- `cpt-cf-account-management-flow-errors-observability-error-surface` (IdP-unavailable path)
- Platform SLO dashboard and runbook

### Ops-Metrics Treatment

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-errors-observability-ops-metrics-treatment`

The module **MUST** define which of the 7 domain-specific metric families back SLO / alert rules and on-call escalation paths, and **MUST** provide the naming alignment contract with the platform metric catalog so downstream dashboards and alert-rule authoring can consume the families without renaming. The metric-catalog table in §5.2 is the authoritative source for the canonical metric-family names consumed by `algo-metric-emission`; sibling features' concrete emit instances (e.g., `bootstrap.attempts`, `bootstrap.outcome`) MUST reconcile against the name-alignment entries registered here. Specific alert rules, dashboard panels, and threshold values are deployment-specific and live outside this FEATURE; this DoD defines the integration surface, not the deployed alerts.

**Implements**:

- Integration contract — enforced through metric-catalog documentation, naming-alignment review, and on-call runbook linkage

**Touches**:

- Platform metric catalog, dashboard tooling (Grafana or equivalent), alert routing

### Vendor and Licensing Hygiene

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-errors-observability-vendor-licensing`

AM **MUST** depend only on platform-approved open-source libraries reached through ModKit (SeaORM, Axum, OpenTelemetry, and their transitive closures per the platform dependency policy). No proprietary or copyleft-licensed dependencies **MUST** be introduced at the module level. Vendor lock-in **MUST** remain scoped to the pluggable IdP provider contract, never to AM's own compile-time dependencies. An SBOM **MUST** be exported as part of the AM build and the license of every runtime dependency **MUST** appear on the platform allowlist.

**Implements**:

- Supply-chain policy surface — enforced through build-time SBOM generation, a license-allowlist lint against Cargo dependencies, and review gates on new runtime dependencies

**Constraints**: `cpt-cf-account-management-constraint-vendor-licensing`

**Touches**:

- AM `Cargo.toml` dependency closure, CI license-allowlist job, build-time SBOM artifact

## 6. Acceptance Criteria

- [ ] All 8 PRD §5.8 error categories are reachable from at least one test scenario across the AM test suite; every category returns the documented HTTP status and a Problem body with the expected `code` and a populated `sub_code`.
- [ ] Every stable public `sub_code` from DESIGN §3.8 appears as an exactly-matching string constant in the module's error enumeration and is covered by at least one test.
- [ ] Public Problem responses never contain domain-diagnostic internals beyond the stable `sub_code`; unclassified errors return `code=internal` with a generic body while the full diagnostic is recoverable through the audit trail.
- [ ] All 7 PRD §5.9 metric families are emitted by AM at runtime; each family's label set is documented and cardinality-guarded; dashboards and alert rules can subscribe to them by platform-aligned canonical names.
- [ ] `actor=system` audit records are emitted for bootstrap completion, conversion expiry, provisioning-reaper compensation, and hard-delete / tenant-deprovision cleanup; tenant-scoped audit records carry the caller's `SecurityContext` identity and tenant identity.
- [ ] Every REST handler, SDK boundary, and inter-module ClientHub contract rejects or refuses to dispatch invocations without a valid `SecurityContext` before invoking domain logic; bootstrap and background jobs attach `actor=system` explicitly and are the only caller-less exemptions.
- [ ] Breaking changes to the OpenAPI `Problem` schema, SDK contract, or IdP integration trait are blocked by contract-version review; path-based versioning is enforced on published REST endpoints. A SemVer-check CI job diffs `account-management-v1.yaml`, the SDK contract crate, and the IdP integration trait file between tagged versions and fails the build if any existing field is removed or retyped, or any required field is added, without a new contract version header.
- [ ] During a synthetic IdP outage, AM tenant reads, children listing, status reads, and metadata resolution continue to succeed while IdP-dependent operations fail cleanly with `code=idp_unavailable`.
- [ ] A classification-mapping artifact enumerates every AM-persisted data category (tenant hierarchy, tenant mode, conversion-request state, opaque identity references, per-schema metadata) with its classification tier (Internal / Confidential / PII-adjacent / per-GTS-schema). A schema-migration lint fails if any AM-owned table gains a column that holds IdP-issued credentials or IdP-sourced profile PII.
- [ ] The metric-catalog table in this FEATURE lists each of the 7 domain-specific metric families with (a) its canonical platform-aligned name, (b) metric kind, (c) allowed labels, (d) cardinality guard, (e) SLO / alert class or explicit `informational only` marker, and (f) the on-call runbook link it backs. At minimum, the PRD §6.12 operator-treatment topics (IdP failure rate, bootstrap not-ready state, provisioning reaper activity, integrity-check violations, background cleanup failures) each map to a family with a non-`informational only` classification.
- [ ] A CI license-allowlist job scans the AM `Cargo.toml` runtime-dependency closure and fails the build if any dependency license is not on the platform allowlist; an SBOM artifact is produced by the AM build and published with every release.

## 7. Deliberate Omissions

The following concerns are explicitly **not** addressed by this FEATURE. Each is recorded so reviewers can distinguish intentional exclusion (author considered and excluded with reasoning) from accidental omission.

- **UX / portal workflows** — *Not applicable.* AM exposes REST and SDK contracts only per DESIGN §4.1; the rendering of error envelopes and metric dashboards is a portal / operator-tooling concern outside the module.
- **Audit storage, retention, tamper resistance, security-monitoring integration** — *Inherited platform controls* (DESIGN §4.1). This FEATURE only defines the emission contract; the sink is platform-owned.
- **Dashboards and alert-rule authoring** — *Downstream / deployment-specific.* This FEATURE defines the metric catalog and the naming-alignment contract (`dod-ops-metrics-treatment`); which panels to show, which alert thresholds to set, and how to route paging is a deployment / SRE concern.
- **Token validation, session renewal, federation, MFA** — *Inherited from platform AuthN.* AM trusts the normalized `SecurityContext` and never validates bearer tokens itself (DESIGN §4.2).
- **Feature-specific error emission points** — *Owned by each feature.* This FEATURE defines the taxonomy and envelope; `tenant-hierarchy-management` emits `tenant_has_children`, `managed-self-managed-modes` emits `pending_exists`, `tenant-metadata` emits `metadata_schema_not_registered`, etc.
- **Concrete retention windows, privacy orchestration, DSAR flows** — *Inherited platform obligations* (DESIGN §4.1). AM contributes data minimization and audit hooks; DSAR/legal-hold/privacy policy administration is not in this FEATURE.
- **Domain data persistence** — *Not applicable.* This FEATURE owns no dbtable, no GTS schema, and no domain entity; per DECOMPOSITION §2.8 Feature 8 **MUST NOT** own a dbtable.
