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

The current consumer subscription API (`POST /v1/subscriptions`, per `schemas/gts.cf.core.events.subscription.v1~.schema.json`) carries `topics: [string]` and `filters: [string]` as **two parallel arrays**. Three problems with that surface:

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
* **Symmetric plugin pattern**: filter engines plug in via the same GTS-typed `types_registry` + `ClientHub` resolution as OAGW auth/guard/transform plugins.

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
      "subject_tenant":  "<uuid>",
      "ownership_filter": "entire_tenant",
      "types":           ["gts.cf.core.events.event.v1~yourorg.orders.placed.v1~"],
      "filter": {
        "engine":     "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
        "expression": "event.data.amount > 100"
      }
    },
    {
      // No filter expression — match every event of these types under this tenant.
      "topic":     "gts.cf.core.events.topic.v1~yourorg.refunds.v1",
      "subject_tenant": "<other-uuid>",
      "ownership_filter": "entire_tenant",
      "types":     ["gts.cf.core.events.event.v1~yourorg.refunds.*"]
    }
  ]
}
```

### Interest Body Shape

Two required fields and five optional ones, `subject_tenant`, `ownership_filter`, `barrier_mode`, `subject_types` and `filter`. No capability flags.

| Field | Cardinality | Description |
|---|---|---|
| `topic` | required | Full GTS topic identifier. Partition / rebalance / authz unit (Kafka-style). |
| `subject_tenant` | optional | UUID, the caller's own tenant when absent; echoed resolved. The tenant the interest reads. Not the nil UUID (`400 InvalidTenant`). Different interests MAY declare different tenants. |
| `ownership_filter` | optional | `only_mine` (the default): the events of the caller's own tenant whose `subject_owner` is the caller (an event with no owner never matches); a `subject_tenant` other than the caller's answers `400 InvalidTenant`. `entire_tenant`: every event of `subject_tenant`. `tenant_subtree_included`: the events of `subject_tenant` and the tenants below it that the consume scope admits. Echoed. Any other value answers `422`. `max_depth` is not accepted (`400 InvalidBody`), and a `tenant_id` field is an unknown field (`422`). |
| `barrier_mode` | optional | `respect` (the default) or `ignore`. An `ignore` interest needs the barrier right (see Authorization); no effect under `only_mine`. Any other value answers `422`. |
| `types` | required, non-empty (non-empty not yet enforced, tracked in #5978) | Array of GTS event-type-instance patterns. Scoped to `topic`. Wildcards per GTS §10. |
| `subject_types` | optional | Array of 1-32 GTS type patterns matched against the event's `subject_type`. A delivery filter, not an access grant: it never widens what the caller may read. Absent or `[]` means no selector. Echoed only when non-empty. |
| `filter` | optional | Consolidated engine-typed filter object with required `engine` (full GTS identifier extending `gts.cf.core.events.filter.v1~`, always full GTS — no short-discriminator shortcut) and `expression` (engine-specific source string). Absent means no filter. |

**Filter object rule**: `filter` is either present or absent. When present, both `engine` and `expression` are required: a `filter` object missing either fails the REST handler's body deserialization → `422`. An interest without `filter` delivers every event matching topic + ownership filter + types - no engine round-trip.

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
exact_pattern    = full GTS event-type identifier (ends with `~`, no `*`)
wildcard_pattern = gts_prefix ("." | "~") "*"
gts_prefix       = canonical GTS prefix ending at a segment boundary
                   (.vendor | .package | .namespace | .name | .v<major> | .v<major>.<minor> | ~<instance-segments>)
```

Valid examples (under topic `~yourorg.orders.v1`):

