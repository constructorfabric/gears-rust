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

One generic flow models how a domain failure surfaces from any AM feature's code path to the client through the envelope defined here. Feature-specific failure modes (e.g., `tenant_has_children`, `pending_exists`, `metadata_entry_not_found`) are emitted by their owning features; this flow shows the shared classification / envelope / emission path.

### Error Surface

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-errors-observability-error-surface`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Domain failure classified, mapped to an RFC 9457 Problem envelope, and returned to the client with the correct HTTP status and a stable non-null `code` field.
- Cross-tenant denial surfaces as `CanonicalError::PermissionDenied` (HTTP 403, `reason=CROSS_TENANT_DENIED`) without leaking the existence or attributes of the target resource beyond the canonical envelope.
- IdP contract failure surfaces as `CanonicalError::ServiceUnavailable` (HTTP 503) with deterministic retry semantics per PRD §6.10; the canonical category covers both IdP outages and other transient infrastructure outages, and `retry_after_seconds` is populated when a defensible hint is available.

**Error Scenarios**:

- Entry point reached without a valid `SecurityContext`: request is rejected by the SecurityContext gate before any domain logic runs, short-circuiting with a platform-standard auth error (not re-classified by this feature).
- Unexpected (unclassified) domain error: falls through to `CanonicalError::Internal` (HTTP 500) with a generic public body while the detailed diagnostic goes only to the audit trail.

**Steps**:

> At every REST handler, SDK boundary, and inter-gear ClientHub contract, the SecurityContext gate **MUST** run before any domain logic: a missing or invalid context short-circuits with the platform-standard auth error, so domain errors are never raised, classified, or mapped for unauthenticated callers.

1. [ ] - `p1` - Validate caller's `SecurityContext` via `algo-security-context-gate` at the entry point (REST handler, SDK boundary, or inter-gear ClientHub contract) before any domain logic executes - `inst-flow-errsurf-securitycontext-gate`
2. [ ] - `p1` - Feature code path raises a domain error (e.g., `TenantHasChildren`, `PendingExists`, `IdPUnavailable`) - `inst-flow-errsurf-raise`
3. [ ] - `p1` - Classify the domain error and map to Problem Details envelope via `algo-error-to-problem-mapping` - `inst-flow-errsurf-classify-and-map`
4. [ ] - `p1` - Emit domain metric via `algo-metric-emission` using the appropriate metric family for the failure mode (e.g., `dependency_health`, `hierarchy_depth_exceedance`, `cross_tenant_denial`) - `inst-flow-errsurf-metric-emit`
5. [ ] - `p1` - **IF** the failure is a state-changing or `actor=system`-eligible condition (per `nfr-audit-completeness`) - `inst-flow-errsurf-audit-branch`
   1. [ ] - `p1` - Emit audit event via `algo-audit-emission` with correct actor attribution (tenant identity or `actor=system`) - `inst-flow-errsurf-audit-emit`
6. [ ] - `p1` - **RETURN** Problem envelope with HTTP status from the category→status mapping and a stable non-null `code` field - `inst-flow-errsurf-return`

## 3. Processes / Business Logic (CDSL)

### Error-to-Problem Mapping

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping`

**Input**: `DomainError` instance (variant plus feature-specific diagnostic fields).

**Output**: An AIP-193 [`CanonicalError`](../../../../../libs/toolkit-canonical-errors/) which the SDK re-exports as `AccountManagementError` and which the platform converts into the RFC 9457 `Problem` envelope at the REST boundary. AM does not invent a private HTTP-status table; the status code is a property of the canonical category. Fine-grained discriminators (`INVALID_TENANT_TYPE`, `TENANT_HAS_CHILDREN`, `SERIALIZATION_CONFLICT`, …) ride inside the envelope as `reason` tokens on field/precondition/quota violations, not as a private AM-side `code` field.

**Steps**:

> This algorithm enumerates the canonical categories AM uses today. Any domain error not matched by steps 2–9 **MUST** fall through to `Internal` (HTTP 500) via step 10 to preserve public-contract stability; the unmatched diagnostic detail is preserved in the audit trail, not the public Problem body. The mapping is implemented by [`From<DomainError> for CanonicalError`](../../account-management/account-management/src/domain/error.rs).

