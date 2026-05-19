<!-- Updated: 2026-04-07 by Constructor Tech -->

# ADR-0002: Two-Phase PDP Authorization for Usage Record Emission


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Subject handling — resolution at `.authorize()`](#subject-handling--resolution-at-authorize)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [PDP call synchronously at `emit()` time (inside or adjacent to DB transaction)](#pdp-call-synchronously-at-emit-time-inside-or-adjacent-to-db-transaction)
  - [Authorization deferred to gateway at delivery time (dispatcher → gateway)](#authorization-deferred-to-gateway-at-delivery-time-dispatcher--gateway)
  - [Pre-loaded static SDK policy (config-based metric allowlist, no PDP)](#pre-loaded-static-sdk-policy-config-based-metric-allowlist-no-pdp)
  - [Two-phase PDP: `with_*().authorize()` before transaction + in-memory constraint evaluation at `enqueue()`](#two-phase-pdp-withauthorize-before-transaction--in-memory-constraint-evaluation-at-enqueue)
- [More Information](#more-information)
  - [TOCTOU Window Analysis](#toctou-window-analysis)
  - [Performance Budget](#performance-budget)
- [Review Cadence](#review-cadence)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-two-phase-emit-authz`

## Context and Problem Statement

The SDK's `emit()` call must persist usage records to the local outbox within the source service's DB transaction, as required by the transactional outbox pattern. Performing PDP authorization synchronously at `emit()` time would require a network call inside or adjacent to an open DB transaction, holding database locks during network I/O and blocking the source service on external service availability. Deferring authorization to delivery time (dispatcher → gateway) avoids this but produces opaque late rejections: the source's original caller has already received a success response, making it impossible to surface authorization failures meaningfully. The system needs a mechanism to enforce authorization checks at request time with immediate failure feedback, without violating the quick injection principle.

## Decision Drivers

* `emit()` MUST NOT make network calls or block on external services — records must be enqueued into the local outbox as fast as a DB insert
* Authorization checks MUST NOT be performed inside an open DB transaction — network calls holding DB locks violate platform conventions and degrade source service throughput
* Authorization errors MUST be surfaced to the source service's caller at request time, before any domain operation is committed
* The system MUST fail closed on authorization failure (`cpt-cf-usage-collector-principle-fail-closed`)
* The `UsageEmitterFactory` from ADR 0001 (`cpt-cf-usage-collector-adr-scoped-emit-source`) is already instantiated per source module (one factory per module, vended by `runtime.factory(MODULE_NAME)`) and holds the source module identity — it is the natural owner of emit authorization logic
* The existing `authz-resolver-sdk` `PolicyEnforcer` / `AuthZResolverClient` must be reused without introducing new authorization primitives

## Considered Options

* PDP call synchronously at `emit()` time (inside or adjacent to DB transaction)
* Authorization deferred to gateway at delivery time (dispatcher → gateway)
* Pre-loaded static SDK policy (config-based metric allowlist, no PDP)
* Two-phase PDP: `with_*().authorize()` before transaction returns `UsageEmitter`; `emit()` evaluates constraints in-memory

## Decision Outcome

Chosen option: "Two-phase PDP: `with_*().authorize()` before transaction + in-memory constraint evaluation at `enqueue()`", because it is the only option that satisfies all three constraints simultaneously: no network call inside a DB transaction, immediate failure feedback at request time, and reuse of the existing platform PDP infrastructure.

`UsageEmitterFactory` (layer 2 in the Runtime/Factory/Emitter architecture; see `cpt-cf-usage-collector-adr-scoped-emit-source`) carries two responsibilities:

1. A linear builder chain `factory.with_tenant(t)?.with_subject(...)?...authorize(ctx, resource_id, resource_type) -> Result<UsageEmitter, UsageEmitterError>` — invoked before the DB transaction opens. Tenant and subject overrides are explicit via the `.with_*()` methods (each consumes `self` and returns `Self`; the factory is cheaply cloneable); see §Subject handling below for the three-state subject contract (`with_subject(Subject)` / `without_subject()` / unset). The terminal `.authorize(...)` step contacts the PDP for `USAGE_RECORD`/`CREATE`, fetches the allowed-metrics configuration for the module from the gateway, and returns a per-call-site `UsageEmitter` token on success — or propagates a denial error immediately.
2. `usage_record_builder(metric, value)?.build()? → enqueue(record)` on the returned `UsageEmitter` — called inside the DB transaction; evaluates the pre-fetched constraints against the record in-memory (no I/O), then inserts the outbox row.

The `UsageEmitter` type (layer 3) carries the PDP permit result, the authorized `tenant_id`/`resource_id`/`resource_type`, and the allowed-metrics list fetched from the gateway; it is opaque to the calling module. A denial from the PDP is surfaced as an `Err` from `.authorize()`, so `UsageEmitter` is only constructed on success. In-memory constraint evaluation uses the `allowed_metrics` list to validate metric names and kinds against the `UsageRecord` fields without any network I/O.

`UsageEmitter` includes `issued_at: Instant` (set at construction time). `enqueue()` calls `UsageEmitter::validate_authorization_freshness()` as the first step before any constraint evaluation or outbox INSERT; if `issued_at.elapsed() > authorization_max_age`, `enqueue()` returns `UsageEmitterError::AuthorizationExpired`. The `authorization_max_age` is a configurable field on `UsageEmitterConfig` (default 30 seconds) — well above any normal request handler duration and well below the minimum operationally meaningful policy propagation delay. This provides runtime enforcement of the single-request constraint without relying solely on code review.

### Subject handling — resolution at `.authorize()`

Subject identity is carried end-to-end as `Option<Subject>` on the wire and inside `UsageRecord`, where `Subject { id: Uuid, r#type: Option<String> }` lives in `usage-collector-sdk::models`. Three legal *wire-level* states exist — "no subject," "id only," "id + type" — exactly the three states expressible by `Option<Subject>` plus the optionality of `Subject.r#type`. The previously-typeable fourth state ("type without id") is no longer representable.

At the factory layer, however, `Option<Subject>` alone is **insufficient**: it cannot distinguish "fall back to `SecurityContext`" from "explicit no subject," and collapsing both into the same `None` would silently substitute a forwarder's own service identity for an absent caller-supplied subject (violating the substitution invariant below). The factory therefore stores a three-state intent enum:

```rust
enum SubjectChoice {
    /// Default — resolve from `SecurityContext` at `.authorize()` time.
    DefaultFromCtx,
    /// Caller-supplied: `Some(s)` = use exactly `s`; `None` = no subject at all.
    Explicit(Option<Subject>),
}
```

The fluent setters map cleanly to this enum:

- never called → `SubjectChoice::DefaultFromCtx` (the runtime default for in-process callers whose identity *is* the subject)
- `.with_subject(s)` → `SubjectChoice::Explicit(Some(s))`
- `.without_subject()` → `SubjectChoice::Explicit(None)`

`.authorize()` resolves the choice in a single `match`:

```rust
let subject = match &self.subject {
    SubjectChoice::Explicit(s) => s.clone(),
    SubjectChoice::DefaultFromCtx => Some(subject_from_ctx(ctx)),
};
```

The resolved `Option<Subject>` is sent to the PDP and stamped into the returned `UsageEmitter`. The builder accepts `with_subject(Subject)`, and `UsageRecord` carries `subject: Option<Subject>` — the same wire shape flows uninterrupted from DTO to outbox payload.

Tenant resolution mirrors only the *default-from-ctx* half of this contract: `tenant.unwrap_or_else(|| ctx.subject_tenant_id())`. Tenant does not need a third state because there is no analogous "explicit no tenant" semantics — every record is owned by some tenant.

The default-from-ctx choice is the right one for in-process modules whose caller identity *is* the subject; forwarders and REST ingest handlers MUST instead translate their wire-level `Option<Subject>` faithfully — `Some(s) → .with_subject(s)`, `None → .without_subject()` — so the forwarder's own service identity is never substituted for the original caller's, and the PDP either authorizes the caller's subject or sees no subject at all.

**PDP attribute schema is unchanged.** The PDP property-name constants `SUBJECT_ID` and `SUBJECT_TYPE` (defined in `usage-collector-sdk::authz::properties`) remain flat at the policy boundary — the PDP still receives two separate `resource_property` entries. The `Subject` struct is flattened back into those two flat properties at a single inline `if let Some(s) = &subject { ... }` block in `usage-emitter/src/domain/factory.rs`:

```rust
if let Some(s) = &subject {
    request = request.resource_property(authz::properties::SUBJECT_ID, s.id.to_string());
    if let Some(ty) = &s.r#type {
        request = request.resource_property(authz::properties::SUBJECT_TYPE, ty.as_str());
    }
}
```

Reviewers must not be misled into thinking the policy contract moved: only the Rust API and the JSON wire shape changed; PDP policies authored against `SUBJECT_ID` / `SUBJECT_TYPE` continue to evaluate the same attributes against the same values.

### Consequences

* Good, because the PDP call happens before any DB transaction opens — no network I/O holds DB locks
* Good, because authorization errors are surfaced at request time via the `.authorize()` failure — the source caller receives an immediate, meaningful error before any domain operation commits
* Good, because `enqueue()` performs only in-memory constraint evaluation and an outbox INSERT — no external calls on the critical emission path
* Good, because the implementation reuses `authz-resolver-sdk` `PolicyEnforcer` for the PDP call without introducing new authorization primitives
* Good, because `UsageEmitterFactory` (established in ADR 0001) is the natural and auditable owner of the `.authorize()` step, which already carries the source module identifier for PDP resource property attribution
* Good, because `UsageEmitter` is opaque to consuming modules — no authz concepts leak into business code at call sites
* Bad, because source services must run the `.authorize()` step once per request that may emit usage — adds one PDP round-trip per request on the non-transaction path
* Bad, because the emission call site changes from a simple `emit(ctx, record)` to `factory.with_*().authorize(ctx, resource_id, resource_type)` → `usage_record_builder(...)?.build()? → enqueue(record)`, changing the SDK API surface
* Bad, because in-memory allowed-metrics validation requires per-metric list lookup added to the `UsageEmitter` — the existing framework only compiles PDP constraints to SQL via `AccessScope`
* Bad, because when the platform PDP (`authz-resolver`) is unavailable, `.authorize()` returns an error and all usage emission from the source fails for the duration of the outage — this is an intentional fail-closed behavior per `cpt-cf-usage-collector-principle-fail-closed`, but it couples usage metering availability to PDP availability; PDP uptime must be treated as a hard dependency for usage sources

### Confirmation

* Unit tests: `factory.with_*().authorize(...)` propagates PDP denial as `UsageEmitterError::AuthorizationFailed` without inserting any outbox row
* Unit tests: `enqueue()` with a `UsageEmitter` whose allowed-metrics list excludes the record's metric name returns `UsageEmitterError::MetricNotAllowed` without inserting any outbox row
* Unit tests: `enqueue()` with an allowed-metrics list that includes the record's metric succeeds and inserts the outbox row
* Unit tests: `enqueue()` with a `UsageEmitter` whose `issued_at` is older than `authorization_max_age` returns `UsageEmitterError::AuthorizationExpired` without inserting any outbox row
* Unit tests: two successive calls to `enqueue()` with the same `UsageEmitter` instance both succeed when within `authorization_max_age`; freshness enforcement is time-based, not single-use (replay within the window is bounded to one request lifetime by construction)
* Integration test: source service calls `factory.with_*().authorize(...)` before opening a DB transaction, then calls `usage_record_builder(...)?.build()? → enqueue(record)` inside the transaction — confirms no PDP or other network calls occur while the transaction is open

## Pros and Cons of the Options

### PDP call synchronously at `emit()` time (inside or adjacent to DB transaction)

Perform the PDP network call immediately when `emit()` is invoked, either inside the caller's DB transaction or in a preamble that still holds a DB connection.

* Good, because no additional call site ceremony — emit semantics remain simple
* Good, because authorization is tightly coupled to emission — no window for stale authorization
* Bad, because a network call inside a DB transaction holds database locks for the duration of the PDP round-trip, degrading throughput and risking lock contention under PDP latency spikes
* Bad, because PDP unavailability directly blocks all usage emission, coupling `emit()` reliability to PDP availability
* Bad, because violates the quick injection principle — `emit()` must complete within a local DB insert budget

### Authorization deferred to gateway at delivery time (dispatcher → gateway)

Validate authorization when the outbox dispatcher delivers records to the collector gateway — a `4xx` response dead-letters the record.

* Good, because `emit()` remains fast with no external calls
* Good, because no API change to `emit()` is required
* Bad, because the source caller has already received a success response before authorization is evaluated — the original request context is gone and the failure cannot be surfaced meaningfully
* Bad, because dead-lettered records are discovered via monitoring lag, not immediate feedback — identifying an authorization misconfiguration is delayed and operationally expensive
* Bad, because the source service has no actionable signal at the point where it should: the handler that produces the usage record

### Pre-loaded static SDK policy (config-based metric allowlist, no PDP)

Encode the allowed metric names for each source service in SDK initialization configuration (static list or file), evaluated at `emit()` time without any runtime network calls.

* Good, because `emit()` has no runtime external dependency — evaluation is purely local
* Good, because startup is fast if config is local (no network call at init time)
* Bad, because authorization policy is not centrally managed — operators must keep per-service configs in sync with PDP policies manually
* Bad, because policy changes require redeploying source services to take effect — no dynamic policy enforcement
* Bad, because deviates from the platform-wide PDP pattern used across all other Cyber Ware modules

### Two-phase PDP: `with_*().authorize()` before transaction + in-memory constraint evaluation at `enqueue()`

Separate the PDP call (network, async, before the transaction) from the constraint check (in-memory, synchronous, inside the transaction). See Decision Outcome for full description.

* Good, because PDP call happens outside the DB transaction — no lock contention
* Good, because `enqueue()` is fast — only in-memory evaluation and outbox INSERT
* Good, because authorization failures surface immediately at request time
* Good, because reuses existing `authz-resolver-sdk` infrastructure and constraint types
* Bad, because requires one PDP call per request that may emit usage
* Bad, because the emission call site changes from a simple `emit(ctx, record)` to a two-step `factory.with_*().authorize(ctx, resource_id, resource_type)` + `usage_record_builder(...)?.build()? → enqueue(record)` API

## More Information

The PDP request sent by `.authorize(...)` uses `PolicyEnforcer::access_scope_with()` with `require_constraints(false)` — a permit/deny decision suffices; raw constraint evaluation was replaced by the allowed-metrics list fetched from the gateway via `get_module_config()`. The `tenant_id`, `resource_id`, and `resource_type` are included as resource properties in the PDP evaluation request. The `source_module` identifier (from ADR 0001) is baked into `UsageEmitterFactory` and attached to every PDP request, enabling source-scoped policies in the PDP.

Per-request PDP calls are chosen over request-level caching for simplicity and consistency with the platform pattern used in other modules (see `examples/modkit/users-info`). Caching can be introduced in a future ADR if PDP call volume proves to be a concern at scale.

### TOCTOU Window Analysis

A time-of-check/time-of-use (TOCTOU) window exists between `.authorize()` (PDP call) and `enqueue()` (constraint evaluation). If an authorization policy is revoked or tightened in this window, `enqueue()` will evaluate constraints from the now-stale `UsageEmitter` and may permit an emission that would be denied under the updated policy.

This window is accepted under the following rationale:

- The window duration is bounded by the request handler execution time — typically well under 500ms in the absence of pathological application code — which is too short for routine policy changes to be operationally targeted at individual requests
- PDP policy changes are administrative operations with propagation delays of their own; expecting sub-second policy revocation enforcement across all active request contexts is beyond the operational model of the platform PDP
- The fail-closed principle applies to the next request: any subsequent `.authorize()` call will observe the updated policy and deny immediately
- Reusing `UsageEmitter` across request boundaries is explicitly prohibited (see Confirmation); each request obtains a fresh emitter, bounding maximum staleness to one request lifetime

To minimize exposure, `UsageEmitter` MUST NOT be cached or reused across request handlers. Runtime enforcement is provided by the freshness check in `enqueue()`: emitters older than `authorization_max_age` (default 30 seconds, configurable) are rejected with `UsageEmitterError::AuthorizationExpired`. This bounds maximum staleness to `MAX_AUTH_AGE` regardless of call-site behavior, eliminating reliance on code review for TOCTOU window closure.

### Performance Budget

The `.authorize()` step introduces one synchronous PDP round-trip per request on the non-transaction path. To satisfy `cpt-cf-usage-collector-nfr-ingestion-latency` (p95 ≤ 200ms for the full emit path including the domain operation), the combined latency budget for the `.authorize()` step is bounded by the following heuristic:

- Local DB insert (`enqueue()`) plus domain operation: ~50ms typical
- PDP call budget: ≤ 100ms p95 to leave ≥ 50ms headroom against the 200ms threshold

The `.authorize()` step MUST use a network timeout of ≤ 150ms to ensure a slow or unresponsive PDP does not breach the ingestion latency NFR. This timeout MUST be configurable and MUST default to 150ms or lower. If the PDP does not respond within the timeout, `.authorize()` MUST return `UsageEmitterError::AuthorizationFailed` (fail-closed).

## Review Cadence

This decision is stable. Revisit if:

- PDP round-trip volume at scale justifies introducing request-level caching of `UsageEmitter` instances — this would require a new ADR defining cache TTL, invalidation, and stale-token risk
- The ingestion latency NFR tightens below 200ms such that the PDP latency budget must be re-evaluated
- A platform-wide change to the PDP infrastructure (e.g., local sidecar PDP) materially changes the round-trip cost, potentially eliminating the TOCTOU concern and the "Bad" PDP-unavailability consequence

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-usage-collector-fr-ingestion` — preserves quick ingestion by keeping the outbox INSERT on the critical path and moving the PDP call off it
* `cpt-cf-usage-collector-fr-tenant-attribution` — the `.authorize()` step passes the subject's tenant context to the PDP, ensuring tenant-aware authorization at the point of emission
* `cpt-cf-usage-collector-fr-tenant-isolation` — PDP constraints returned by `.authorize()` can enforce tenant-scoped metric restrictions, evaluated before any record enters the outbox
* `cpt-cf-usage-collector-principle-fail-closed` — denial from the PDP propagates as an `Err` from `.authorize()`; no record is enqueued on authorization failure
* `cpt-cf-usage-collector-principle-source-side-persistence` — the outbox INSERT in `enqueue()` remains within the caller's DB transaction; only in-memory evaluation is added inside the transaction boundary
* `cpt-cf-usage-collector-component-emitter` — the `.authorize()` step and the resulting `UsageEmitter` (layer 3) extend the emitter component's responsibility scope
* `cpt-cf-usage-collector-interface-scoped-emitter` — `UsageEmitterFactory` exposes the builder chain `factory.with_tenant(t)?.with_subject*(...)?...authorize(ctx, resource_id, resource_type) -> Result<UsageEmitter, UsageEmitterError>`; the returned `UsageEmitter`'s `usage_record_builder(metric, value)?.build()? → enqueue(record)` performs in-memory validation within the caller's transaction
* `cpt-cf-usage-collector-adr-scoped-emit-source` — this decision builds on ADR 0001: `UsageEmitterFactory` is the owner of the `.authorize()` step, using the `source_module` it already holds (bound at `runtime.factory(MODULE_NAME)`) for PDP resource property attribution