- `gts.cf.core.events.event.v1~yourorg.orders.placed.v1~` - `placed.v1` and every type derived from it (implicit derived-type coverage, see Acknowledged GTS Features).
- `gts.cf.core.events.event.v1~yourorg.orders.placed.v1~` matches every `placed.v1.x` (minor-version-omitted matching; see below).
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
3. **Per-name latest** rule: among matches, select the highest version per `(vendor.package.namespace.name)` tuple (not yet implemented, tracked in #5978).
4. **Minor-version-omitted matching** (GTS §10 rule 5, applied in v1): a pattern that pins only the major version (e.g., `placed.v1` without explicit minor) matches every registered minor of that major; per-name latest then picks the highest minor (e.g., `placed.v1.7` over `placed.v1.0`). A higher major (`placed.v2.0`) is **not** picked unless the pattern explicitly admits it.
5. **Type-belongs-to-topic** defense-in-depth: every resolved type's `parent_topic` MUST equal `interest.topic`; mismatch → `400 TypeNotInTopic` (not yet implemented, tracked in #5978).
6. **Unmatched entries**: a pattern that matches no registered type does not fail the JOIN. The `201` body lists it in that interest's `unmatched_types` (`[]` when every entry matched), so a typo is visible at once. On a topic with more than 256 registered event types only exact entries are checked, and a wildcard is never listed. The list belongs to the JOIN response: a read or a listing of the subscription does not carry it.
7. **Matching is live**: a type that registers after the JOIN and matches the pattern is delivered without a re-JOIN, once the caller's consume scope admits it.

### Filter-Engine Plugin Pattern

Not yet implemented; tracked in #5978.

Mirrors OAGW plugins:

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

- **Resolution at JOIN**: broker reads `interest.filter.engine`, resolves the engine via `ClientHub`, calls `engine.compile(interest.filter.expression, MAX_EXPRESSION_LENGTH_BYTES)`, stores the compiled handle in the subscription's cache entry alongside the resolved type set and tenant_id.
- **Per-event delivery**: broker iterates `(prerequisite_check, compiled_handle)` pairs; on first match, short-circuit.

### CEL Engine Filter Context (v1)

Not yet implemented; tracked in #5978.

The CEL engine binds one variable `event` whose fields are the read-side event (the same `gts.cf.core.events.event.v1~.schema.json` resource consumers receive — `meta` is stripped on read via its `writeOnly` marker; `partition`/`sequence`/`sequence_time` are server-stamped via `readOnly`):

| CEL identifier | Type | Source |
|---|---|---|
| `event.id` | string (UUID) | publish input |
| `event.type` | string (GTS) | publish input |
| `event.subject_tenant` | string (UUID) | publish input (authz-validated) |
| `event.source` | string | publish input |
| `event.subject` | string | publish input |
| `event.subject_type` | string (GTS) | publish input |
| `event.occurred_at` | timestamp | publish input |
| `event.partition` | int | backend-assigned |
| `event.sequence` | int (i64) | backend-assigned |
| `event.sequence_time` | timestamp | backend-assigned |
| `event.trace_parent` | string (optional) | publish input |
| `event.data` | map / dyn | publish input (per the event type's `data_schema`) |

`meta` is **never** in scope (producer-protocol; stripped on read projection per ADR-0003). `event.data` access uses CEL's standard `dyn` typing; missing-field access surfaces as `FilterError::EvalRuntime` and is treated as a non-match for the offending interest.

### Filter Limits (compile-time constants in v1)

All values are Rust `const`s. Runtime configurability is deferred — once workloads characterize the typical bounds, v2 lifts them to deployment config.

- `JoinRequest::MAX_INTERESTS: usize = 64`
- `JoinRequest::MAX_TYPES_PER_INTEREST: usize = 32`
- `JoinRequest::MAX_SUBJECT_TYPES_PER_INTEREST: usize = 32`
- `MAX_EXPRESSION_LENGTH_BYTES: usize = 4096` (not yet implemented, tracked in #5978)
- `MAX_COMPILED_FILTER_BYTES: usize = 65_536` (engine-reported) (not yet implemented, tracked in #5978)
- `EVAL_TIMEOUT_MICROS: u64 = 10_000` (10ms per `(event, interest)` evaluation) (not yet implemented, tracked in #5978)

Breaches → `400 TooManyInterests` / `400 TooManyTypes` / `400 TooManySubjectTypes` at JOIN; `400 ExpressionTooLong` / `400 CompiledFilterTooLarge` at JOIN (not yet implemented, tracked in #5978). Eval-timeout at delivery drops the offending event from this consumer's batch + warn log + `evbk_filter_eval_timeout_total{consumer_group}` metric (does NOT fail the stream) (not yet implemented, tracked in #5978).

### JOIN Validation Order

```text
POST /v1/subscriptions { consumer_group, client_agent, session_timeout, interests[] }
  ↓
1. Authn: SecurityContext present? else 401.
2. Per-tenant rate cap on JOIN (`cpt-cf-evbk-nfr-tenant-rate-caps`; default 60/min/tenant) else 429
   (not yet implemented, tracked in #5697).
3. Request parsing, in the REST handler: a body that is not valid JSON → 400; a missing,
   unknown or mistyped field → 422 (an unknown barrier_mode, and a `filter` object
   missing `engine` or `expression`, included); then client_agent, session_timeout and
   the consumer_group id → 400; a named consumer group → 501; then each interest's
   max_depth (refused when present), topic id, filter.engine id and the GTS syntax of
   every types[] and subject_types[] pattern → 400 (400 BadTypePattern for a pattern).
   In process, the SDK-to-domain conversion (`LocalBroker`) parses the consumer_group id
   and each interest (its topic id and patterns), resolving an absent subject_tenant and
   session_timeout; the domain then checks client_agent (400, reason ascii_only,
   field_too_long or user_agent_grammar) and session_timeout (400
   InvalidSessionTimeout), which the REST handler has already refused on that path.
4. Caller identity (domain): a subject or tenant id that is the nil UUID → 403
   TenantIdNotAuthorized. A bearer token does not exempt the caller.
5. Limits (domain): interests[] count → 400 TooManyInterests; then each interest in
   order: types[] count → 400 TooManyTypes, subject_types[] count → 400
   TooManySubjectTypes, a nil subject_tenant or an only_mine subject_tenant other than
   the caller's → 400 InvalidTenant.
   No read and no PDP call happen before this point.
6. consumer_group exists in event_broker_consumer_group? else 404 ConsumerGroupNotFound.
   Anonymous group: the caller is the principal that created it, in its tenant? else
   403 ConsumerGroupNotOwned (no PDP call). Then the group check: consume on the group?
   else 403 ConsumerGroupNotAuthorized (500 when its answer does not compile).
7. Every types[] pattern can match some event type (it covers, or is covered by, the
   event family) → else 400 BadTypePattern.
8. For each interest group - the interests sharing a topic and a tenant context (the
   caller's tenant, root only, barriers respected for only_mine; subject_tenant, root
   only for entire_tenant or with its subtree for tenant_subtree_included, in the
   interest's barrier_mode), in the order each group first appears:
   8a. The event types registered on the topic (the candidates) → 503 when unavailable.
   8b. barrier_mode ignore only: the barrier right (consume_ignore_barrier on the topic)
       → 403 TenantIdNotAuthorized when denied or when its answer does not compile.
   8c. The consume scope on the topic for the group's patterns and, when the topic has at
       most 256 registered event types, the candidates they match.
   8d. Only when 8c is refused (denied, not compilable, or nothing enforceable): the
       tenant check picks the code → 403 TopicNotAuthorized when it reaches the context
       (or its answer does not compile), 403 TenantIdNotAuthorized when it does not.
   8e. The entries of each interest's types[] that match no registered type, for
       unmatched_types.
   The first refused group ends the JOIN.
   Per interest (not yet implemented, tracked in #5978):
   - Defense-in-depth: every resolved type's parent_topic == interest.topic → else 400 TypeNotInTopic.
   - expression length cap (only if `filter.expression` supplied) → 400 ExpressionTooLong.
   - If `filter` present: resolve `filter.engine` via ClientHub → 400 UnknownFilterEngine on miss.
   - If `filter` present: engine.compile(`filter.expression`, MAX_EXPRESSION_LENGTH_BYTES) → 400 InvalidFilterExpression
     on err; 400 CompiledFilterTooLarge on size budget exceed.
9. Once every group is allowed, each exact selector is checked against its group's scope:
   an exact types[] entry whose registered types the scope admits none of → 403
   EventTypeNotAuthorized; an exact subject_types[] entry that overlaps a subject type a
   matched event type allows, and that no constraint of the scope admits → 403
   SubjectTypeNotAuthorized.
10. Each interest.topic exists in the specification cache (`event_broker_spec_cache`)? else 404 TopicNotFound.
11. Collect the topic set from interest.topic across all interests (no derivation; topics are explicit).
12. Write the routing markers (subscription, group) → 503 when the cluster cache is unavailable.
13. Add the member to its group (its interests, topic set and assignment) and run rebalance with its (topic, partition) interest set.
14. Return 201 Created with { id, consumer_group, client_agent, interests[] (echoed, each with unmatched_types), assigned, topology_version, created_at }.
```

All validation BEFORE persistence. Failed JOIN leaves no broker state.

### Per-Event Delivery Evaluation

```text
delivery service has event E for (topic, partition) assigned to subscription S
  ↓
For each interest I in S.compiled_interests:
  if E.topic       != I.topic              → skip this interest, continue
  if I's ownership narrowing does not hold → skip this interest, continue
    (only_mine: E.subject_tenant == caller's tenant and E.subject_owner == caller;
     entire_tenant: E.subject_tenant == I.subject_tenant; tenant_subtree_included: none)
  if E.type not in I.resolved_type_set     → skip this interest, continue
  if I.subject_types is set and E.subject_type matches none of them
                                           → skip this interest, continue
  if the member's consume scope does not admit E (tenant, owner, type, subject type)
                                           → skip this interest, continue
  if I.compiled is None:                    // no filter object supplied at JOIN
    → INCLUDE E in this subscription's batch; short-circuit (other interests not evaluated)
  else:                                     // not yet implemented, tracked in #5978
    match I.engine.eval(I.compiled, FilterContext::from(E)) {
      Ok(true)  → INCLUDE E; short-circuit
      Ok(false) → continue to next interest
      Err(EvalTimeout)        → log warn; bump metric; treat as Ok(false); continue
      Err(EvalRuntime { .. }) → log warn; bump metric; treat as Ok(false); continue
    }
If no interest matched → drop E from this subscription's batch.
```

Short-circuit on first match. Filter-eval failures (timeout, missing field) DO NOT propagate as broker errors - events are silently dropped from the offending consumer's batch and counted via metric (not yet implemented, tracked in #5978).

### Authorization

A caller whose subject or tenant id is the nil UUID is refused `403 TenantIdNotAuthorized` before any read or PDP call; a bearer token does not exempt it.

JOIN authorizes per interest group: the interests sharing a topic and a tenant context, which the ownership filter sets (`only_mine`: the caller's tenant, root only, barriers respected; `entire_tenant`: the interest's tenant, root only; `tenant_subtree_included`: the interest's tenant with its subtree; both in the interest's barrier mode). For each group it asks the PDP in that context:

- **I, the barrier right**, only for a `barrier_mode: ignore` group: `consume_ignore_barrier` on the topic. Denied, or an answer that does not compile → `403 TenantIdNotAuthorized`, and nothing more is asked for the group.
- **E, the consume scope**: `consume` on the topic, naming the group's type patterns and, when the topic has at most 256 registered event types, the ones they match and the subject types those allow. The answer is a scope that may constrain the tenant, the owner (`owner_id`), the event type and the subject type, and it decides delivery.
- **R, the tenant check**, only when E is refused (denied, not compilable, or leaving nothing the broker can enforce), to pick the refusal's code: R reaches the tenant (or its answer does not compile) → `403 TopicNotAuthorized`; R denies → `403 TenantIdNotAuthorized`.

The first refused group ends the JOIN. Once every group is allowed, each exact selector is checked against its scope: an exact `types` entry whose registered types the scope admits none of → `403 EventTypeNotAuthorized`, and an exact `subject_types` entry that overlaps a subject type a matched event type allows, and that no constraint of the scope admits → `403 SubjectTypeNotAuthorized`. Both name the entry in the message. Wildcards are not checked: the broker cannot tell what they will match. An exact `subject_types` entry that no allowed pattern covers and that covers none of them is not checked either, even when it partly overlaps one; skipping it never widens delivery, because the consume scope still gates each event (a known gap, tracked in #5976).

An interest group's barrier-right, consume-scope or tenant-check answer that does not compile is a refusal (`403`). The group check (JOIN, stream open and seek) and the tenant gate at stream open and seek answer `500` for one, as publish does. A failed evaluation (the PDP could not answer) is a `500` on JOIN, stream open, seek and publish.

`subject_types` has no authorization meaning: the PDP decides what the caller may read, and the selector only narrows delivery within that.

**Answers reused.** A JOIN always asks the PDP and keeps each allowed barrier right and each consume scope for `streaming.authz_scope_ttl_secs` (default 60), keyed by the exact request it answered, while the instance holds fewer than 1,024 live answers. A stream open reuses a kept answer within that time instead of asking again, and keeps what it had to ask; an open stream's renewal of its consume scope does the same, reusing only an answer that outlasts the renewal's lead time. A refusal is never kept. The key covers every field the request sends, the caller's token scopes included, so a caller whose scopes change is asked again. The bearer token is outside the key (#5502): a caller whose token alone changes reuses an answer until its time ends.

### Acknowledged GTS Features

- **Applied in v1**: minor-version-omitted matching (§10 rule 5). A pattern pinning only the major version matches every registered minor of that major; per-name-latest rule picks the highest minor.
- **Applied in v1**: implicit derived-type coverage (§3.6). A bare type identifier (no `*`) also matches every type derived from it, at any depth. The broker matches interest `types` and `subject_types`, and resolves JOIN candidates, with the GTS library's pattern matching (`GtsId::matches_pattern`) and adds no matching rule of its own. The consume scope then admits each delivered event type, and each subject type it constrains, by exact id, never as a pattern; a scope constraint without an event-type filter admits every type.

### Consequences

- Good, because the wire shape eliminates the parallel-array ambiguity by construction.
- Good, because topic is explicit (Kafka-style) — partition assignment, rebalance, and authz operate on the same unit the consumer declared.
- Good, because filter engines are extensible via the same GTS-typed plugin registry used for OAGW plugins.
- Good, because event-type filtering happens before engine eval — the broker skips non-matching events cheaply.
- Good, because the optional `filter` object lets the common case (no filter beyond topic+tenant+types) be the simplest wire.
- Good, because GTS-spec-compliant patterns mean every other system that handles GTS identifiers (types_registry, authz resolver, observability) understands the broker's patterns natively.
- Good, because per-name-latest + minor-version-omitted matching gives consumers a stable major-pinned subscription that gracefully picks up minor-version updates.
- Bad / accepted, because two-level addressing (topic + type-patterns) is more surface than topic-only. Mitigation: documented; the `types: ["gts.cf.core.events.event.v1~*"]` idiom is the "all types in this topic" pattern.
- Good, because matching is live: a type that registers after the JOIN reaches an active subscription without a re-JOIN, once the caller's consume scope admits it, and a typo in a pattern shows at once in `unmatched_types`.
- Bad / accepted, because the per-event eval timeout silently drops events from the offending consumer's batch rather than failing the stream (not yet implemented, tracked in #5978). Mitigation: alert-able metric.
- Bad / accepted, because all-or-nothing JOIN validation (one denied type or unauthorized tenant fails the whole JOIN). Mitigation: clearer authz model; no surprise partial subscriptions.

### Confirmation

- **GTS spec compliance test**: every example pattern in ADRs, DESIGN, scenarios, schemas, and fixtures is verified against the GTS spec's wildcard rules (single trailing `*`, segment-boundary, no `**`, no substring-within-segment).
- **JOIN wire-shape test**: schema codegen produces a strongly-typed `Interest` struct with `topic` and `types` required, optional `subject_tenant` and `ownership_filter`, and an optional `filter { engine, expression }` object.
- **JOIN validation test matrix**: each step in the JOIN validation order has a test asserting the documented error code on a triggering input.
- **Per-event delivery test**: short-circuit on first match; no-filter interest matches by prerequisites alone; eval timeout drops event silently + metric (not yet implemented, tracked in #5978).
- **Rolling-deploy test**: v1 + v2 members with different interests in the same group; partition handoff preserves the per-member filter on subsequent events; cursor preserves position.

## Pros and Cons of the Options

### Topic-Anchored Interests With Typed Filters (chosen)

* Good, because the interest is a single self-contained selection unit — no parallel-array ambiguity.
* Good, because topic is explicit on the wire (Kafka-style) — partition assignment, rebalance, and authz all key on the same identifier.
* Good, because filter engines are pluggable via the platform's standard GTS-typed registry pattern.
* Good, because per-name-latest + minor-version-omitted matching gives stable major-pinned subscriptions.
* Good, because the optional `filter` object keeps the no-filter common case simple.
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

- **Second built-in filter engine** (post-v1): starlark, rego, or jsonpath. The plugin pattern is the unblock; choosing the second engine is a separate decision.
- **Filter-engine cardinality**: today's `filter.v1~` base type allows N concurrent engines in a deployment. Limits are implementation-defined.

External references:

- `GlobalTypeSystem/gts-spec` README §10 "Collecting Identifiers with Wildcards", §3.5 wildcard intent, §3.6 implicit derived-type coverage, conformance tests at `tests/test_op4_id_match_pattern.py`. The wildcard rules in this ADR are taken directly from §10 rules 1–5 (verified, see Pattern Resolution above).
- RFC 9457 — Problem Details, used for error response shapes.
- Apache Kafka — topic-anchored subscription model, referenced as the prior art for "topic is the partition / rebalance / authz unit."

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-subscription-join` — JOIN body shape (this ADR pins the interests[] structure).
  - `cpt-cf-evbk-fr-filter-expression` — new FR for typed filter engines + plugin pattern.
  - `cpt-cf-evbk-nfr-tenant-rate-caps` - extended to cover `POST /v1/subscriptions` per-tenant rate cap (not yet implemented, tracked in #5697).
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §3.1 Subscription Schema — updated to reference the new interests[] body shape.
  - §3.2 SubscriptionResolutionCache — adds filter-engine handle caching.
  - §3.3 JOIN endpoint — references this ADR for the validation flow.
  - §3.6 Stream Consumption Flow - per-event filter evaluation step added.
  - §4.3 Metrics - `evbk_filter_eval_timeout_total`, `evbk_filter_eval_error_total` added (not yet implemented, tracked in #5978).
- **Scenario and test coverage**: JOIN shape, interest body, pattern syntax, version resolution, topic / tenant / type authz, filter-engine resolution, expression compilation, per-event delivery, limits, and rolling deploy are covered by repository scenarios and tests.
- **Related ADRs**:
  - [`0002-partition-selection`](0002-partition-selection.md) — partition derivation; the subscription rebalance still operates on `(topic, partition)`.
  - [`0003-event-schema`](0003-event-schema.md) — read-side event fields exposed to the CEL filter context (the `gts.cf.core.events.event.v1~.schema.json` resource with `readOnly` fields populated and `writeOnly` `meta` stripped).
  - [`0004-idempotent-producer-protocol`](0004-idempotent-producer-protocol.md) - `client_agent` field convention on resource-creating endpoints; JOIN carries its own `client_agent`, checked as on the other resource-creating endpoints.
- **Feature doc**: [`docs/features/0002-consumer-subscription-lifecycle.md`](../features/0002-consumer-subscription-lifecycle.md) — CDSL flows, ACs, test plans.
- **Schemas**:
  - [`schemas/gts.cf.core.events.subscription.v1~.schema.json`](../schemas/gts.cf.core.events.subscription.v1~.schema.json) — subscription resource (updated for interests[]).