1. [ ] - `p1` - Identify the `DomainError` variant - `inst-algo-etp-identify-kind`
2. [ ] - `p1` - **IF** variant is `Validation` / `InvalidTenantType` / `RootTenantCannotDelete` / `RootTenantCannotConvert` - `inst-algo-etp-invalid-argument`
   1. [ ] - `p1` - **RETURN** `CanonicalError::InvalidArgument` (HTTP 400) populated with the matching `reason` token (`VALIDATION` / `INVALID_TENANT_TYPE` / `ROOT_TENANT_CANNOT_DELETE` / `ROOT_TENANT_CANNOT_CONVERT`) on a field-violation entry - `inst-algo-etp-invalid-argument-return`
3. [ ] - `p1` - **IF** variant is `NotFound` / `MetadataEntryNotFound` - `inst-algo-etp-not-found`
   1. [ ] - `p1` - **RETURN** `CanonicalError::NotFound` (HTTP 404) with `resource_type` set to the matching GTS tag (`gts.cf.core.am.{tenant|tenant_metadata|conversion_request}.v1~`) and `resource_name` carrying the missing identifier - `inst-algo-etp-not-found-return`
4. [ ] - `p1` - **IF** variant is `TypeNotAllowed` / `TenantDepthExceeded` / `TenantHasChildren` / `TenantHasResources` / `PendingExists` / `InvalidActorForTransition` / `AlreadyResolved` / `Conflict` - `inst-algo-etp-failed-precondition`
   1. [ ] - `p1` - **RETURN** `CanonicalError::FailedPrecondition` (HTTP 400) with a precondition-violation entry whose `reason` token discriminates the specific cause (`TENANT_HAS_CHILDREN`, `TENANT_HAS_RESOURCES`, `TYPE_NOT_ALLOWED`, `PENDING_EXISTS`, `INVALID_ACTOR_FOR_TRANSITION`, `ALREADY_RESOLVED`, `PRECONDITION_FAILED`, …) - `inst-algo-etp-failed-precondition-return`
5. [ ] - `p1` - **IF** variant is `CrossTenantDenied` (barrier violation, unauthorized cross-tenant access, non-platform-admin attempting root-tenant-scoped operations) - `inst-algo-etp-permission-denied`
   1. [ ] - `p1` - **RETURN** `CanonicalError::PermissionDenied` (HTTP 403) with `reason=CROSS_TENANT_DENIED`; body **MUST NOT** leak target-resource attributes beyond the canonical envelope - `inst-algo-etp-permission-denied-return`
6. [ ] - `p1` - **IF** variant is `ServiceUnavailable` (covers IdP outages, AuthZ PDP transport failure, DB outages, generic transient infra outages) - `inst-algo-etp-service-unavailable`
   1. [ ] - `p1` - **RETURN** `CanonicalError::ServiceUnavailable` (HTTP 503), populating `retry_after_seconds` from `DomainError::ServiceUnavailable::retry_after` when the caller has a defensible hint - `inst-algo-etp-service-unavailable-return`
7. [ ] - `p1` - **IF** variant is `UnsupportedOperation` (IdP plugin does not support the requested administrative operation) - `inst-algo-etp-unimplemented`
   1. [ ] - `p1` - **RETURN** `CanonicalError::Unimplemented` (HTTP 501) - `inst-algo-etp-unimplemented-return`
8. [ ] - `p1` - **IF** variant is `IntegrityCheckInProgress` (single-flight contention; hierarchy-integrity check gate per DESIGN §3.2 — at most one concurrent integrity check, concurrent callers receive a retry-after refusal) - `inst-algo-etp-resource-exhausted`
   1. [ ] - `p1` - **RETURN** `CanonicalError::ResourceExhausted` (HTTP 429) with a quota-violation entry keyed by `integrity_check` - `inst-algo-etp-resource-exhausted-return`
