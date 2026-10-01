# Feature: Backend Registry, Routing and Affinity

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Configuration](#2-configuration)
  - [2.1 Backend Registry](#21-backend-registry)
  - [2.2 Backend Routing](#22-backend-routing)
  - [2.3 Affinity](#23-affinity)
  - [2.4 Startup Consistency Check](#24-startup-consistency-check)
  - [2.5 Outbox Groups](#25-outbox-groups)
- [3. Dispatcher Routing](#3-dispatcher-routing)
  - [3.1 Published Label](#31-published-label)
  - [3.2 Routing Table](#32-routing-table)
  - [3.3 Choosing an Instance](#33-choosing-an-instance)
  - [3.4 Best Effort](#34-best-effort)
  - [3.5 Pre-Parsing](#35-pre-parsing)
- [4. Out of Scope](#4-out-of-scope)
- [5. Definitions of Done](#5-definitions-of-done)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
- [8. E2E Test Plan](#8-e2e-test-plan)


## 1. Feature Context

### 1.1 Overview

A deployment names every storage backend instance it runs in a **backend registry**, and maps topics onto those names in a separate **backend routing** section. Both are event-broker configuration, checked for consistency when the process starts.

Every ingest and delivery instance is configured alike and handles any request. The shared configuration also names **affinity groups** - sets of topic patterns - and an instance may select one. It publishes its group in `DirectoryService`; a dispatcher reads every instance's group and prefers the most specific match, falling back to any live instance.

Routing says where a topic's log is expected to be. Putting it there the first time is feature 0006; proving later that it is really there is ADR-0008.

### 1.2 Purpose

- Let one deployment run several backends and several instances of one backend type, and put each topic on the one its operator chose.
- Keep backend addresses and credentials in one place, apart from the decision of which topic lives where.
- Let a loud topic get dedicated instances by config rollout alone, without any instance becoming unable to serve it.

### 1.3 Actors

- **Operator**: writes the registry, the routing, the affinity groups and each instance's group.
- **Ingest instance**: validates configuration, builds the registry, resolves routing, handles any topic.
- **Delivery instance**: validates configuration, builds the registry, resolves routing, handles any topic and owns any group.
- **Dispatcher**: reads the affinity groups, fetches every instance's published group and routes by preference.
- **Storage backend plugin**: builds a backend from an entry's settings, rejecting settings it does not define.

### 1.4 References

- DESIGN.md §"Storage Backend Plugin System"
- DESIGN.md §3.1 "Topic" - resolved settings, and ingest as the one writer of the resolved record
- DESIGN.md §"Pattern Syntax" - the topic pattern syntax
- Feature 0007 - storage backend API: `backend::Backend`, `backend::Provider`
- Feature 0006 - topic provisioning
- ADR-0008 - topic log identity
- `libs/system-sdks/sdks/directory/src/api.rs` - `DirectoryClient`, `ServiceInstanceInfo.labels`
- `libs/system-sdks/sdks/directory/src/labels.rs` - label limits
- `event-broker/src/infra/dispatcher/` - route classification and forwarding

## 2. Configuration

```yaml
modules:
  event_broker:
    # Every backend instance this deployment runs. Read by ingest and delivery.
    backends:
      main:
        type: "gts.cf.core.events.backend.v1~vendor.events.backend.example.v1~"
        endpoint: "${EB_MAIN_ENDPOINT}"
      audit:
        type: "gts.cf.core.events.backend.v1~vendor.events.backend.example.v1~"
        endpoint: "${EB_AUDIT_ENDPOINT}"

    # Which registry entry holds a topic's log. Read by ingest and delivery.
    backend_routing:
      "gts.cf.core.events.topic.v1~": main
      "gts.cf.core.events.topic.v1~vendor.audit.v1": audit

    # Named sets of topic patterns. Read by ingest, delivery and the dispatcher.
    affinity_groups:
      audit: ["gts.cf.core.events.topic.v1~vendor.audit.v1"]

    # The group the dispatcher prefers this instance for. Per instance; a hint, not a boundary.
    affinity: audit

    # Named ingest outboxes and the topics each carries. Read by ingest.
    outbox_groups:
      audit:   { partitions: 4, topics: ["gts.cf.core.events.topic.v1~vendor.audit.v1"] }
      default: { partitions: 16 }
```

`backends`, `backend_routing`, `affinity_groups` and `outbox_groups` describe the deployment: every ingest and delivery instance, and every dispatcher, carries the same four. `affinity` is the only setting that may differ between instances, and one file can be deployed everywhere with `affinity` set per instance (for example from the environment).

These sections replace the per-topic `backend` block and `default_storage_backend`; neither exists, and a configuration carrying either is refused at startup. A cluster deployment, for example, runs one file in every process and varies only `mode` and `affinity`:

```yaml
config:
  mode: ${EB_MODE}                       # cluster_ingest | cluster_delivery | cluster_dispatcher
  affinity: ${EB_AFFINITY:-default}
  backends:
    main:  { type: "gts.cf.core.events.backend.v1~vendor.events.backend.example.v1~", endpoint: "${EB_MAIN_ENDPOINT}" }
    audit: { type: "gts.cf.core.events.backend.v1~vendor.events.backend.example.v1~", endpoint: "${EB_AUDIT_ENDPOINT}" }
  backend_routing:
    "gts.cf.core.events.topic.v1~": main
    "gts.cf.core.events.topic.v1~vendor.audit.v1": audit
  affinity_groups:
    audit: ["gts.cf.core.events.topic.v1~vendor.audit.v1"]
  outbox_groups:
    audit:   { partitions: 4, topics: ["gts.cf.core.events.topic.v1~vendor.audit.v1"] }
    default: { partitions: 16 }
```

| Deployment | `mode` | `affinity` | The dispatcher prefers it for |
|---|---|---|---|
| ingest-default | `cluster_ingest` | `default` | publishes to every topic but audit |
| ingest-audit | `cluster_ingest` | `audit` | publishes to `vendor.audit.v1` |
| delivery-default | `cluster_delivery` | `default` | groups on other topics, and wide groups |
| delivery-audit | `cluster_delivery` | `audit` | groups joining only `vendor.audit.v1` |

`backend_routing` decides where a topic's log lives, the same in every process; affinity decides which process the dispatcher prefers, each instance publishing its group under its own role; outbox groups decide which ingest outbox a topic drains through. The shared name `audit` is a convention, not a requirement.

### 2.1 Backend Registry

- An entry's key is its **registry name**: `[a-zA-Z0-9_-]{1,255}`, the rule cluster profile names follow.
- An entry is its `type` - a GTS backend type derived from `gts.cf.core.events.backend.v1~` - plus that backend's own settings, written beside it. The gear passes the settings through; the plugin's own settings type rejects a key it does not define.
- Credentials are written as `${VAR}` references and expanded from the environment when the backend is built, the way toolkit-db and the cluster plugins read theirs. They never leave the process: nothing the broker persists carries an address or a setting.
- Each entry is built once per process. Every topic routed to it shares the one backend and its connection pool. Building validates settings and connects lazily (feature 0007 §2.7).
- A dispatcher holds no registry.

### 2.2 Backend Routing

- Keys follow the `topics` section's scheme: an entry keyed by a topic's own identifier wins over the entry keyed by the instance-less key for its type.
- The instance-less key `gts.cf.core.events.topic.v1~` is mandatory, so every topic resolves to a registry entry.
- Values are registry names, never addresses or types.
- A fully qualified key that names no registered topic is warned about on every specification refresh, naming the key: a mistyped key would otherwise leave its topic on the instance-less route for good.
- Routing says where a topic's log is expected to be. Whether it is really there is ADR-0008's to decide.

### 2.3 Affinity

```yaml
affinity_groups:
  audit:   ["gts.cf.core.events.topic.v1~vendor.audit.v1"]
  billing: ["gts.cf.core.events.topic.v1~vendor.billing.*"]
affinity: audit        # absent means the group `default`, which is ["*"]
```

- A group's name is 1-63 characters of `[a-zA-Z0-9_-]`, starting and ending with a letter or digit: it travels as a `DirectoryService` label value (§3.1) and names a toolkit-db outbox queue (§2.5), and both reject other forms. `default` is reserved and always means `["*"]`.
- A group's patterns use the syntax of DESIGN §"Pattern Syntax": `*` alone, `<prefix>.*`, or an exact topic identifier.
- Affinity is a routing preference and nothing else. The instance publishes its group (§3.1) and the dispatcher prefers it for matching topics (§3.3); the instance itself never looks at it.
- Every instance handles every request, whatever its group: it accepts publishes for, provisions, verifies, reads and streams any topic, and owns any consumer group. Where a request lands decides isolation, never correctness.
- A deployment that wants no dedicated instances defines no groups and sets no `affinity`. A typical dedicated layout is two default instances and two in group `audit`, so that either pair can lose an instance and the dedicated topic still has a preferred home.

### 2.4 Startup Consistency Check

Configuration-level checks. Any failure stops the process from starting.

| # | Role | Rule |
|---|---|---|
| C1 | ingest, delivery | Every registry entry's `type` is served by a backend provider linked into the build. |
| C2 | ingest, delivery | Every registry entry's settings are accepted by its provider. Reachability is not checked: an unreachable backend fails its topics' operations and degrades readiness, never the start. |
| C3 | ingest, delivery | Every `backend_routing` value names a registry entry. |
| C4 | ingest, delivery | `backend_routing` carries the instance-less key `gts.cf.core.events.topic.v1~`. |
| C5 | ingest, delivery | Every registry entry is named by at least one route. An unreferenced entry is an operator mistake, and would open connections for nothing. |
| C6 | ingest, delivery, dispatcher | Every group name follows §2.3, and every group pattern is well-formed. |
| C7 | ingest, delivery | `affinity`, when present, names a group in `affinity_groups` or `default`. |
| C8 | ingest | Every `outbox_groups` name follows §2.3's name rule, every pattern is well-formed, and every `partitions` is a power of 2 from 1 to 64 (the toolkit-db outbox's limit); `default` sets nothing but `partitions`. |
| C9 | ingest | No two outbox groups match a topic pattern at equal specificity, so every topic belongs to exactly one outbox. |

A failure names the section, the rule it broke and what the rule requires. It reproduces only validated names - a registry name, a group name, a routing key that parsed as a GTS identifier - and never a settings value.

### 2.5 Outbox Groups

Ingest's transactional outbox is split into named groups, one toolkit-db queue each, so a loud or fragile topic drains apart from the rest.

```yaml
outbox_groups:
  audit:   { partitions: 4,  topics: ["gts.cf.core.events.topic.v1~vendor.audit.v1"] }
  billing: { partitions: 8,  topics: ["gts.cf.core.events.topic.v1~vendor.billing.*"] }
  default: { partitions: 16 }      # reserved, always ["*"]; implicit with 16 partitions when absent
```

- **Same shape as affinity groups.** Names follow the affinity group rule (§2.3) with `default` reserved, and patterns use DESIGN §"Pattern Syntax". The difference is that the choice is deterministic: a topic belongs to the group whose pattern matches it most specifically (exact > longer prefix > `*`), and a tie is a startup error (C9).
- **Independent of affinity.** A dedicated topic typically gets both an affinity group and an outbox group, often with the same name; nothing requires it.
- **Per group:** its own partitions (ingest partitions), its own per-partition `seq`, its own leased processors. A backend failing with `Retry` stalls only the groups whose topics it holds.
- **Ingest partition** of an event: `murmur3("{topic}:{key}") % group.partitions` (feature 0007 §3.1).
- **Fixed once created.** A group's `partitions` cannot change once its queue exists (toolkit-db refuses with `PartitionCountMismatch`). Moving a topic to another group reorders its keys while both queues drain; it is a drain-then-switch operation.
- **Identity.** Each group gets a random 64-bit `outbox_id` the first time it is created, and its outbox tables carry it in their prefix: `event_broker__ob_{outbox_id as 16 hex}` (33 characters, within toolkit-db's 36-character prefix limit), e.g. `event_broker__ob_3f2a9c1e0b7d4e21_partitions`. Each group is its own toolkit-db outbox instance (`table_prefix`), so its partition ids and sequences belong to its own table family; a new table family is a new identity by construction. The identity table maps the configured name to the id, so ingest knows which prefix to open. The backend keys its deduplication by `(outbox_id, ingest_partition)` (feature 0007 §2.2), so a recreated database or a second deployment never collides with an existing fence.

```sql
CREATE TABLE event_broker__outbox_identity (
    name        VARCHAR(63) NOT NULL PRIMARY KEY,   -- group name
    outbox_id   VARCHAR(16) NOT NULL,               -- random 64-bit, 16 lowercase hex; the table-prefix part
    created_at  VARCHAR(32) NOT NULL                -- RFC 3339 UTC
);
```

## 3. Dispatcher Routing

A dispatcher holds no registry and no backend routing. It carries `affinity_groups`, learns each instance's group from `DirectoryService`, and uses it to prefer an instance, never to exclude one.

### 3.1 Published Label

Every ingest and delivery instance registers in `DirectoryService` under its role's name, `event-broker-ingest` or `event-broker-delivery`, with one label:

```
evbk-affinity = audit          # or "default" when `affinity` is absent
```

- A label rather than free-form metadata: `ServiceInstanceInfo` carries only `labels` (`libs/system-sdks/sdks/directory/src/api.rs:154-157`), and a label's key and value are limited to 63 characters of `[A-Za-z0-9._-]`, alphanumeric at both ends (`labels.rs:24-28, 95-102`). A topic pattern does not fit; a group name does.
- Every instance publishes the label, `default` included. A re-registration with no labels keeps the stored ones, so an absent label could outlive the affinity it described.
- It is written at registration and does not change while the instance runs; a new affinity is a new rollout.

### 3.2 Routing Table

```python
# every refresh (fixed 5s cadence), per role
def refresh(role):
    table[role] = [
        Entry(instance.id, instance.rest_endpoint, instance.state,
              patterns=groups.get(instance.labels.get("evbk-affinity", "default"), None))
        for instance in directory.list_instances(role.service_name())
    ]
# groups = affinity_groups from config, plus default -> ["*"]
```

- The dispatcher fetches every instance of both roles and rebuilds the table whole.
- A request is routed from the table, never by a directory call of its own.
- An instance whose label names a group the dispatcher does not know - a rollout in progress - is kept with `["*"]` and a warning naming the instance and the group: it can still handle anything, so leaving it out would only shrink the pool.

### 3.3 Choosing an Instance

```python
def tiers(role, topics):
    # instances ranked by how specifically their group covers every topic in `topics`
    live = [e for e in table[role] if e.state in (Ready, Healthy)]
    covering = [e for e in live if all(matches_any(e.patterns, t) for t in topics)]
    by_rank = group_by(covering, key=lambda e: min(best_rank(e.patterns, t) for t in topics))
    return [by_rank[r] for r in sorted(by_rank, reverse=True)] + [live]   # `live`: last resort

def pick(role, topics):
    for tier in tiers(role, topics):
        if tier: return round_robin(tier)
    return None                                                     # no live instance at all

def best_rank(patterns, topic):
    return max(rank(p, topic) for p in patterns if matches(p, topic))

def rank(pattern, topic):
    if pattern == topic:          return (2, len(pattern))          # exact
    if pattern.endswith(".*"):    return (1, len(pattern) - 2)      # <prefix>.*, longer prefix wins
    return (0, 0)                                                   # "*"
```

| Request | Topics taken from | Role | Choice |
|---|---|---|---|
| `POST /events` | the event's `type`, resolved to its topic (§3.5) | ingest | `pick(ingest, {topic})` |
| `POST /events:batch` | the first event's `type` | ingest | `pick(ingest, {topic})`; a batch may carry several topics, and the rest ride with the first |
| `POST /producers/{id}:reset` with a `topic` | the body | ingest | `pick(ingest, {topic})` |
| `GET /topics/segments` | the `topic` parameter | ingest | `pick(ingest, {topic})` |
| `POST /producers`, `GET /producers/{id}/cursors`, `:reset` without a topic, `GET /topics`, `GET /event-types` | none | ingest | any live instance |
| `POST /subscriptions` joining a group with no owner | every interest's `topic`, from the body | delivery | `pick(delivery, topics)`, then DESIGN's placement rules within the chosen tier |
| any request for a group that has an owner, or for a subscription | the owner, per DESIGN §"Delivery Routing (Cache-Based, Not Hash-Based)" | delivery | the owner |

- A **wide** group - interests in `orders` and `audit` - is covered only by instances whose patterns match both, which in the typical layout is the `default` tier. It lands on a default instance. That is the intended home for it, not a fallback.
- Two dedicated instances down means `audit` falls through to the `default` tier; every tier down but one instance means that instance takes everything. The chain always ends in `live`.

### 3.4 Best Effort

Dispatcher routing is a preference; it rejects nothing.

- A body that does not parse, a `type` it cannot resolve, a topic no group covers: the request goes to any live instance, which validates it and answers with the real error.
- A connection to the chosen instance that fails before any byte of the request is sent is retried once on the next member of the same tier, or of the next tier when the tier has no other member. Nothing is retried once the request has started.
- The dispatcher's own answers are the existing transport ones only: `503` when no instance of the role is live or reachable, `413` for an oversized body.
- The table trails reality by at most one refresh. A request routed on a stale table lands on an instance that handles it anyway, or, if that instance is gone, on the retry target; staleness costs isolation for a few seconds, never a failed request.

### 3.5 Pre-Parsing

The one piece of a publish the dispatcher reads is the event `type`, and it needs the type's topic.

- It reads only the `type` member of the body - the first event's, for a batch - and forwards the body unchanged.
- It resolves type to topic through a bounded LRU cache filled from `types-registry` under the dispatcher's own service `SecurityContext`, and refreshed on the specification refresh cadence.
- A cache miss never delays the request: it routes as "no topic" (§3.4) while the lookup runs in the background, and no negative result is cached. A stream of unknown types costs one lookup each at most and grows no memory past the bound.
- Join bodies name their topics directly, so delivery routing needs no lookup.

## 4. Out of Scope

- Backend capability negotiation.
- Topic provisioning - feature 0006; log identity checks - ADR-0008.
- Delivery group ownership, placement caps and failover - DESIGN §"Delivery Routing (Cache-Based, Not Hash-Based)"; this feature only chooses the tier placement happens in.
- Refusing a request because of affinity; nothing does.

## 5. Definitions of Done

### Configuration

- `backends`, `backend_routing`, `affinity_groups`, `affinity` and `outbox_groups` parse as §2 describes.
- Every rule C1-C9 is enforced at startup, in the roles §2.4 assigns.
- Each outbox group is one toolkit-db queue with an identity row minted on first registration (§2.5).
- Each registry entry is built once per process and shared by every topic routed to it.
- An unmatched fully qualified routing key is warned about on refresh (§2.2).

### Dispatcher

- Ingest and delivery publish the `evbk-affinity` label at every registration, `default` included.
- The dispatcher rebuilds its routing table from every instance on each refresh and routes every request from it (§3.3), retrying once on a connect failure (§3.4).
- The dispatcher reads only the event `type` from a publish body and resolves it through a bounded type-to-topic cache (§3.5).
- No dispatcher decision rejects a request (§3.4).

## 6. Acceptance Criteria

- **AC-1**: A registry entry whose `type` no linked provider serves stops the process at start.
- **AC-2**: A route naming an absent registry entry, a missing instance-less route, an unreferenced registry entry, or an `affinity` naming an undefined group stops ingest and delivery at start.
- **AC-3**: An instance accepts publishes for, and a delivery instance admits groups on, topics outside its group exactly as it does inside it.
- **AC-4**: Two topics routed to one registry entry share one built backend.
- **AC-5**: No startup error or log line reproduces a backend settings value.
- **AC-6**: With two default instances and two in group `audit`, the dispatcher sends `audit` publishes to the dedicated pair and every other topic's to the default pair.
- **AC-7**: With both dedicated instances down, `audit` publishes succeed on a default instance.
- **AC-8**: A group joining with interests in `audit` and `orders` is placed on a default instance.
- **AC-9**: A publish whose body the dispatcher cannot parse, or whose type it cannot resolve, reaches an instance and gets that instance's answer.
- **AC-10**: After an instance's group changes, the dispatcher routes by the new group within one refresh.
- **AC-11**: A backend unreachable at start does not stop the process; its topics fail with `Unavailable` and readiness reports it.

## 7. Unit Test Plan

- **config checks**: each of C1-C9 fails on its own violation and passes otherwise; the unmatched-routing-key warning.
- **outbox groups**: exact beats prefix beats `default`; an absent `default` is 16 partitions; an identity row is minted once per group and reused on restart.
- **specificity**: exact beats every prefix, a longer prefix beats a shorter one, any prefix beats `*`.
- **tiers**: a multi-topic request is ranked by its least specifically covered topic; an instance not covering every topic drops to the last-resort tier; the chain always ends in every live instance.
- **routing table**: a missing label is `default`; an unknown group is `["*"]` with a warning; non-live instances are not candidates.
- **retry**: a connect failure before any byte is retried once on the next candidate; a failure after the request started is not.
- **pre-parsing**: `type` read from a single event and from a batch's first event; unparseable body and unknown type route as "no topic"; a miss does not wait for the lookup; the cache never exceeds its bound.
- **routing resolution**: a fully qualified key wins over the instance-less key; a topic no specific key names takes the instance-less route.

## 8. E2E Test Plan

Suite: `testing/e2e/suites/event_broker/`.

- **S1 - bad config refuses to start**: an unknown backend type, a route to an absent entry, an unreferenced entry, a missing instance-less route, an undefined affinity group; assert each stops the process with its rule named.
- **S2 - two backends**: route two topics to two registry entries; publish to both; assert each topic's events are read back from its own backend.
- **S3 - dedicated routing**: two default and two `audit` instances behind one dispatcher; publish to `audit` and to `orders`; assert each lands on its pair (AC-6).
- **S4 - fallback**: stop both `audit` instances; assert `audit` publishes succeed through the dispatcher (AC-7).
- **S5 - wide group**: join a group with `audit` and `orders` interests; assert it is owned by a default instance and streams both (AC-8).
- **S6 - direct request**: publish to `audit` directly on a default instance, bypassing the dispatcher; assert it succeeds (AC-3).
- **S7 - rollout convergence**: change an instance's group, restarting it; assert the dispatcher follows within one refresh with no failed request meanwhile (AC-10).
- **S8 - unreachable backend at start**: point a registry entry at an unreachable host; assert the process starts, its topics answer `Unavailable`, the other entry's topics work (AC-11).
