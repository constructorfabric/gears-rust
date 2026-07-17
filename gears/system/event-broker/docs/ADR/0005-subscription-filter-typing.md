---
status: proposed
date: 2026-05-14
decision-makers: Event Broker Team
---

# Subscription Filter Typing — Topic-Anchored Interests With GTS-Typed Filter Engines

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Interest Body Shape](#interest-body-shape)
  - [Type-Pattern Syntax (per GTS spec)](#type-pattern-syntax-per-gts-spec)
  - [Pattern Resolution and Version Selection](#pattern-resolution-and-version-selection)
  - [Filter-Engine Plugin Pattern](#filter-engine-plugin-pattern)
  - [CEL Engine Filter Context (v1)](#cel-engine-filter-context-v1)
  - [Filter Limits (compile-time constants in v1)](#filter-limits-compile-time-constants-in-v1)
  - [JOIN Validation Order](#join-validation-order)
  - [Per-Event Delivery Evaluation](#per-event-delivery-evaluation)
  - [Authorization](#authorization)
  - [Acknowledged GTS Features](#acknowledged-gts-features)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Topic-Anchored Interests With Typed Filters (chosen)](#topic-anchored-interests-with-typed-filters-chosen)
  - [Keep Parallel Topics and Filters Arrays (status quo)](#keep-parallel-topics-and-filters-arrays-status-quo)
  - [Event-Type-Centric Only (drop `topic` from interest)](#event-type-centric-only-drop-topic-from-interest)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-evbk-adr-subscription-filter-typing`

## Context and Problem Statement

The current consumer subscription API (`POST /v1/subscriptions`, per `schemas/subscription.v1.schema.json`) carries `topics: [string]` and `filters: [string]` as **two parallel arrays**. Three problems with that surface:

1. **Parallel arrays with no declared mapping.** The JSON Schema does not specify whether `filters[i]` applies to `topics[i]`, to all of `topics`, or to none. Prose in `features/0002-consumer-subscription-lifecycle.md` is ambiguous; the rolling-deploy scenario (v1 with `(topics=T1, filters=F1)`, v2 with `(T2, F2)`) becomes guess-work as soon as a member subscribes to more than one topic.
2. **Untyped filter expressions.** `filters[i]` is a bare string, implicitly CEL, with no declared expression language, no engine-extensibility story, no validation contract. Adding a second engine (starlark, rego, jsonpath) would break the wire.
3. **No event-type filtering on the wire.** Today the broker delivers every event on a subscribed topic and relies on the filter expression to select. For consumers that want a narrow type-set (e.g., "orders.placed.v1.x"), this forces them to encode the type check inside the CEL string, repeated for every interest. The broker can't skip non-matching types before invoking the engine.

The broker is unshipped. Re-iterating the consumer subscription surface now - before any caller has integrated - is the right time to make the wire shape explicit and stable.

## Decision Drivers

* **Unambiguous wire shape**: which filter applies to which topic must be self-evident from the JSON, not inferred.
* **Engine extensibility**: support CEL today, starlark / rego / vendor-custom tomorrow, without breaking the wire when a second engine ships.
* **Topic-anchored model** (Kafka-style): topics are the partition / rebalance / authz unit; explicit on the wire.
* **Two-level type filtering**: events the consumer doesn't pattern-match on `types[]` should be skipped before invoking the engine (cheap), with the engine reserved for richer predicates over `event.data` etc.
* **GTS spec compliance**: type patterns follow `GlobalTypeSystem/gts-spec` §10 wildcard rules exactly — no broker-specific extensions.
* **Rolling-deploy unambiguity**: different members of the same consumer group can declare different interest sets without positional-correspondence games.
* **Symmetric plugin pattern**: filter engines plug in via the same GTS-typed `types_registry` + `ClientHub` resolution as storage backends and OAGW auth/guard/transform plugins.

## Considered Options

* Topic-anchored `interests[]` with typed filter engines and GTS-spec-compliant patterns (chosen).
* Keep parallel `topics[]` + `filters[]` arrays; add a per-filter `type` field as a back-compat extension (rejected — preserves the mapping ambiguity).
* Event-type-centric only: drop `topic` from the interest, derive it from `types_registry` via the resolved event type → parent topic (rejected — silent cross-topic-wildcard hazard; topic-level authz becomes per-type-only; topology resolution couples to the registry on every JOIN).

## Decision Outcome

Adopt **topic-anchored `interests[]` with GTS-typed filter engines**. The JOIN body becomes:

```jsonc
{
  "consumer_group":  "gts.cf.core.events.consumer_group.v1~<id>",
  "client_agent":    "<rfc-9110-user-agent>",
  "session_timeout": "PT30S",
  "interests": [
    {
      "topic":           "gts.cf.core.events.topic.v1~yourorg.orders.v1",
      "tenant_id":       "<uuid>",
      "types":           ["gts.cf.core.events.event.v1~yourorg.orders.placed.v1"],
      "expression_type": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
      "expression":      "event.data.amount > 100"
    },
    {
      // No filter expression — match every event of these types under this tenant.
      "topic":     "gts.cf.core.events.topic.v1~yourorg.refunds.v1",
      "tenant_id": "<other-uuid>",
      "types":     ["gts.cf.core.events.event.v1~yourorg.refunds.*"]
    }
  ]
}
```

### Interest Body Shape

Three required fields + two paired-optional fields. No capability flags.

| Field | Cardinality | Description |
|---|---|---|
| `topic` | required | Full GTS topic identifier. Partition / rebalance / authz unit (Kafka-style). |
| `tenant_id` | required | UUID. Tenant scope. Authz-validated by the platform tenant resolver. Different interests MAY declare different tenant_ids. |
| `types` | required, non-empty | Array of GTS event-type-instance patterns. Scoped to `topic`. Wildcards per GTS §10. |
| `expression_type` | optional (paired with `expression`) | Full GTS identifier extending `gts.cf.core.events.filter.v1~`. Always full GTS — no short-discriminator shortcut. |
| `expression` | optional (paired with `expression_type`) | Engine-specific source string. |

**Paired-optional rule**: `expression_type` and `expression` are both present (engine-typed filter) or both absent (no filter beyond topic + tenant + types). Exactly one present → `400 BadRequest`. An interest without the paired filter fields delivers every event matching topic + tenant + types — no engine round-trip.

**Multiple interests OR together**: an event matches a subscription if at least one interest matches. Consumers wanting AND-of-two-predicates encode both in the single `expression`.

### Type-Pattern Syntax (per GTS spec)

Per `GlobalTypeSystem/gts-spec` README §10 "Collecting Identifiers with Wildcards" (verified rules 1–5 + conformance tests):

- The wildcard `*` MAY be used **at most once**.
- The wildcard MUST appear at the **end** of the pattern.
- The wildcard MUST start at a **segment boundary** (`.` or `~`).
- Per rule 4, the wildcard MAY follow the `v` that begins the version segment (`placed.v*` is valid; the `v` is the segment start, `*` is trailing).
- Mid-pattern wildcards, substring wildcards within a segment (`vendor*`), and multi-segment wildcards (`**`) are rejected.

```
type_pattern     = exact_pattern | wildcard_pattern
exact_pattern    = full GTS event-type instance identifier
wildcard_pattern = gts_prefix ("." | "~") "*"
gts_prefix       = canonical GTS prefix ending at a segment boundary
                   (.vendor | .package | .namespace | .name | .v<major> | .v<major>.<minor> | ~<instance-segments>)
```

Valid examples (under topic `~yourorg.orders.v1`):

- `gts.cf.core.events.event.v1~yourorg.orders.placed.v1` — exact match.
- `gts.cf.core.events.event.v1~yourorg.orders.placed.v1` matches every `placed.v1.x` (minor-version-omitted matching; see below).
- `gts.cf.core.events.event.v1~yourorg.orders.placed.v*` — all versions of `placed` (rule 4).
- `gts.cf.core.events.event.v1~yourorg.orders.placed.*` — broader: all continuations of `placed.`.
- `gts.cf.core.events.event.v1~yourorg.orders.*` — all event types under `yourorg.orders` namespace.
- `gts.cf.core.events.event.v1~*` — all event types of this schema (broker's `parent_topic` filter restricts to `interest.topic`).

Rejected examples → `400 BadTypePattern`:

- `~yourorg.orders.*.v1` — mid-pattern wildcard.
- `~yourorg.**` — multi-segment wildcard.
- `~vendor*` — substring wildcard within a segment.
- `~yourorg.orders.*~*` — two wildcards.

### Pattern Resolution and Version Selection

At JOIN, the broker:

1. Filters `types_registry` entries to those with `parent_topic == interest.topic`.
2. Applies each pattern in `interest.types[]` against that filtered set.
3. **Per-name latest** rule: among matches, select the highest version per `(vendor.package.namespace.name)` tuple.
4. **Minor-version-omitted matching** (GTS §10 rule 5, applied in v1): a pattern that pins only the major version (e.g., `placed.v1` without explicit minor) matches every registered minor of that major; per-name latest then picks the highest minor (e.g., `placed.v1.7` over `placed.v1.0`). A higher major (`placed.v2.0`) is **not** picked unless the pattern explicitly admits it.
5. **Type-belongs-to-topic** defense-in-depth: every resolved type's `parent_topic` MUST equal `interest.topic`; mismatch → `400 TypeNotInTopic`.
6. **Empty match**: a pattern resolving to zero registered types → `400 NoTypesMatched`. All-or-nothing — partial JOINs over the subset that did match are NOT permitted.
7. **Pinned at JOIN**: newly-registered types matching the pattern after JOIN do NOT auto-subscribe. Consumer re-JOINs to refresh.

### Filter-Engine Plugin Pattern

Mirrors storage backends and OAGW plugins:

- **Base type**: `gts.cf.core.events.filter.v1~` (registered via `types_registry`).
- **Built-in v1 engine**: `gts.cf.core.events.filter.v1~cf.core.expression.cel.v1`.
- **Plugin trait** (in `cf-gears-event-broker-sdk`):

```rust
#[async_trait]
pub trait FilterEngine: Send + Sync {
    /// Compile an expression. Called at JOIN per interest. Stateless beyond the returned CompiledFilter.
    fn compile(
        &self,
        expression: &str,
        max_length_bytes: usize,
    ) -> Result<CompiledFilter, FilterError>;

    /// Evaluate a compiled filter against an event context. Per (event, interest) at delivery.
    /// MUST complete within EVAL_TIMEOUT_MICROS.
    fn eval(
        &self,
        compiled: &CompiledFilter,
        ctx: &FilterContext,
    ) -> Result<bool, FilterError>;
}

pub struct CompiledFilter {
    inner: Box<dyn Any + Send + Sync>,   // engine-specific compiled form; opaque to broker
    compiled_size_bytes: usize,           // engine-reported, for accounting
}

pub enum FilterError {
    CompileFailed { diagnostic: String },
    ExpressionTooLong,
    EvalTimeout,
    EvalRuntime { diagnostic: String },
}
```

- **Resolution at JOIN**: broker reads `interest.expression_type`, resolves the engine via `ClientHub`, calls `engine.compile(interest.expression, MAX_EXPRESSION_LENGTH_BYTES)`, stores the compiled handle in the subscription's cache entry alongside the resolved type set and tenant_id.
- **Per-event delivery**: broker iterates `(prerequisite_check, compiled_handle)` pairs; on first match, short-circuit.

### CEL Engine Filter Context (v1)

The CEL engine binds one variable `event` whose fields are the read-side event (the same `event.v1.schema.json` resource consumers receive — `meta` is stripped on read via its `writeOnly` marker; `partition`/`sequence`/`sequence_time` are server-stamped via `readOnly`):

| CEL identifier | Type | Source |
|---|---|---|
| `event.id` | string (UUID) | publish input |
| `event.type` | string (GTS) | publish input |
| `event.topic` | string (GTS) | publish input |
| `event.tenant_id` | string (UUID) | publish input (authz-validated) |
| `event.source` | string | publish input |
| `event.subject` | string | publish input |
| `event.subject_type` | string (GTS) | publish input |
| `event.occurred_at` | timestamp | publish input |
| `event.partition_key` | string (optional) | publish input |
| `event.partition` | int | broker-derived |
| `event.sequence` | int (i64) | backend-assigned |
| `event.sequence_time` | timestamp | backend-assigned |
| `event.trace_parent` | string (optional) | publish input |
| `event.data` | map / dyn | publish input (per the event type's `data_schema`) |

`meta` is **never** in scope (producer-protocol; stripped on read projection per ADR-0003). `event.data` access uses CEL's standard `dyn` typing; missing-field access surfaces as `FilterError::EvalRuntime` and is treated as a non-match for the offending interest.

### Filter Limits (compile-time constants in v1)

All values are Rust `const`s. Runtime configurability is deferred — once workloads characterize the typical bounds, v2 lifts them to deployment config.

- `MAX_INTERESTS_PER_SUBSCRIPTION: usize = 64`
- `MAX_TYPES_PER_INTEREST: usize = 32`
- `MAX_EXPRESSION_LENGTH_BYTES: usize = 4096`
- `MAX_COMPILED_FILTER_BYTES: usize = 65_536` (engine-reported)
- `EVAL_TIMEOUT_MICROS: u64 = 10_000` (10ms per `(event, interest)` evaluation)

Breaches → `400 TooManyInterests` / `400 TooManyTypes` / `400 ExpressionTooLong` / `400 CompiledFilterTooLarge` at JOIN. Eval-timeout at delivery drops the offending event from this consumer's batch + warn log + `evbk_filter_eval_timeout_total{consumer_group}` metric (does NOT fail the poll).

### JOIN Validation Order

```text
POST /v1/subscriptions { consumer_group, client_agent, session_timeout, interests[] }
  ↓
1. Authn: SecurityContext present? else 401.
2. Per-tenant rate cap on JOIN (`cpt-cf-evbk-nfr-tenant-rate-caps`; default 60/min/tenant) else 429.
3. consumer_group exists in evbk_consumer_group? else 404 ConsumerGroupNotFound.
4. interests[] length cap check → 400 TooManyInterests.
5. For each interest in interests[]:
   5a. topic exists in evbk_topic? else 404 TopicNotFound.
   5b. tenant_id authz via platform tenant resolver → 403 TenantIdNotAuthorized.
   5c. topic-level consume authz → 403 TopicNotAuthorized.
   5d. types[] length cap → 400 TooManyTypes.
   5e. expression length cap (only if expression supplied) → 400 ExpressionTooLong.
   5f. For each pattern in types[]:
       - Pattern syntax valid per GTS spec → else 400 BadTypePattern.
       - Resolve via types_registry filtered to parent_topic == interest.topic:
         - 0 matches → 400 NoTypesMatched (whole JOIN fails).
         - Otherwise expand to concrete type GTS instances; apply per-name-latest + minor-version-omitted.
       - Defense-in-depth: every resolved type's parent_topic == interest.topic → else 400 TypeNotInTopic.
       - Per-resolved-type consume authz → 403 EventTypeNotAuthorized on any miss.
   5g. Paired-optional check: exactly-one of (expression_type, expression) → 400 BadRequest.
       If both absent, skip 5h–5i; this interest has no compiled filter.
   5h. If both present: resolve expression_type via ClientHub → 400 UnknownFilterEngine on miss.
   5i. If both present: engine.compile(expression, MAX_EXPRESSION_LENGTH) → 400 InvalidFilterExpression
       on err; 400 CompiledFilterTooLarge on size budget exceed.
6. Collect the topic set from interest.topic across all interests (no derivation; topics are explicit).
7. Run rebalance with this member's (topic, partition) interest set.
8. Persist subscription cache entry: { compiled_filters_or_None[], resolved_type_sets[], topic_set, assignments }.
9. Return 201 Created with { id, assigned, topology_version, expires_at, interests[] (echoed) }.
```

All validation BEFORE persistence. Failed JOIN leaves no broker state.

### Per-Event Delivery Evaluation

```text
delivery service has event E for (topic, partition) assigned to subscription S
  ↓
For each interest I in S.compiled_interests:
  if E.topic       != I.topic              → skip this interest, continue
  if E.tenant_id   != I.tenant_id          → skip this interest, continue
  if E.type not in I.resolved_type_set     → skip this interest, continue
  if I.compiled is None:                    // no paired-optional fields supplied at JOIN
    → INCLUDE E in this subscription's batch; short-circuit (other interests not evaluated)
  else:
    match I.engine.eval(I.compiled, FilterContext::from(E)) {
      Ok(true)  → INCLUDE E; short-circuit
      Ok(false) → continue to next interest
      Err(EvalTimeout)        → log warn; bump metric; treat as Ok(false); continue
      Err(EvalRuntime { .. }) → log warn; bump metric; treat as Ok(false); continue
    }
If no interest matched → drop E from this subscription's batch.
```

Short-circuit on first match. Filter-eval failures (timeout, missing field) DO NOT propagate as broker errors — events are silently dropped from the offending consumer's batch and counted via metric.

### Authorization

Two layers, both all-or-nothing:

- **Topic-level** (common grant shape): platform authz resolver checks `consume` against `interest.topic`. Denial → `403 TopicNotAuthorized`.
- **Per-resolved-type** (fine-grained, optional in deployments that grant per-type): resolver additionally checks each resolved event type. Denial → `403 EventTypeNotAuthorized` with the offending type in the response body.
- **Per-interest tenant** (platform resolver): `interest.tenant_id` authorized for the calling principal. Denial → `403 TenantIdNotAuthorized`.

### Acknowledged GTS Features

- **Applied in v1**: minor-version-omitted matching (§10 rule 5). A pattern pinning only the major version matches every registered minor of that major; per-name-latest rule picks the highest minor.
- **Not applied in v1**: implicit derived-type coverage (§3.6 — bare `~` type identifier auto-matched to derived-type instances). Broker requires explicit patterns or full identifiers; future versions MAY opt in as an additive change.

### Consequences

- Good, because the wire shape eliminates the parallel-array ambiguity by construction.
- Good, because topic is explicit (Kafka-style) — partition assignment, rebalance, and authz operate on the same unit the consumer declared.
- Good, because filter engines are extensible via the same GTS-typed plugin registry used for storage backends + OAGW plugins.
- Good, because event-type filtering happens before engine eval — the broker skips non-matching events cheaply.
- Good, because paired-optional `expression_type` + `expression` lets the common case (no filter beyond topic+tenant+types) be the simplest wire.
- Good, because GTS-spec-compliant patterns mean every other system that handles GTS identifiers (types_registry, authz resolver, observability) understands the broker's patterns natively.
- Good, because per-name-latest + minor-version-omitted matching gives consumers a stable major-pinned subscription that gracefully picks up minor-version updates.
- Bad / accepted, because two-level addressing (topic + type-patterns) is more surface than topic-only. Mitigation: documented; the `types: ["gts.cf.core.events.event.v1~*"]` idiom is the "all types in this topic" pattern.
- Bad / accepted, because version-resolution pinned at JOIN means new versions don't auto-flow to active subscriptions. Mitigation: future `dynamic_types_refresh` flag; out of v1 scope.
- Bad / accepted, because the per-event eval timeout silently drops events from the offending consumer's batch rather than failing the poll. Mitigation: alert-able metric.
- Bad / accepted, because all-or-nothing JOIN validation (one denied type or unauthorized tenant fails the whole JOIN). Mitigation: clearer authz model; no surprise partial subscriptions.

### Confirmation

- **GTS spec compliance test**: every example pattern in ADRs, DESIGN, scenarios, schemas, and fixtures is verified against the GTS spec's wildcard rules (single trailing `*`, segment-boundary, no `**`, no substring-within-segment).
- **JOIN wire-shape test**: schema codegen produces a strongly-typed `Interest` struct with `topic`, `tenant_id`, `types` required and `expression_type` + `expression` as a paired Option group.
- **JOIN validation test matrix**: each step in the JOIN validation order has a test asserting the documented error code on a triggering input.
- **Per-event delivery test**: short-circuit on first match; no-filter interest matches by prerequisites alone; eval timeout drops event silently + metric.
- **Rolling-deploy test**: v1 + v2 members with different interests in the same group; partition handoff preserves the per-member filter on subsequent events; cursor preserves position.

## Pros and Cons of the Options

### Topic-Anchored Interests With Typed Filters (chosen)

* Good, because the interest is a single self-contained selection unit — no parallel-array ambiguity.
* Good, because topic is explicit on the wire (Kafka-style) — partition assignment, rebalance, and authz all key on the same identifier.
* Good, because filter engines are pluggable via the platform's standard GTS-typed registry pattern.
* Good, because per-name-latest + minor-version-omitted matching gives stable major-pinned subscriptions.
* Good, because paired-optional `expression_type` + `expression` keeps the no-filter common case simple.
* Bad, because the wire is slightly larger than the parallel-array shape (5 fields vs. 2 arrays).
* Bad, because two-level addressing (topic + types) is more surface than single-level routing — but it's the right surface for both broker-side filtering optimization AND consumer-side declarativeness.

### Keep Parallel Topics and Filters Arrays (status quo)

**Description**: Retain the current shape. Optionally add a per-filter `type` field as a back-compat extension.

* Good, because zero migration cost (the broker is unshipped, so this argument is weak).
* Bad / decisive against, because the parallel-array mapping ambiguity is unresolvable without inventing a positional or naming correspondence rule.
* Bad, because adding a typed-filter field via back-compat extension preserves the underlying topology problem (topics are still topic-flat; types still implicit).
* Bad, because rolling-deploy scenarios become guess-work as soon as a member subscribes to more than one topic.

### Event-Type-Centric Only (drop `topic` from interest)

**Description**: Each interest declares `types[]` only; broker derives `parent_topic` from `types_registry` and computes the topic set from there.

* Good, because the wire is one field smaller per interest.
* Bad, because wildcard `~yourorg.orders.*` can accidentally span topics if types under that namespace happen to belong to multiple topics — silent cross-topic subscription.
* Bad, because the "all events on this topic" use case becomes fragile (no clean way to say "everything in this topic" without enumerating types).
* Bad, because broker has to consult `types_registry` for topic derivation on every JOIN — extra coupling.
* Bad, because topic-level authz (the common grant shape) becomes a derived check via per-resolved-type — operators who currently grant per-topic see the model leak.
* Captured as the design-time alternative; rejected on the cross-topic-wildcard risk alone.

## More Information

- **Dynamic type-pattern refresh** (post-v1): a `dynamic_types_refresh: true` flag on interests would cause the broker to re-resolve patterns on each topology change and silently extend the resolved type set. Deferred — pinned-at-JOIN is simpler and meets the typical use case.
- **Second built-in filter engine** (post-v1): starlark, rego, or jsonpath. The plugin pattern is the unblock; choosing the second engine is a separate decision.
- **Implicit derived-type coverage** (GTS §3.6, post-v1): treat a bare `~` type identifier as auto-matching its derived-type instances. Adds an additive matching path; out of v1 scope to keep the resolution rule simple.
- **Filter-engine cardinality**: today's `filter.v1~` base type allows N concurrent engines in a deployment. Limits are implementation-defined.

External references:

- `GlobalTypeSystem/gts-spec` README §10 "Collecting Identifiers with Wildcards", §3.5 wildcard intent, §3.6 implicit derived-type coverage, conformance tests at `tests/test_op4_id_match_pattern.py`. The wildcard rules in this ADR are taken directly from §10 rules 1–5 (verified, see Pattern Resolution above).
- RFC 9457 — Problem Details, used for error response shapes.
- Apache Kafka — topic-anchored subscription model, referenced as the prior art for "topic is the partition / rebalance / authz unit."

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-subscription-join` — JOIN body shape (this ADR pins the interests[] structure).
  - `cpt-cf-evbk-fr-filter-expression` — new FR for typed filter engines + plugin pattern.
  - `cpt-cf-evbk-nfr-tenant-rate-caps` — extended to cover `POST /v1/subscriptions` per-tenant rate cap.
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §3.1 Subscription Schema — updated to reference the new interests[] body shape.
  - §3.2 SubscriptionResolutionCache — adds filter-engine handle caching.
  - §3.3 JOIN endpoint — references this ADR for the validation flow.
  - §3.5 Long-Poll Consumption Flow — per-event filter evaluation step added.
  - §4.3 Metrics — `evbk_filter_eval_timeout_total`, `evbk_filter_eval_error_total` added.
- **Scenario and test coverage**: JOIN shape, interest body, pattern syntax, version resolution, topic / tenant / type authz, filter-engine resolution, expression compilation, per-event delivery, limits, and rolling deploy are covered by repository scenarios and tests.
- **Related ADRs**:
  - [`0002-partition-selection`](0002-partition-selection.md) — partition derivation; the subscription rebalance still operates on `(topic, partition)`.
  - [`0002-event-schema`](0002-event-schema.md) — read-side event fields exposed to the CEL filter context (the `event.v1.schema.json` resource with `readOnly` fields populated and `writeOnly` `meta` stripped).
  - [`0003-idempotent-producer-protocol`](0003-idempotent-producer-protocol.md) — `client_agent` field convention on resource-creating endpoints; JOIN reuses the consumer-group's `client_agent` rather than re-declaring per subscription.
- **Feature doc**: [`docs/features/0002-consumer-subscription-lifecycle.md`](../features/0002-consumer-subscription-lifecycle.md) — CDSL flows, ACs, test plans.
- **Schemas**:
  - [`schemas/subscription.v1.schema.json`](../schemas/subscription.v1.schema.json) — subscription resource (updated for interests[]).