9. [ ] - `p1` - **DB-error classification happens upstream of this mapping**, at retry exit in the infra canonical-mapping classifier (`classify_db_err_to_domain`). The raw database error is carried through `with_serializable_retry` by the infra-internal transaction error type (never inside `DomainError`); on retry exhaustion the surviving database error is translated into a typed `DomainError` variant (`Aborted`, `AlreadyExists`, `ServiceUnavailable`, or `Internal`) using the SQLSTATE / availability ladder below. The conversion from `DomainError` to `CanonicalError` then forwards each typed variant to its canonical category — no DB-aware code needs to live inside `domain/`. - `inst-algo-etp-db-classify`
   1. [ ] - `p1` - Serialization failure (Postgres SQLSTATE 40001 / SQLite `BUSY` / `BUSY_SNAPSHOT`) with retry budget exhausted → `DomainError::Aborted { reason: "SERIALIZATION_CONFLICT" }` → `CanonicalError::Aborted` (HTTP 409) - `inst-algo-etp-db-aborted`
   2. [ ] - `p1` - Unique-constraint violation (Postgres `23505` / SQLite `2067`) → `DomainError::AlreadyExists` → `CanonicalError::AlreadyExists` (HTTP 409) - `inst-algo-etp-db-already-exists`
   3. [ ] - `p1` - Typed availability signal (`ConnectionAcquire` timeout/closed, `Conn`, `DbError::Io`) → `DomainError::ServiceUnavailable` → `CanonicalError::ServiceUnavailable` (HTTP 503), no `retry_after_seconds` (the database itself is down) - `inst-algo-etp-db-unavailable`
   4. [ ] - `p1` - Anything else → `DomainError::Internal` → `CanonicalError::Internal` (HTTP 500); a redacted (no DSN / driver text) diagnostic is recorded in the audit-only `diagnostic` field, never in the public envelope - `inst-algo-etp-db-internal`
10. [ ] - `p1` - **ELSE** unclassified `DomainError` variant (fallthrough to preserve contract stability) - `inst-algo-etp-fallthrough`
    1. [ ] - `p1` - Record the full diagnostic detail in the audit trail (not the public Problem body) via `algo-audit-emission` - `inst-algo-etp-fallthrough-audit`
    2. [ ] - `p1` - **RETURN** `CanonicalError::Internal` (HTTP 500); public body **MUST NOT** disclose diagnostic internals - `inst-algo-etp-fallthrough-return`

### Metric Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-metric-emission`

**Input**: Metric family identifier (one of the 7 PRD §5.9 AM metric families plus the `serializable_retry` operational family this FEATURE adds — 8 families total in the catalog below), metric kind (counter, gauge, histogram), and labeled dimension set

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

**Input**: Audit event kind, actor attribution (`actor=<tenant-scoped-identity>` or `actor=system` for gear-owned background transitions), tenant identity, and structured payload

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
5. [ ] - `p1` - **IF** kind is a `CanonicalError::PermissionDenied` (cross-tenant denial), `CanonicalError::ServiceUnavailable` (IdP / DB outage), or `CanonicalError::Internal` (unclassified fallthrough from `algo-error-to-problem-mapping` step 10) failure surfaced through the error-surface flow — every such envelope MUST also write its diagnostic to the audit trail so operators retain a forensics record beyond the public Problem body, **subject to source-specific redaction**: DB-internal `Internal` diagnostics travel through `redacted_db_diagnostic` (no DSN / driver text), and IdP `Internal` diagnostics carry only the digest+length pair from `redact_provider_detail` (no vendor SDK strings, no token-bearing fragments). The audit record therefore contains the most-detailed safe diagnostic available at the source, not necessarily the raw producer text - `inst-algo-audit-failure`
   1. [ ] - `p1` - Construct the audit record carrying the source-redacted diagnostic detail (suppressed from the public Problem body but governed by the same redaction contracts that protect operator logs) - `inst-algo-audit-construct-failure`
6. [ ] - `p1` - Emit through the platform audit sink; AM does **not** own storage, retention, tamper resistance, or security-monitoring integration (those are inherited platform controls per DESIGN §4.1) - `inst-algo-audit-emit`
7. [ ] - `p1` - **RETURN** — audit emission is fire-and-forget from the caller's perspective; delivery durability is a platform SLA - `inst-algo-audit-return`

### SecurityContext Gate

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-errors-observability-security-context-gate`

**Input**: Inbound request or inter-gear invocation at an AM entry point (REST handler, SDK boundary, or ClientHub contract)

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

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-error-taxonomy-and-envelope`

