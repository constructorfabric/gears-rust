<!--
cpt:
  version: 0.3.2
  updated: 2026-06-04
-->

# Feature: Gear Foundation & Pluggable Storage

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Plugin Host Binding (Lazy Resolve)](#plugin-host-binding-lazy-resolve)
  - [PDP Authorize and Constraint Return](#pdp-authorize-and-constraint-return)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Plugin Host Binding (Lazy Resolution)](#plugin-host-binding-lazy-resolution)
  - [PDP Authorize](#pdp-authorize)
- [4. Definitions of Done](#4-definitions-of-done)
  - [FR: Pluggable Storage](#fr-pluggable-storage)
  - [FR: Data Classification](#fr-data-classification)
  - [NFR: Availability](#nfr-availability)
  - [NFR: Plugin Contract Stability](#nfr-plugin-contract-stability)
  - [NFR: Operational Visibility](#nfr-operational-visibility)
  - [Principle: Fail Closed](#principle-fail-closed)
  - [Principle: Pluggable Storage](#principle-pluggable-storage)
  - [Principle: Contract Stability](#principle-contract-stability)
  - [Principle: PDP-Centric Authorization](#principle-pdp-centric-authorization)
  - [Principle: Plugin Resolution via ClientHub](#principle-plugin-resolution-via-clienthub)
  - [Principle: OTLP Push Emission](#principle-otlp-push-emission)
  - [Principle: Gateway HTTP Server Instrument Reuse](#principle-gateway-http-server-instrument-reuse)
  - [Constraint: Plugin Contract Stability](#constraint-plugin-contract-stability)
  - [Constraint: Vendor Pluggable](#constraint-vendor-pluggable)
  - [Constraint: NFR Thresholds](#constraint-nfr-thresholds)
  - [ADR: Contract Stability](#adr-contract-stability)
  - [ADR: PDP-Centric Authorization](#adr-pdp-centric-authorization)
  - [ADR: Pluggable Storage](#adr-pluggable-storage)
  - [Contract: Storage Plugin](#contract-storage-plugin)
  - [Contract: AuthZ Resolver](#contract-authz-resolver)
  - [Contract: GTS Registry](#contract-gts-registry)
  - [Entity: PluginBinding](#entity-pluginbinding)
  - [Entity: SecurityContext](#entity-securitycontext)
  - [Entity: PdpDecision](#entity-pdpdecision)
  - [Entity: PdpConstraint](#entity-pdpconstraint)
  - [Component: Plugin Host](#component-plugin-host)
  - [§2.1-item → DoD-ID Coverage Matrix](#21-item--dod-id-coverage-matrix)
- [5. Acceptance Criteria](#5-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-foundation`

## 1. Feature Context

- [ ] `p1` - `cpt-cf-usage-collector-feature-foundation`

### 1.1 Overview

Establishes the Usage Collector's stateless gear runtime substrate and its three public contract surfaces — the in-process SDK trait, the REST API, and the storage Plugin SPI — so that every later capability (UsageType Catalog, Usage Emission, Usage Query, Event Deactivation, Compensation) plugs into a single, identical execution shape. The foundation owns plugin host binding, `SecurityContext` acceptance plus PDP dispatch through the shared `domain/authz.rs` helpers, audit-correlation propagation, and deployment topology.

Operational metrics reach the platform exclusively through OTLP push via ToolKit's `SdkMeterProvider`. Platform liveness/readiness probes are handled by the ToolKit host above the gear boundary; no gear-local health endpoints are exposed.

Authentication is owned by the ToolKit gateway upstream of the collector. Every request arrives carrying a resolved `SecurityContext`, and PDP enforcement is dispatched by each domain method through the shared helpers in `domain/authz.rs` (`authorize` for catalog ops; `authorize_usage_record_submit` for the ingestion surface). Both helpers call `cpt-cf-usage-collector-contract-authz-resolver` with `require_constraints(false)` per ADR-0012 / PRD §5.8 (catalog is platform-global; ingestion uses subject-only PDP for v1) — an unconstrained permit (`allow_all`) is a legitimate happy-path outcome.

### 1.2 Purpose

This feature exists so safety-critical behavior — fail-closed authentication, PDP-mediated authorization, and audit-correlation propagation — is realized once at the substrate layer rather than re-implemented per feature, and so storage vendors can ship and migrate backends independently of the core release train through a contract-stable Plugin SPI bound through the GTS Registry and ClientHub, which is the single seam through which both `usage_records` and the plugin-owned `usage_type_catalog` table are persisted.

**Requirements**: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-data-classification`, `cpt-cf-usage-collector-nfr-availability`, `cpt-cf-usage-collector-nfr-plugin-contract-stability`, `cpt-cf-usage-collector-nfr-operational-visibility`

**Principles**: `cpt-cf-usage-collector-principle-fail-closed`, `cpt-cf-usage-collector-principle-pluggable-storage`, `cpt-cf-usage-collector-principle-plugin-resolution-via-client-hub`, `cpt-cf-usage-collector-principle-contract-stability`, `cpt-cf-usage-collector-principle-pdp-centric-authorization`, `cpt-cf-usage-collector-principle-otlp-push-emission`, `cpt-cf-usage-collector-principle-gateway-http-server-instrument-reuse`

**Platform dependencies (foundation-level)**: `toolkit` (gear wiring, `#[toolkit::gear]`, `ClientHub`, and the global `SdkMeterProvider` constructed via `opentelemetry::global::meter_with_scope("usage_collector", …)` at gear bootstrap), `toolkit-gts` (`PluginV1<P>` GTS base type and the `gts_type_schema` derive consumed by `usage-collector-sdk/src/gts.rs` to declare `UsageCollectorPluginSpecV1`), `types-registry-sdk` (`TypesRegistryClient::list_instances` consumed by `GtsPluginSelector` lazily on the first dispatch call after the `types-registry` is consistent — there is no runtime config-change channel that would re-trigger this query), `toolkit-security` (`SecurityContext` propagation), and `toolkit-canonical-errors` (canonical `Problem` envelope on the REST surface; taken by the host crate `usage-collector` only — the SDK crate `usage-collector-sdk` does NOT depend on it, and the host's `From<UsageCollectorError> for CanonicalError` lift in `usage-collector/src/infra/sdk_error_mapping.rs` produces the canonical Problem envelope from the flat SDK error per DESIGN §3.3 Error Envelopes).

### 1.3 Actors

| Actor                                             | Role in Feature                                                                                                                                                                                                           |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-actor-platform-operator`  | Selects and configures the active storage plugin via `[usage_collector].vendor` (read once at `Gear::init`); observes operational endpoints; changes to the binding require a gear restart                            |
| `cpt-cf-usage-collector-actor-platform-developer` | Consumes the in-process SDK trait through ClientHub; implements the Plugin SPI when delivering a storage backend                                                                                                          |
| `cpt-cf-usage-collector-actor-storage-backend`    | Implements the Plugin SPI surface bound by the Plugin Host; receives dispatched persistence/query/deactivate calls per the `cpt-cf-usage-collector-contract-storage-plugin`                                               |
| `cpt-cf-usage-collector-actor-usage-source`       | Arrives at the foundation carrying a gateway-resolved `SecurityContext`; the shared `domain/authz.rs` helper authorizes emission (substrate-only role here; emission semantics owned by §2.3)                             |
| `cpt-cf-usage-collector-actor-usage-consumer`     | Arrives at the foundation carrying a gateway-resolved `SecurityContext`; the shared `domain/authz.rs` helper authorizes reads (substrate-only role here; query semantics owned by §2.4)                                   |
| `cpt-cf-usage-collector-actor-tenant-admin`       | Arrives at the foundation carrying a gateway-resolved `SecurityContext` scoped to their own tenant; PDP authorization is dispatched uniformly by every domain method through the shared `domain/authz.rs` helpers |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) -- Actors §2, Pluggable Storage §5.4, Security & Data Governance §5.8, NFR catalog §6
- **Design**: [DESIGN.md](../DESIGN.md) -- Plugin Host (§3.2), UsageType Catalog (§3.2), Contract Surfaces (§3.3), Deployment Topology (§3.8), PRD→DESIGN Realization (§5.3)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) -- §2.1 Gear Foundation & Pluggable Storage; §4.3 Plugin discovery and dispatch
- **ADR**: [ADR-0001](../ADR/0001-pdp-centric-authorization.md) -- PDP-Centric Authorization; [ADR-0002](../ADR/0002-pluggable-storage.md) -- Pluggable Storage; [ADR-0006](../ADR/0006-contract-stability.md) -- Contract Stability; [ADR-0012](../ADR/0012-unified-plugin-catalog-and-gts-id-reference.md) -- Unified plugin-DB usage-type catalog and `gts_id` reference model (supersedes ADR-0007 / ADR-0009 / ADR-0010)
- **Plugin SPI reference**: [plugin-spi.md](../plugin-spi.md)
- **SDK trait reference**: [sdk-trait.md](../sdk-trait.md)
- **REST contract**: [usage-collector-v1.yaml](../usage-collector-v1.yaml)
- **Dependencies**: None

## 2. Actor Flows (CDSL)

### Plugin Host Binding (Lazy Resolve)

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`

**Actor**: `cpt-cf-usage-collector-actor-usage-source` | `cpt-cf-usage-collector-actor-usage-consumer` | `cpt-cf-usage-collector-actor-tenant-admin` | `cpt-cf-usage-collector-actor-platform-operator` (the first downstream caller whose dispatch causes the lazy resolve; the operator only contributes by selecting `[usage_collector].vendor` at gear bootstrap, not by triggering this flow directly)

**Success Scenarios**:

- **Cold path (first dispatch)**: at gear bootstrap the host `cpt-cf-usage-collector-component-plugin-host` reads the vendor selection from `[usage_collector].vendor` once, constructs the gateway-side service with an embedded GTS plugin selector (no instance resolution happens yet), and registers the consumer-facing `cpt-cf-usage-collector-interface-sdk-client` in ClientHub. Independently, each `cpt-cf-usage-collector-actor-storage-backend` plugin gear publishes a `PluginV1<UsageCollectorPluginSpecV1>` instance through the types-registry client and registers its scoped `cpt-cf-usage-collector-contract-storage-plugin` trait object in ClientHub under the corresponding GTS instance scope. On the first dispatch after the types-registry is consistent, the selector queries the registry by `UsageCollectorPluginSpecV1::gts_schema_id()`, applies the configured vendor + priority selection (lowest priority wins), and caches the resolved `PluginBinding` for the service's lifetime.
- **Warm path (cached selector hit)**: subsequent dispatches reuse the cached `GtsInstanceId` and obtain the scoped `cpt-cf-usage-collector-contract-storage-plugin` handle via `ClientHub::try_get_scoped` with no further types-registry round-trip.

**Error Scenarios**:

- The types-registry is unreachable on the first dispatch — the per-call selector initialization surfaces a `PluginUnavailable` outcome (per the published Plugin SPI error taxonomy) to that caller; the selector remains uncached and the next dispatch retries the lazy resolve. The host's `usage_collector.plugin.ready` gauge reports `0` until the structural readiness fact holds.
- No plugin instance is registered under the resolved GTS instance scope (for example a plugin gear's bootstrap failed before its scoped registration step, or the dispatch arrived before that step ran) — `ClientHub::try_get_scoped` returns `None`, which the host lifts to a per-call `PluginUnavailable` outcome on the published Plugin SPI error taxonomy. The `usage_collector.plugin.ready` gauge reflects the same structural fact.
- Binding selection is monotonic for the service's lifetime: once the selector has cached an instance id, that selection is reused until the gear restarts. There is no runtime configuration-change channel that would re-trigger resolution (re-binding requires a gear restart).

**Steps**:

1. [ ] - `p1` - At gear bootstrap, read the vendor selection from `[usage_collector].vendor` once and construct the gateway-side service with an embedded GTS plugin selector; no types-registry query is performed here - `inst-binding-config-read-once`
2. [ ] - `p1` - Each storage-backend plugin gear publishes its `PluginV1<UsageCollectorPluginSpecV1>` instance through the types-registry client, then registers its scoped `cpt-cf-usage-collector-contract-storage-plugin` trait object in ClientHub under the corresponding GTS instance scope - `inst-binding-clienthub-register`
3. [ ] - `p1` - **IF** the selector cache is empty (cold path) - `inst-binding-cold-path`
   1. [ ] - `p1` - Query the types-registry by `UsageCollectorPluginSpecV1::gts_schema_id()` and apply the configured vendor + priority selection (lowest priority wins) - `inst-binding-lazy-resolve`
   2. [ ] - `p1` - Cache the resolved `PluginBinding` exactly once for the service's lifetime - `inst-binding-cache-instance-id`
4. [ ] - `p1` - **ELSE** (warm path) reuse the cached `GtsInstanceId` without a types-registry round-trip - `inst-binding-warm-path`
5. [ ] - `p1` - Resolve the scoped `cpt-cf-usage-collector-contract-storage-plugin` handle via `ClientHub::try_get_scoped` on the GTS instance scope; **IF** the lookup returns no handle **RETURN** a `PluginUnavailable` outcome on the per-call path per the published Plugin SPI error taxonomy - `inst-binding-try-get-scoped`
6. [ ] - `p1` - Compute the structural readiness fact for the `cpt-cf-usage-collector-contract-storage-plugin` and reflect it on the `usage_collector.plugin.ready` gauge (`1` when both facts hold — selector has cached an instance id AND `ClientHub::try_get_scoped` returns a handle — `0` otherwise) - `inst-binding-readiness-fact`
7. [ ] - `p1` - **RETURN** the resolved scoped `cpt-cf-usage-collector-contract-storage-plugin` handle to the calling pipeline so the dispatch can complete; warm-path calls reuse the cached id and the cached scoped handle with no further types-registry round-trip - `inst-binding-return-handle`

### PDP Authorize and Constraint Return

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-foundation-pdp-authorize`

**Actor**: `cpt-cf-usage-collector-actor-usage-source` | `cpt-cf-usage-collector-actor-usage-consumer` | `cpt-cf-usage-collector-actor-tenant-admin` | `cpt-cf-usage-collector-actor-platform-operator` (any caller whose domain-component invocation triggers the shared PDP dispatch helpers in `domain/authz.rs`; PDP authorize is a reusable algorithmic seam invoked inside every domain-component method, not an actor-initiated flow on its own)

**Success Scenarios**:

- With the inbound `SecurityContext` (resolved upstream by the ToolKit gateway on REST or supplied by the caller on the in-process SDK) and the operation's attribution, the domain component handling the call invokes the shared PDP authorization helper in `domain/authz.rs` (`authorize` for catalog ops; `authorize_usage_record_submit` for the ingestion surface), which calls `cpt-cf-usage-collector-contract-authz-resolver` with the attributes that apply to the operation's resource type and `require_constraints(false)` (per ADR-0012 / PRD §5.8 the catalog is platform-global and the ingestion surface uses subject-only PDP for v1 — an unconstrained permit, `allow_all`, is a legitimate happy-path outcome). The helper returns `Ok(())` to the caller on permit and never caches the decision; downstream constraint-intersection lands in Usage Query when its first caller arrives.

**Error Scenarios**:

- The `cpt-cf-usage-collector-contract-authz-resolver` is unreachable or times out — fail closed with a deterministic platform-authorization error (`AuthorizationUnavailable`), never serve a cached or permissive decision.
- The resolver returns deny — the operation is rejected immediately with an actionable error envelope (`AuthorizationDenied`) and no plugin dispatch is performed.
- The resolver returns `CompileFailed` for the (resource, action) tuple — the helper collapses this to the same `AuthorizationDenied` envelope (fail closed; the collector calls the enforcer with `require_constraints(false)` so a non-deny compile-failure is unexpected and must not derive a permissive fallback).

**Steps**:

1. [ ] - `p1` - Receive (`SecurityContext`, operation descriptor, attribution) from the surface boundary; the `SecurityContext` is already resolved upstream of the collector by the ToolKit gateway (REST) or supplied by the in-process SDK caller - `inst-pdp-input`
2. [ ] - `p1` - Compose the attribution required by `cpt-cf-usage-collector-contract-authz-resolver` for the operation's resource type (catalog ops carry no resource attributes; usage-record submission adds `OWNER_TENANT_ID`, `OWNER_ID`, `resource_type`, `resource_id`) and call the shared `domain/authz.rs` helper with `require_constraints(false)` - `inst-pdp-compose-tuple`
3. [ ] - `p1` - **TRY** invoke `cpt-cf-usage-collector-contract-authz-resolver` via the shared PDP helper (`enforcer.access_scope_with(...)`) - `inst-pdp-resolver-call`
4. [ ] - `p1` - **CATCH** transport-or-evaluation failure (`EnforcerError::EvaluationFailed`) - `inst-pdp-resolver-catch`
   1. [ ] - `p1` - **RETURN** `AuthorizationUnavailable` (no cached decision, no permissive fallback) - `inst-pdp-fail-closed`
5. [ ] - `p1` - **IF** the returned `PdpDecision` is deny **RETURN** `AuthorizationDenied` so the operation is rejected at the boundary - `inst-pdp-deny`
6. [ ] - `p1` - **RETURN** `Ok(())` to the bound domain component without caching the result; the helper does not propagate constraint sets (intersection with user-supplied read filters lands in the read-path features when their first caller arrives) - `inst-pdp-return`

## 3. Processes / Business Logic (CDSL)

### Plugin Host Binding (Lazy Resolution)

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Input**: the vendor selection (already cached on the gateway-side service from `[usage_collector].vendor` at gear bootstrap), the embedded GTS plugin selector's current cache state (cached instance id after the first successful resolve, or empty before it), and the types-registry client + ClientHub handles obtained from the gear context.

**Output**: a scoped `cpt-cf-usage-collector-contract-storage-plugin` handle for the lazily resolved `PluginBinding` (cached `GtsInstanceId` + scoped trait object reachable via `ClientHub::try_get_scoped` on the GTS instance scope), or a per-call deterministic `PluginUnavailable` outcome on the published Plugin SPI error taxonomy when either the registry call fails, no instance matches the vendor + priority selection, or the scoped client slot is empty. The `usage_collector.plugin.ready` gauge reflects the resulting structural readiness fact; the SPI exposes no plugin-side `ready()` probe (the structural readiness fact is the sole readiness signal — see `cpt-cf-usage-collector-contract-storage-plugin` DoD).

**Steps**:

1. [ ] - `p1` - On every dispatch, enter the gateway-side `cpt-cf-usage-collector-component-plugin-host` resolution seam and invoke its embedded GTS plugin selector cache lookup - `inst-algo-binding-enter-selector`
2. [ ] - `p1` - **IF** the selector cache is empty (cold path) - `inst-algo-binding-cold-path`
   1. [ ] - `p1` - **TRY** query the types-registry for instances of `UsageCollectorPluginSpecV1` by its GTS schema id and apply the configured vendor + priority selection (lowest priority wins) - `inst-algo-binding-resolve-plugin`
   2. [ ] - `p1` - **CATCH** registry-or-selector failure - `inst-algo-binding-catch`
      1. [ ] - `p1` - **RETURN** a `PluginUnavailable` outcome per the published Plugin SPI error taxonomy (covering both registry unavailability and no-match outcomes) on the per-call path; the selector cache remains empty so the next dispatch retries the lazy resolve - `inst-algo-binding-plugin-unavailable-cold`
   3. [ ] - `p1` - Cache the resolved `PluginBinding` exactly once for the service's lifetime - `inst-algo-binding-cache-instance-id`
3. [ ] - `p1` - **ELSE** (warm path) reuse the cached `GtsInstanceId` without a types-registry round-trip - `inst-algo-binding-warm-path`
4. [ ] - `p1` - Resolve the scoped `cpt-cf-usage-collector-contract-storage-plugin` handle via `ClientHub::try_get_scoped` on the GTS instance scope; **IF** the lookup returns no handle **RETURN** a `PluginUnavailable` outcome per the published Plugin SPI error taxonomy on the per-call path - `inst-algo-binding-try-get-scoped`
5. [ ] - `p1` - Compute the structural readiness fact (selector has cached an instance id AND `ClientHub::try_get_scoped` returns a handle under the GTS instance scope) and set `usage_collector.plugin.ready` to `1` when both facts hold or `0` otherwise - `inst-algo-binding-readiness-fact`
6. [ ] - `p1` - **RETURN** the resolved scoped `cpt-cf-usage-collector-contract-storage-plugin` handle to the calling pipeline so the dispatch completes; warm-path subsequent calls hit the selector fast path and the ClientHub read seam and reuse both caches with no further types-registry round-trip - `inst-algo-binding-return`

### PDP Authorize

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Input**: a reference to the `SecurityContext` (resolved upstream by the ToolKit gateway on REST or supplied by the in-process SDK caller), the operation descriptor, and the operation's attribution (catalog ops carry no resource attributes; usage-record submission adds `OWNER_TENANT_ID`, optional `OWNER_ID`, `resource_type`, `resource_id`).

**Output**: `Ok(())` surfaced to the bound domain component on permit, or `AuthorizationDenied` / `AuthorizationUnavailable` on deny / transport failure. The helper does not propagate a constraint set — constraint intersection with user-supplied read filters lands in Usage Query when its first caller arrives.

**Steps**:

1. [ ] - `p1` - Compose the attribution from the caller-supplied operation descriptor and the resolved `SecurityContext`, applying the attributes that match the operation's resource type with `require_constraints(false)` - `inst-algo-pdp-compose`
2. [ ] - `p1` - **TRY** invoke `cpt-cf-usage-collector-contract-authz-resolver` via the shared PDP helper (`enforcer.access_scope_with(...)`) with the composed attribution - `inst-algo-pdp-call`
3. [ ] - `p1` - **CATCH** transport-or-evaluation failure (`EnforcerError::EvaluationFailed`) - `inst-algo-pdp-catch`
   1. [ ] - `p1` - **RETURN** `AuthorizationUnavailable`; never serve a cached or permissive decision - `inst-algo-pdp-fail-closed`
4. [ ] - `p1` - **IF** the returned `PdpDecision` is deny (`EnforcerError::Denied` or fail-closed `EnforcerError::CompileFailed`) **RETURN** `AuthorizationDenied` - `inst-algo-pdp-deny`
5. [ ] - `p1` - **RETURN** `Ok(())` to the bound domain component without caching the result - `inst-algo-pdp-return`

## 4. Definitions of Done

### FR: Pluggable Storage

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-fr-pluggable-storage`

The system **MUST** materialize the active storage backend exclusively through `cpt-cf-usage-collector-contract-storage-plugin`, resolved through the `PluginV1<UsageCollectorPluginSpecV1>` GTS base + `types-registry` + `ClientHub` scoped registration pattern (SDK declares `UsageCollectorPluginSpecV1` in `usage-collector-sdk/src/gts.rs`; plugins publish through `TypesRegistryClient` and register a scoped `dyn UsageCollectorPluginV1` in `ClientHub` under `ClientScope::gts_id(&instance_id)`; the host's `GtsPluginSelector` lazily resolves the instance on the first dispatch call via `get_or_init` and caches the `GtsInstanceId` for the `Service`'s lifetime; subsequent dispatches reuse the cache via `ClientHub::try_get_scoped`). `[usage_collector].vendor` is read once at `Gear::init`; changing the binding requires a gear restart. There is no in-core fallback path and no parallel cache. Per `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`, the bound plugin owns durable storage for both the usage-type catalog and the usage records store, reached exclusively through `cpt-cf-usage-collector-interface-plugin`. The catalog payload shape (`gts_id`, `kind: UsageKind`, `metadata_fields`) is documented in `cpt-cf-usage-collector-feature-usage-type-lifecycle`; the SPI's cross-entity invariants (dedup composite permanence, FK `ON DELETE RESTRICT` enforcement surfaced as `UsageTypeReferenced`, status-only deactivation with depth-1 cascade) are documented in `plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI". Concrete table shapes are plugin-internal and owned by each plugin's own DESIGN document.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-principle-plugin-resolution-via-client-hub`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`

**Touches**:

- Telemetry: `usage_collector.plugin.ready` gauge (set to `0` when no `GtsInstanceId` is cached OR no scoped client exists under `ClientScope::gts_id(instance_id)`; the SPI exposes no plugin-side `ready()` probe)
- Entities: `PluginBinding`

### FR: Data Classification

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-fr-data-classification`

The data the gear persists **MUST** belong to one of three classes, intrinsic to the data itself (not carried on the `SecurityContext`): (1) **opaque platform identifiers** (tenant IDs, resource IDs, subject IDs, UsageType `gts_id`), treated as opaque references with no resolution paths introduced inside the gear; (2) **operational telemetry** (counters, gauges, latencies, structured logs, span attributes); (3) **caller-supplied metadata** (the closed `metadata_fields` keys declared at UsageType registration plus their `String` values). Caller-supplied metadata **MUST NOT** contain PII, payment data, regulated health data, or credentials; this is a product-level contract on usage sources enforced upstream of the gear boundary. The collector takes no data-classification decision locally — it persists the classes named above through `cpt-cf-usage-collector-contract-storage-plugin` and does not introduce any other class.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-fr-data-classification`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `SecurityContext`

### NFR: Availability

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-nfr-availability`

The system **MUST** sustain **99.95% monthly uptime** for usage ingestion endpoints. As the foundation-owned realization, the collector keeps its stateless runtime instances reachable through the platform API gateway so that PDP and Plugin SPI dispatch remain available whenever the bound plugin's structural readiness fact (selector cached AND `ClientHub::try_get_scoped` returns `Some`) holds and is surfaced by the `usage_collector.plugin.ready` gauge. AuthN availability is owned by the ToolKit gateway upstream and is not part of the collector's readiness surface; gear-local liveness and readiness HTTP probes are likewise owned by the ToolKit host above the gear boundary.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-nfr-availability`

**Touches**:

- Telemetry: `usage_collector.plugin.ready` gauge
- Entities: `PluginBinding`

### NFR: Plugin Contract Stability

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-nfr-plugin-contract-stability`

The system **MUST** treat each of the three public surfaces — `cpt-cf-usage-collector-interface-plugin` (Plugin SPI / `cpt-cf-usage-collector-contract-storage-plugin`), `cpt-cf-usage-collector-interface-sdk-client` (SDK trait), and `cpt-cf-usage-collector-interface-rest-api` (REST API) — as **stable within a major version**, with **at most one prior major version supported concurrently per surface**, and **MUST** carry any breaking change on a new major version. The foundation feature owns the Plugin SPI realization: any breaking change there MUST be carried on a versioned suffix so vendors can ship and migrate backends independently of the core release train. The same major-version stability obligation applies to the SDK trait and REST API surfaces.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-nfr-plugin-contract-stability`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### NFR: Operational Visibility

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-nfr-operational-visibility`

The system **MUST** expose the following operational visibility surface (the catalog of signals/alerts mirrors the long-term PRD list; foundation realizes only the items called out below as already wired):

- **Structural readiness signal**: `usage_collector.plugin.ready` gauge (realized today). The remaining signals from the PRD list (ingestion latency, ingestion throughput, query latency, PDP error rate, plugin acceptance error rate, usage-type-catalog freshness) land alongside the features that own those operations.
- **Structured logs**: every API operation (100%) **MUST** emit structured logs carrying the `correlation_id`, `trace_id`, and `request_id` correlation identifiers (realized today via ToolKit's W3C TraceContext propagator wired through `init_tracing`).
- **Log retention**: operational logs **MUST** be retained for **≥ 30 calendar days** (platform-owned obligation; not realized in gear source).
- **Alert categories** and **dashboards**: PRD-pinned obligations realized once the long-term signal catalog is wired in downstream features.

As the foundation-owned realization, the gear constructs all foundation-owned instruments on `opentelemetry::global::meter_with_scope(MODULE_NAME)` at bootstrap so that they appear in the OTLP stream emitted by ToolKit's `SdkMeterProvider`; propagates `trace-id` and `request-id` headers per W3C TraceContext (enabled by ToolKit's `init_tracing`) so every emitted log, metric exemplar, and span shares the same correlation identifiers; and **MUST NOT** expose any in-gear HTTP metrics endpoint — metrics reach the collector exclusively through the OTLP push path established by ToolKit's `SdkMeterProvider`. Platform liveness and readiness HTTP probes are owned by the ToolKit host above the gear boundary; the collector contributes only the structural-readiness gauge `usage_collector.plugin.ready`. `trace_id` and `request_id` are carried in structured logs and span attributes only, never as OTLP metric labels. This DoD remains unchecked until the long-term signal catalog and the platform-owned obligations (log retention, alerts, dashboards) are wired up.

**Implements**:

- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-nfr-operational-visibility`

**Touches**:

- Telemetry: `usage_collector.plugin.ready` gauge
- Entities: `PluginBinding`, `SecurityContext`

### Principle: Fail Closed

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-fail-closed`

The system **MUST** fail closed whenever the inbound `SecurityContext` is missing or invalid, the PDP resolver is unreachable, or the storage plugin binding is unreachable or returns an unexpected outcome — never synthesize identity, never serve a cached decision, never invent a binding.

**Implements**:

- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `SecurityContext`, `PdpDecision`, `PluginBinding`

### Principle: Pluggable Storage

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-pluggable-storage`

The system **MUST** keep the storage backend pluggable behind `cpt-cf-usage-collector-contract-storage-plugin` and reach durable state exclusively through the ClientHub-bound plugin handle.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-principle-pluggable-storage`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Principle: Contract Stability

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-contract-stability`

The system **MUST** evolve every published contract surface — `cpt-cf-usage-collector-contract-storage-plugin`, `cpt-cf-usage-collector-contract-authz-resolver`, `cpt-cf-usage-collector-contract-gts-registry` — through versioned, additive changes so existing consumers and backend implementors continue to bind without code change.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`

**Constraints**: `cpt-cf-usage-collector-principle-contract-stability`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`, `cpt-cf-usage-collector-interface-sdk-client`, `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PluginBinding`, `SecurityContext`

### Principle: PDP-Centric Authorization

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-pdp-centric-authorization`

The system **MUST** dispatch every read and write operation through `cpt-cf-usage-collector-contract-authz-resolver` for a permit/deny `PdpDecision` plus the `PdpConstraint` set, never serving a cached decision and never deriving authorization locally.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `cpt-cf-usage-collector-principle-pdp-centric-authorization`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PdpDecision`, `PdpConstraint`, `SecurityContext`

### Principle: Plugin Resolution via ClientHub

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-plugin-resolution-via-client-hub`

The system **MUST** resolve storage-plugin binding through the platform's `PluginV1<P>` GTS base type, `types-registry`, and `ClientHub` scoped registration so that plugin discovery and per-request dispatch follow the platform-standard pattern shared with `credstore`, `authn-resolver`, and `authz-resolver`. The host's Plugin Host component caches the resolved instance id in a `GtsPluginSelector` for the `Service`'s lifetime; per-request dispatch is an in-memory scoped lookup with no `types-registry` round-trip on the warm path, and the host crate has no compile-time dependency on any concrete plugin crate (binding is settled at startup, not per request).

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-principle-plugin-resolution-via-client-hub`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Principle: OTLP Push Emission

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-otlp-push-emission`

The system **MUST** push operational telemetry via OTLP from ToolKit's global `SdkMeterProvider`, constructing all foundation-owned instruments on `opentelemetry::global::meter_with_scope("usage_collector", …)` at gear bootstrap; it **MUST NOT** expose an in-gear HTTP scrape endpoint, **MUST NOT** instantiate its own exporter, and **MUST NOT** reintroduce a `/metrics` path. Downstream pipeline concerns (log shippers, trace exporters, OTLP collector and backend selection, dashboards, retention) are governed by the platform `[opentelemetry]` config block and are not gear-owned concerns.

**Implements**:

- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-principle-otlp-push-emission`

**Touches**:

- Telemetry: `usage_collector.plugin.ready` gauge via ToolKit's `SdkMeterProvider`
- Entities: `SecurityContext`

### Principle: Gateway HTTP Server Instrument Reuse

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-principle-gateway-http-server-instrument-reuse`

The system **MUST** reuse the fixed set of OTel-semantic-conventions `http.server.*` instruments (request duration histogram, active requests gauge) emitted by the platform API gateway middleware in front of every REST handler, and **MUST NOT** redeclare them inside the gear. These instruments are exported through the same `SdkMeterProvider` and OTLP pipeline as the gear-scoped `usage_collector.*` inventory and count as part of the gear's observability contract alongside the gear-scoped instruments.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-principle-gateway-http-server-instrument-reuse`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Telemetry: gateway-emitted `http.server.*` instruments exported via ToolKit's `SdkMeterProvider`
- Entities: `SecurityContext`

### Constraint: Plugin Contract Stability

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-constraint-plugin-contract-stability`

The system **MUST** treat `cpt-cf-usage-collector-contract-storage-plugin` as the only durable-state interface and refuse to introduce parallel storage paths that bypass the binding.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-constraint-plugin-contract-stability`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Constraint: Vendor Pluggable

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-constraint-vendor-pluggable`

The system **MUST** keep concrete vendor backends out of the foundation feature so any compliant `cpt-cf-usage-collector-contract-storage-plugin` implementation can be bound through the GTS instance selector without core changes.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-constraint-vendor-pluggable`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Constraint: NFR Thresholds

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-constraint-nfr-thresholds`

The system **MUST** preserve the foundation's stateless, horizontally-scaled topology so that downstream availability, scalability, and capacity-headroom NFR thresholds remain valid as feature surfaces are added.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PluginBinding`

### ADR: Contract Stability

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-adr-contract-stability`

The system **MUST** carry every breaking change to a published surface on a versioned suffix per the contract-stability ADR, so existing implementors continue to bind through `cpt-cf-usage-collector-contract-storage-plugin` and `cpt-cf-usage-collector-interface-sdk-client` without recompilation.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`

**Constraints**: `cpt-cf-usage-collector-adr-contract-stability`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`, `cpt-cf-usage-collector-interface-sdk-client`
- Entities: `PluginBinding`

### ADR: PDP-Centric Authorization

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-adr-pdp-centric-authorization`

The system **MUST** route every authorization decision through `cpt-cf-usage-collector-contract-authz-resolver` per the PDP-centric authorization ADR; no local policy table, no cached decision, no derived bypass.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `cpt-cf-usage-collector-adr-pdp-centric-authorization`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PdpDecision`, `PdpConstraint`

### ADR: Pluggable Storage

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-adr-pluggable-storage`

The system **MUST** retain pluggable storage as the only durable-state path per the pluggable-storage ADR (`cpt-cf-usage-collector-adr-pluggable-storage`), binding the active backend exclusively through `cpt-cf-usage-collector-contract-storage-plugin` resolved against the GTS instance selector. Per `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference` the usage-type catalog is the sole catalog and lives on the pluggable-storage substrate alongside `usage_records`; no gateway-local `usage_type_catalog` table is provisioned.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-adr-pluggable-storage`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Contract: Storage Plugin

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-contract-storage-plugin`

The system **MUST** publish `cpt-cf-usage-collector-contract-storage-plugin` as the sole durable-state contract and register the bound plugin in ClientHub with GTS instance scope so the host's structural readiness fact (selector cached AND `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>` returns `Some`) drives the `usage_collector.plugin.ready` gauge; the contract exposes no plugin-side `ready()` probe. Per `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`, the contract carries the catalog write/read/list/delete/reference-check surface alongside the usage-records surface; the catalog payload shape, the cross-entity invariants (FK `ON DELETE RESTRICT` enforcement surfaced as `UsageTypeReferenced`, dedup composite permanence, status-only deactivation atomicity), and the gateway↔plugin error taxonomy are owned by `plugin-spi.md` and are referenced — not redefined — here.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-contract-storage-plugin`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Contract: AuthZ Resolver

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-contract-authz-resolver`

The system **MUST** dispatch every operation through `cpt-cf-usage-collector-contract-authz-resolver`, propagate the audit-correlation context on the call (ambient W3C TraceContext via ToolKit's `init_tracing`), and emit a deterministic platform-authorization error envelope on resolver transport failure (`AuthorizationUnavailable`) or PDP deny (`AuthorizationDenied`). The shared `domain/authz.rs` helpers call the resolver with `require_constraints(false)` per ADR-0012 / PRD §5.8 (catalog is platform-global; ingestion uses subject-only PDP for v1) — an unconstrained permit (`allow_all`) is a legitimate happy-path outcome. Constraint-set intersection with user-supplied read filters lands in Usage Query when its first caller arrives.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `cpt-cf-usage-collector-contract-authz-resolver`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PdpDecision`, `PdpConstraint`

### Contract: GTS Registry

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-contract-gts-registry`

The system **MUST** resolve the active storage plugin identity through `cpt-cf-usage-collector-contract-gts-registry` from the `[usage_collector].vendor` value cached at `Gear::init`, lazily on the first dispatch call after the `types-registry` is consistent (single-flight `GtsPluginSelector::get_or_init`), and cache the resolved `GtsInstanceId` for the `Service`'s lifetime; subsequent binding changes require a gear restart.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-contract-gts-registry`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Entities: `PluginBinding`

### Entity: PluginBinding

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-entity-plugin-binding`

The system **MUST** materialize `PluginBinding` exclusively through the Plugin Host (the host gear's own Service) using the GTS-resolved plugin identity. Binding state is the two structural facts recomputed per call by the `cpt-cf-usage-collector-flow-foundation-plugin-host-binding` flow (selector-cached `GtsInstanceId` AND `ClientHub::try_get_scoped` returns `Some`); the prior finite-state-machine model (`Unbound`/`Resolving`/`Bound`/`Refreshing`/`Failed`) was removed because it is not present in the reference gears.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `PluginBinding`

**Touches**:

- API: `cpt-cf-usage-collector-interface-plugin`
- Telemetry: `usage_collector.plugin.ready` gauge
- Entities: `PluginBinding`

### Entity: SecurityContext

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-entity-security-context`

The system **MUST** carry the inbound `SecurityContext` (resolved by the ToolKit gateway upstream of the collector on the REST surface, or supplied by the in-process caller on the SDK surface) through every PDP, plugin, and operational-event boundary without local mutation.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `SecurityContext`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `SecurityContext`

### Entity: PdpDecision

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-entity-pdp-decision`

The system **MUST** lift the `PdpDecision` returned by `cpt-cf-usage-collector-contract-authz-resolver` into the appropriate outcome on every call: a permit returns `Ok(())` to the bound domain component, a deny (or fail-closed `CompileFailed`) returns `AuthorizationDenied`, and a transport-or-evaluation failure returns `AuthorizationUnavailable`. The decision is never cached and never derived locally; the explicit `PdpDecision` value type is not materialized in the foundation surface because the shared `domain/authz.rs` helpers collapse the outcome into `Result<(), DomainError>` at the call site.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `PdpDecision`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PdpDecision`

### Entity: PdpConstraint

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-foundation-entity-pdp-constraint`

The foundation **MUST NOT** materialize the `PdpConstraint` set as a propagated value type in v1. The shared `domain/authz.rs` helpers call the resolver with `require_constraints(false)` per ADR-0012 / PRD §5.8 (catalog is platform-global; ingestion uses subject-only PDP for v1), so an unconstrained permit (`allow_all`) is a legitimate happy-path outcome and no constraint payload reaches downstream code. Constraint-set materialization, intersection with user-supplied read filters, and widen-rejection all land in Usage Query when its first caller arrives — that feature owns the surfacing and propagation seam. This DoD remains unchecked until then.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-foundation-pdp-authorize`

**Constraints**: `PdpConstraint`

**Touches**:

- API: `cpt-cf-usage-collector-interface-rest-api`
- Entities: `PdpConstraint`

### Component: Plugin Host

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-foundation-component-plugin-host`

The system **MUST** realize `cpt-cf-usage-collector-component-plugin-host` as the sole owner of lazy binding resolution (`GtsPluginSelector::get_or_init` on the first dispatch after the `types-registry` is consistent, cached for the `Service`'s lifetime) and the `usage_collector.plugin.ready` structural readiness gauge. Scoped `dyn UsageCollectorPluginV1` registration in `ClientHub` is owned by each `usage-collector-plugin-<backend>` crate's own `init()`, not by the host.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-plugin-host-binding`
- `cpt-cf-usage-collector-algo-foundation-plugin-host-binding`

**Constraints**: `cpt-cf-usage-collector-component-plugin-host`

**Touches**:

- Telemetry: `usage_collector.plugin.ready` gauge
- Entities: `PluginBinding`

### §2.1-item → DoD-ID Coverage Matrix

Coverage of every DECOMPOSITION §2.1 catalog item:

| §2.1 source ID                                                          | §2.1 kind         | DoD ID                                                                                 |
| ----------------------------------------------------------------------- | ----------------- | -------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-fr-pluggable-storage`                           | FR                | `cpt-cf-usage-collector-dod-foundation-fr-pluggable-storage`                           |
| `cpt-cf-usage-collector-fr-data-classification`                         | FR                | `cpt-cf-usage-collector-dod-foundation-fr-data-classification`                         |
| `cpt-cf-usage-collector-nfr-availability`                               | NFR               | `cpt-cf-usage-collector-dod-foundation-nfr-availability`                               |
| `cpt-cf-usage-collector-nfr-plugin-contract-stability`                  | NFR               | `cpt-cf-usage-collector-dod-foundation-nfr-plugin-contract-stability`                  |
| `cpt-cf-usage-collector-nfr-operational-visibility`                     | NFR               | `cpt-cf-usage-collector-dod-foundation-nfr-operational-visibility`                     |
| `cpt-cf-usage-collector-principle-fail-closed`                          | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-fail-closed`                          |
| `cpt-cf-usage-collector-principle-pluggable-storage`                    | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-pluggable-storage`                    |
| `cpt-cf-usage-collector-principle-contract-stability`                   | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-contract-stability`                   |
| `cpt-cf-usage-collector-principle-pdp-centric-authorization`            | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-pdp-centric-authorization`            |
| `cpt-cf-usage-collector-principle-plugin-resolution-via-client-hub`     | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-plugin-resolution-via-client-hub`     |
| `cpt-cf-usage-collector-principle-otlp-push-emission`                   | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-otlp-push-emission`                   |
| `cpt-cf-usage-collector-principle-gateway-http-server-instrument-reuse` | Principle         | `cpt-cf-usage-collector-dod-foundation-principle-gateway-http-server-instrument-reuse` |
| `cpt-cf-usage-collector-constraint-plugin-contract-stability`           | Design constraint | `cpt-cf-usage-collector-dod-foundation-constraint-plugin-contract-stability`           |
| `cpt-cf-usage-collector-constraint-vendor-pluggable`                    | Design constraint | `cpt-cf-usage-collector-dod-foundation-constraint-vendor-pluggable`                    |
| `cpt-cf-usage-collector-constraint-nfr-thresholds`                      | Design constraint | `cpt-cf-usage-collector-dod-foundation-constraint-nfr-thresholds`                      |
| `cpt-cf-usage-collector-adr-contract-stability`                         | ADR-derived       | `cpt-cf-usage-collector-dod-foundation-adr-contract-stability`                         |
| `cpt-cf-usage-collector-adr-pdp-centric-authorization`                  | ADR-derived       | `cpt-cf-usage-collector-dod-foundation-adr-pdp-centric-authorization`                  |
| `cpt-cf-usage-collector-adr-pluggable-storage`                          | ADR-derived       | `cpt-cf-usage-collector-dod-foundation-adr-pluggable-storage`                          |
| `cpt-cf-usage-collector-contract-storage-plugin`                        | Contract          | `cpt-cf-usage-collector-dod-foundation-contract-storage-plugin`                        |
| `cpt-cf-usage-collector-contract-authz-resolver`                        | Contract          | `cpt-cf-usage-collector-dod-foundation-contract-authz-resolver`                        |
| `cpt-cf-usage-collector-contract-gts-registry`                          | Contract          | `cpt-cf-usage-collector-dod-foundation-contract-gts-registry`                          |
| `PluginBinding`                          | Domain entity     | `cpt-cf-usage-collector-dod-foundation-entity-plugin-binding`                          |
| `SecurityContext`                        | Domain entity     | `cpt-cf-usage-collector-dod-foundation-entity-security-context`                        |
| `PdpDecision`                            | Domain entity     | `cpt-cf-usage-collector-dod-foundation-entity-pdp-decision`                            |
| `PdpConstraint`                          | Domain entity     | `cpt-cf-usage-collector-dod-foundation-entity-pdp-constraint`                          |
| `cpt-cf-usage-collector-component-plugin-host`                          | Design component  | `cpt-cf-usage-collector-dod-foundation-component-plugin-host`                          |

Coverage totals: FR=6, NFR=13, Principle=7, Design constraint=4, ADR-derived=3, Contract=3, Domain entity=4, Design component=1 — total 41 DoD entries, zero duplicates. `cpt-cf-usage-collector-fr-tenant-isolation` is intentionally not realized in foundation: tenant-isolation enforcement (PDP-constraint intersection with user-supplied read filters and widen-rejection) lands in the Usage Query feature alongside its first read-path caller; the foundation contract is the surfacing and propagation seam only.

## 5. Acceptance Criteria

- [ ] `p1` - At gear bootstrap with a valid `[usage_collector].vendor` configuration, the foundation constructs the `Service` with an embedded `GtsPluginSelector` (no `types-registry` query is issued at bootstrap); each `usage-collector-plugin-<backend>` `init()` independently registers its scoped `dyn UsageCollectorPluginV1` in `ClientHub` under `ClientScope::gts_id(&instance_id)`. On the first dispatch call after the `types-registry` is consistent, the host lazily resolves the binding via `GtsPluginSelector::get_or_init` and publishes `usage_collector.plugin.ready=1` through the OTLP stream emitted by ToolKit's `SdkMeterProvider` once the structural readiness fact holds; while resolution has not yet succeeded (or the `types-registry` is unreachable on the per-call path), the dispatch returns the deterministic `plugin-unavailable` error envelope and the gauge reads `0`.
- [ ] `p1` - The host's `GtsPluginSelector` performs lazy single-flight resolution on the first dispatch call after the `types-registry` is consistent and caches the resolved `GtsInstanceId` for the `Service`'s lifetime; a per-call dispatch whose scoped slot in `ClientHub` is empty returns the deterministic `plugin-unavailable` error envelope (mirroring `gears/credstore/credstore/src/domain/service.rs:57-74`) without inventing a binding or substituting a prior one. Binding changes require a gear restart.
- [ ] `p1` - Every REST and SDK operation that arrives without a resolved `SecurityContext` is rejected at the boundary with a deterministic error envelope and no operation is dispatched to the bound plugin; the collector never synthesizes identity and never holds credentials, because AuthN is owned by the ToolKit gateway upstream.
- [ ] `p1` - When `cpt-cf-usage-collector-contract-authz-resolver` is unreachable / times out the call returns `AuthorizationUnavailable`; when the resolver returns deny (or fail-closed `CompileFailed`) the call returns `AuthorizationDenied`; in both cases no cached or permissive decision is ever served. Per ADR-0012 / PRD §5.8 the shared `domain/authz.rs` helpers call the resolver with `require_constraints(false)` for v1, so an unconstrained permit (`allow_all`) is a legitimate happy-path outcome and there is no foundation-side empty-constraint gate.
- [ ] `p1` - Every inbound request that arrives with a W3C `traceparent` (and any accompanying `tracestate`) causes that `trace-id` plus the captured `request-id` correlation pair to appear on every downstream PDP call (through the shared `domain/authz.rs` helpers, both wrapped in `#[tracing::instrument]` spans) and Plugin SPI call and on every structured operational event emitted by the gear; the W3C Trace Context pair (`traceparent` plus optional `tracestate`) is co-propagated end-to-end per DESIGN §3.11.5 (realized by ToolKit's `init_tracing`, which installs the `TraceContextPropagator` globally) and the outbound REST response and SDK return value reflect the resulting `traceparent`.
- [ ] `p1` - Platform liveness and readiness probes are handled by the ToolKit host above the gear boundary; the collector exposes no gear-local health endpoints. The foundation-owned instrument `usage_collector.plugin.ready` is visible in the OTLP stream emitted by ToolKit's `SdkMeterProvider` and flips to `0` whenever the bound plugin's structural readiness fact stops holding (selector cache missing OR `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>` returns `None`; the SPI exposes no plugin-side `ready()` probe). Additional readiness gauges and cause-classified counters (PDP readiness, PDP failures, plugin accept errors) are intentionally out of foundation scope for v1.
- [ ] `p1` - The Plugin SPI (`cpt-cf-usage-collector-interface-plugin`), SDK trait (`cpt-cf-usage-collector-interface-sdk-client`), and REST API (`cpt-cf-usage-collector-interface-rest-api`) are published with the sibling specifications (`plugin-spi.md`, `sdk-trait.md`, `usage-collector-v1.yaml`), accept gateway-resolved `SecurityContext` values at the boundary, operate through `cpt-cf-usage-collector-contract-authz-resolver`, and expose no data path that bypasses `cpt-cf-usage-collector-contract-storage-plugin`.
- [ ] `p2` - Any breaking change to a published contract surface is carried on a versioned suffix, so existing in-process SDK consumers and storage backend implementors continue to bind without recompilation across foundation revisions.
- [ ] `p1` - **Given** a storage plugin bound through `cpt-cf-usage-collector-contract-storage-plugin` whose `usage_type_catalog` table is empty for the candidate `gts_id`, **when** any caller attempts to insert a `usage_records` row carrying that `gts_id`, **then** the gateway dispatches `get_usage_type` against the plugin SPI, observes the unknown `gts_id` before any write dispatch, and the request is rejected with the canonical `unknown_usage_type` discriminator (`UsageTypeNotFound` envelope mapped to HTTP `404 NotFound` per DESIGN §3.3 Error Envelopes); the plugin's in-database `ON DELETE RESTRICT` foreign key on `usage_records.gts_id` → `usage_type_catalog(gts_id)` is the storage backstop that rejects any insert that bypasses the gateway check, ensuring no orphan `usage_records.gts_id` rows can exist (referential-integrity invariant; the SPI `UsageTypeNotFound` variant is raised by both `get_usage_type` and `delete_usage_type` per `plugin-spi.md` §Error Taxonomy, but the gateway re-classifies the ingestion-path miss as `unknown_usage_type` so the wire reason distinguishes the catalog-admin GET case from the ingestion-path miss).
- [ ] `p1` - **Given** a storage plugin bound through `cpt-cf-usage-collector-contract-storage-plugin` whose `usage_type_catalog` table holds a row for `gts_id = G` and whose `usage_records` table holds at least one row whose `gts_id = G`, **when** any caller invokes the catalog delete SPI for `gts_id = G`, **then** the plugin's `ON DELETE RESTRICT` foreign key rejects the delete inside the same transaction and the SPI surfaces a structured `UsageTypeReferenced` error to the gateway that carries the `gts_id` and a sample reference count, no `usage_type_catalog` row is removed, and no `usage_records` row is mutated — preserving the `cpt-cf-usage-collector-adr-caller-supplied-attribution` invariant by construction (referential-delete semantics).