**PR1 scope**: `DomainError` enum + `From<DomainError> for CanonicalError` (AIP-193 boundary mapping) ship in `domain/error.rs`. The RFC 9457 `Problem` envelope rendering at the REST handler boundary uses [`toolkit_canonical_errors::Problem`](../../../../../libs/toolkit-canonical-errors/) directly and arrives with the REST surface in a later PR.

The gear **MUST** map every domain failure to one of the AIP-193 canonical categories enumerated in PRD §5.8 / DESIGN §3.8 — `InvalidArgument`, `NotFound`, `FailedPrecondition`, `Aborted`, `AlreadyExists`, `PermissionDenied`, `ResourceExhausted`, `ServiceUnavailable`, `Unimplemented`, `Internal` — and **MUST NOT** mint AM-private categories or override the AIP-193 HTTP-status table. Fine-grained discriminators ride inside the canonical envelope as `reason` tokens on field/precondition/quota violations or in `resource_type` / `resource_name`. Unclassified domain errors **MUST** fall through to `CanonicalError::Internal` rather than leaking new public categories.

The authoritative AIP-193 mapping is documented in [DESIGN.md §3.8 Error Codes Reference](../DESIGN.md#38-error-codes-reference); the table below records only the discriminators added or refined by this feature. Discriminators contributed by sibling features (tenant-hierarchy-management, mode-conversion, tenant-metadata, etc.) are documented in their own feature files.

| `reason` token | Canonical category (HTTP) | Domain source |
|----------------|---------------------------|---------------|
| `SERIALIZATION_CONFLICT` | `Aborted` (409) | `DomainError::Aborted { reason: "SERIALIZATION_CONFLICT" }` produced by `infra::canonical_mapping::classify_db_err_to_domain` after the SERIALIZABLE retry budget is exhausted on a `DbErr` carrying SQLSTATE 40001 (or the SQLite analogue) |

**Implements**:

- `cpt-cf-account-management-flow-errors-observability-error-surface`
- `cpt-cf-account-management-algo-errors-observability-error-to-problem-mapping`

**Touches**:

- Contract: RFC 9457 Problem schema in `account-management-v1.yaml`
- Gears: every feature's error boundary; this DoD is consumed transitively

### Observability Metric Catalog

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-metric-catalog`

**PR1 scope**: metric-name constants (`AM_DEPENDENCY_HEALTH`, …, 11 total) live in `domain/metrics.rs` with no-op `emit_metric` shells; the cardinality guards and OTel meter-provider plumbing land alongside the observability port wiring in a later PR.

The gear **MUST** export the 7 domain-specific metric families required by PRD §5.9 (dependency health, metadata resolution, bootstrap lifecycle, tenant-retention, conversion lifecycle, hierarchy-depth threshold exceedance, cross-tenant denials) plus the `serializable_retry` operational family added by this FEATURE (8 families total in the catalog table below). Metric names **MUST** align with platform observability naming conventions; label sets **MUST** be documented and cardinality-guarded so no metric exposes unbounded per-tenant or per-user dimensions without an explicit hashing policy.

| Family ID | Canonical family name | Kind(s) | Allowed labels | Cardinality guard | SLO / alert class | Runbook linkage |
|-----------|-----------------------|---------|----------------|-------------------|-------------------|-----------------|
| `dependency_health` | `am.dependency_health` | counter, histogram | `target`, `op`, `outcome`, `error_class` | no raw tenant/user IDs; provider-specific errors bucketed by `error_class` | Alerting: IdP failure rate, RG cleanup failures, GTS/AuthZ availability | Platform on-call runbook: `account-management/dependency-health` |
| `metadata_resolution` | `am.metadata_resolution` | counter, histogram | `operation`, `outcome`, `inheritance_policy` | `schema_id` omitted unless explicitly hashed by platform policy | Informational by default; alert only on sustained error-rate threshold | Platform on-call runbook: `account-management/metadata-resolution` |
| `bootstrap_lifecycle` | `am.bootstrap_lifecycle` | counter, histogram | `phase`, `classification`, `outcome` | no tenant ID label; root tenant is implicit | Alerting: bootstrap not-ready state and IdP-wait timeout | Platform on-call runbook: `account-management/bootstrap` |
| `tenant_retention` | `am.tenant_retention` | counter, gauge, histogram | `job`, `outcome`, `failure_class` | no raw tenant ID; backlog counts only | Alerting: provisioning reaper activity and background cleanup failures | Platform on-call runbook: `account-management/tenant-retention` |
| `conversion_lifecycle` | `am.conversion_lifecycle` | counter, histogram | `transition`, `initiator_side`, `outcome` | no request ID, tenant ID, or user ID labels | Informational by default; alert on stuck/expired backlog if platform policy enables | Platform on-call runbook: `account-management/conversions` |
| `hierarchy_depth_exceedance` | `am.hierarchy_depth_exceedance` | counter, gauge | `mode`, `threshold`, `outcome` | threshold values bucketed; no tenant/parent IDs | Alerting: integrity-check violations and repeated hard-limit rejects | Platform on-call runbook: `account-management/hierarchy-integrity` |
| `cross_tenant_denial` | `am.cross_tenant_denial` | counter | `operation`, `barrier_mode`, `reason` | no subject or target tenant/user IDs | Security alert candidate; routed through platform security/on-call policy | Platform on-call runbook: `account-management/cross-tenant-denials` |
| `serializable_retry` | `am.serializable_retry` | counter | `outcome` (`recovered`/`exhausted`), `attempts` | `attempts` bounded by `MAX_SERIALIZABLE_ATTEMPTS`; no tenant/user labels | Alerting: sustained `outcome=exhausted` rate (DB contention); informational `recovered` rate for retry-budget tuning | Platform on-call runbook: `account-management/serializable-retry` |

**Implements**:

- `cpt-cf-account-management-algo-errors-observability-metric-emission`

**Touches**:

- Platform observability pipeline (meter provider, metric registry)

### Audit Contract and `actor=system` Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-audit-contract`

**PR1 scope**: `AuditActor` / `AuditEvent` / `AuditEventKind` shapes ship in the impl crate at `cf-gears-account-management::domain::audit` with `serde` `camelCase` wire format. The downstream public surface for these shapes is the `cf-gears-account-management-sdk` SDK crate (`account_management_sdk::audit`, exported once consumers stabilize); gear-internal callers may import from the impl-crate path until the SDK re-exports land. The `AuditEmitter` runtime, sinks, and the per-call-site `emit_audit` invocations land with the audit-classifier set in a later PR.

The gear **MUST** emit platform audit records for every AM-owned state-changing operation with actor identity and tenant identity preserved, and **MUST** emit `actor=system` records for the AM-owned background transitions enumerated in `nfr-audit-completeness` (bootstrap completion, conversion expiry, provisioning-reaper compensation, hard-delete / tenant-deprovision cleanup). Audit storage, retention, and tamper resistance are inherited platform controls and are **not** owned by AM.

**Implements**:

- `cpt-cf-account-management-algo-errors-observability-audit-emission`

**Touches**:

- Platform audit sink (inherited control)
- Gears: bootstrap feature, conversion feature, retention jobs, hard-delete job

### SecurityContext Gate at Every Entry Point

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-errors-observability-security-context-gate`

Every AM entry point (REST handler, SDK boundary, inter-gear ClientHub contract) **MUST** require or propagate a validated platform `SecurityContext` before dispatching into domain logic. Bootstrap and internally-owned background jobs are exempt and **MUST** attach `actor=system` explicitly. AM **MUST NOT** validate bearer tokens, mint session credentials, or perform AuthZ evaluation — those are platform concerns inherited per DESIGN §4.2.

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

AM **MUST** inherit the platform core infrastructure SLA (target 99.9% uptime). During IdP outages, AM **MUST** continue serving tenant reads, child listing, status reads, and metadata resolution from AM-owned data while failing only the IdP-dependent operations with `CanonicalError::ServiceUnavailable` (HTTP 503). Platform recovery targets RPO ≤ 1 hour and RTO ≤ 15 minutes are inherited. Tenant creation remains intentionally non-idempotent across ambiguous external failures per PRD §6.10.

**Implements**:

- Operational contract — enforced through SLO definitions, degradation routing in the error-surface flow, and IdP-outage behavioral tests

**Touches**:

- `cpt-cf-account-management-flow-errors-observability-error-surface` (IdP-unavailable path)
- Platform SLO dashboard and runbook

### Ops-Metrics Treatment

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-errors-observability-ops-metrics-treatment`

The gear **MUST** define which of the 8 documented metric families (the 7 PRD §5.9 domain-specific families plus `serializable_retry`) back SLO / alert rules and on-call escalation paths, and **MUST** provide the naming alignment contract with the platform metric catalog so downstream dashboards and alert-rule authoring can consume the families without renaming. The metric-catalog table in §5.2 is the authoritative source for the canonical metric-family names consumed by `algo-metric-emission`; sibling features' concrete emit instances (e.g., `bootstrap.attempts`, `bootstrap.outcome`) MUST reconcile against the name-alignment entries registered here. Specific alert rules, dashboard panels, and threshold values are deployment-specific and live outside this FEATURE; this DoD defines the integration surface, not the deployed alerts.

**Implements**:

- Integration contract — enforced through metric-catalog documentation, naming-alignment review, and on-call runbook linkage

**Touches**:

- Platform metric catalog, dashboard tooling (Grafana or equivalent), alert routing

### Vendor and Licensing Hygiene

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-errors-observability-vendor-licensing`

AM **MUST** depend only on platform-approved open-source libraries reached through ToolKit (SeaORM, Axum, OpenTelemetry, and their transitive closures per the platform dependency policy). No proprietary or copyleft-licensed dependencies **MUST** be introduced at the gear level. Vendor lock-in **MUST** remain scoped to the pluggable IdP provider contract, never to AM's own compile-time dependencies. An SBOM **MUST** be exported as part of the AM build and the license of every runtime dependency **MUST** appear on the platform allowlist.

**Implements**:

- Supply-chain policy surface — enforced through build-time SBOM generation, a license-allowlist lint against Cargo dependencies, and review gates on new runtime dependencies

**Constraints**: `cpt-cf-account-management-constraint-vendor-licensing`

**Touches**:

- AM `Cargo.toml` dependency closure, CI license-allowlist job, build-time SBOM artifact

## 6. Acceptance Criteria

- [ ] Every AIP-193 canonical category AM uses (`InvalidArgument`, `NotFound`, `FailedPrecondition`, `Aborted`, `AlreadyExists`, `PermissionDenied`, `ResourceExhausted`, `ServiceUnavailable`, `Unimplemented`, `Internal`) is reachable from at least one test scenario across the AM test suite; every category returns the documented HTTP status (per DESIGN §3.8) and a `Problem` body with the canonical `type` / `title` / `errors[]` shape.
- [ ] At least one test scenario exercises the `ResourceExhausted` mapping end-to-end: it triggers the single-flight gate, receives an HTTP 429 response, and asserts the canonical envelope carries `quota_violations[0].subject` matching `integrity_check`. The runtime/SDK chain `DomainError::IntegrityCheckInProgress → CanonicalError::ResourceExhausted → Public Problem(429)` is locked by this test.
- [ ] Every documented `reason` token (`SERIALIZATION_CONFLICT`, `TENANT_HAS_CHILDREN`, `TENANT_HAS_RESOURCES`, `TYPE_NOT_ALLOWED`, `TENANT_DEPTH_EXCEEDED`, `PENDING_EXISTS`, `INVALID_ACTOR_FOR_TRANSITION`, `ALREADY_RESOLVED`, `INVALID_TENANT_TYPE`, `ROOT_TENANT_CANNOT_DELETE`, `ROOT_TENANT_CANNOT_CONVERT`, `CROSS_TENANT_DENIED`) appears as an exactly-matching string constant in the boundary-mapping code and is covered by at least one test.
- [ ] Public `Problem` responses never contain domain-diagnostic internals beyond the canonical envelope; unclassified errors return `CanonicalError::Internal` (HTTP 500) with a generic body while the full diagnostic is recoverable through the audit trail.
- [ ] All 8 documented metric families (7 PRD §5.9 + `serializable_retry`) are emitted by AM at runtime; each family's label set is documented and cardinality-guarded; dashboards and alert rules can subscribe to them by platform-aligned canonical names.
- [ ] `actor=system` audit records are emitted for bootstrap completion, conversion expiry, provisioning-reaper compensation, and hard-delete / tenant-deprovision cleanup; tenant-scoped audit records carry the caller's `SecurityContext` identity and tenant identity.
- [ ] Every REST handler, SDK boundary, and inter-gear ClientHub contract rejects or refuses to dispatch invocations without a valid `SecurityContext` before invoking domain logic; bootstrap and background jobs attach `actor=system` explicitly and are the only caller-less exemptions.
- [ ] Breaking changes to the OpenAPI `Problem` schema, SDK contract, or IdP integration trait are blocked by contract-version review; path-based versioning is enforced on published REST endpoints. A SemVer-check CI job diffs `account-management-v1.yaml`, the SDK contract crate, and the IdP integration trait file between tagged versions and fails the build if any existing field is removed or retyped, or any required field is added, without a new contract version header.
- [ ] During a synthetic IdP outage, AM tenant reads, children listing, status reads, and metadata resolution continue to succeed while IdP-dependent operations fail cleanly with `CanonicalError::ServiceUnavailable` (HTTP 503).
- [ ] A classification-mapping artifact enumerates every AM-persisted data category (tenant hierarchy, tenant mode, conversion-request state, opaque identity references, per-schema metadata) with its classification tier (Internal / Confidential / PII-adjacent / per-GTS-schema). A schema-migration lint fails if any AM-owned table gains a column that holds IdP-issued credentials or IdP-sourced profile PII.
- [ ] The metric-catalog table in this FEATURE lists each of the 8 documented metric families (7 PRD §5.9 domain-specific families + `serializable_retry`) with (a) its canonical platform-aligned name, (b) metric kind, (c) allowed labels, (d) cardinality guard, (e) SLO / alert class or explicit `informational only` marker, and (f) the on-call runbook link it backs. At minimum, the PRD §6.12 operator-treatment topics (IdP failure rate, bootstrap not-ready state, provisioning reaper activity, integrity-check violations, background cleanup failures) each map to a family with a non-`informational only` classification.
- [ ] A CI license-allowlist job scans the AM `Cargo.toml` runtime-dependency closure and fails the build if any dependency license is not on the platform allowlist; an SBOM artifact is produced by the AM build and published with every release.

## 7. Deliberate Omissions

The following concerns are explicitly **not** addressed by this FEATURE. Each is recorded so reviewers can distinguish intentional exclusion (author considered and excluded with reasoning) from accidental omission.

- **UX / portal workflows** — *Not applicable.* AM exposes REST and SDK contracts only per DESIGN §4.1; the rendering of error envelopes and metric dashboards is a portal / operator-tooling concern outside the gear.
- **Audit storage, retention, tamper resistance, security-monitoring integration** — *Inherited platform controls* (DESIGN §4.1). This FEATURE only defines the emission contract; the sink is platform-owned.
- **Dashboards and alert-rule authoring** — *Downstream / deployment-specific.* This FEATURE defines the metric catalog and the naming-alignment contract (`dod-ops-metrics-treatment`); which panels to show, which alert thresholds to set, and how to route paging is a deployment / SRE concern.
- **Token validation, session renewal, federation, MFA** — *Inherited from platform AuthN.* AM trusts the normalized `SecurityContext` and never validates bearer tokens itself (DESIGN §4.2).
- **Feature-specific error emission points** — *Owned by each feature.* This FEATURE defines the taxonomy and envelope; `tenant-hierarchy-management` emits `tenant_has_children`, `managed-self-managed-modes` emits `pending_exists`, `tenant-metadata` emits the unified metadata `not_found`, etc.
- **Concrete retention windows, privacy orchestration, DSAR flows** — *Inherited platform obligations* (DESIGN §4.1). AM contributes data minimization and audit hooks; DSAR/legal-hold/privacy policy administration is not in this FEATURE.
- **Domain data persistence** — *Not applicable.* This FEATURE owns no dbtable, no GTS schema, and no domain entity; per DECOMPOSITION §2.8 Feature 8 **MUST NOT** own a dbtable.
