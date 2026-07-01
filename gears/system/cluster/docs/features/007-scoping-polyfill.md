# Feature: Per-primitive Scoping & Prefix-Watch Polyfill

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-scoping-polyfill-implemented`

- [x] `p2` - `cpt-cf-clst-feature-scoping-polyfill`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Composable Scoped Names](#composable-scoped-names)
  - [Prefix Watch via Polyfill](#prefix-watch-via-polyfill)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Prefix Translation](#prefix-translation)
  - [Polling Diff](#polling-diff)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Per-primitive Scoping Wrappers](#per-primitive-scoping-wrappers)
  - [Polling Prefix-Watch Polyfill](#polling-prefix-watch-polyfill)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Lets a consumer carve composable sub-namespaces inside any primitive without manual prefixing, and synthesizes prefix-watch semantics on backends that lack native support. Scoping applies to coordination names only; for service discovery it scopes the service name but never the metadata keys or values.

### 1.2 Purpose

Without per-module namespacing, two modules sharing a profile would collide on cache keys, lock names, election names, and service names; manual prefixing is bug-prone. This feature handles namespacing transparently and provides a polyfill so prefix-watch consumers work even on backends without native prefix subscriptions.

**Requirements**: `cpt-cf-clst-fr-namespacing-scoped`, `cpt-cf-clst-fr-namespacing-sd-metadata-unscoped`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Scopes its names per-module to avoid cross-module collisions |
| `cpt-cf-clst-actor-event-broker` | Subdivides its own namespace per-shard and watches by prefix |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.8
- **Design**: [DESIGN.md](../DESIGN.md) §3.8 (per-primitive scoping rules), §3.12 (polyfill)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-cache-primitive`
  - [x] `p2` - `cpt-cf-clst-feature-leader-election`
  - [x] `p2` - `cpt-cf-clst-feature-distributed-lock`
  - [x] `p2` - `cpt-cf-clst-feature-service-discovery`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — addressed: the polling prefix-watch polyfill documents its non-trivial cost and is opt-in, directing high-scale consumers toward native prefix-watch backends (§2).
- Reliability — addressed: the polling task stops when its watch is dropped and the diff approximates prefix-watch recovery (§3); scoping itself is a stateless name translation.

## 2. Actor Flows (CDSL)

### Composable Scoped Names

- [x] `p1` - **ID**: `cpt-cf-clst-flow-scoping-polyfill-scoped-names`

**Actor**: `cpt-cf-clst-actor-event-broker`

**Success Scenarios**:
- A consumer scopes per-module and then per-shard; it sees name-relative names while the backend sees the composed prefix.

**Error Scenarios**:
- A prefix that violates the name rule is rejected at scope construction.

**Steps**:
1. [x] - `p1` - Consumer scopes a primitive by a prefix, then scopes again to nest - `inst-sn-scope`
2. [x] - `p1` - **IF** a prefix violates the cluster name rule - `inst-sn-validate`
   1. [x] - `p1` - **RETURN** an invalid-name error at scope construction - `inst-sn-reject`
3. [x] - `p1` - On the write path the wrapper prepends the composed prefix to the coordination name - `inst-sn-prepend`
4. [x] - `p1` - On the read path the wrapper strips the prefix before returning names to the consumer - `inst-sn-strip`
5. [x] - `p1` - For service discovery the wrapper scopes only the service name; metadata keys and values pass through unchanged - `inst-sn-meta`

### Prefix Watch via Polyfill

- [x] `p1` - **ID**: `cpt-cf-clst-flow-scoping-polyfill-prefix-watch`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- A consumer watches a prefix on a backend without native prefix-watch by opting into the polling polyfill.

**Error Scenarios**:
- The polyfill cost is non-trivial — the consumer is warned and may route to a native prefix-watch backend at scale.

**Steps**:
1. [x] - `p1` - **IF** the backend declares no native prefix-watch support - `inst-pw-check`
   1. [x] - `p1` - Consumer opts into the polling prefix watch with an interval - `inst-pw-optin`
2. [x] - `p1` - The polyfill periodically lists keys under the prefix and diffs against the previous list - `inst-pw-poll`
3. [x] - `p1` - The polyfill emits changed/deleted watch events for observed differences - `inst-pw-emit`
4. [x] - `p1` - **IF** the watch is dropped - `inst-pw-drop`
   1. [x] - `p1` - The polling task stops - `inst-pw-stop`

## 3. Processes / Business Logic (CDSL)

### Prefix Translation

- [x] `p1` - **ID**: `cpt-cf-clst-algo-scoping-polyfill-prefix-translate`

**Input**: A scoped operation and the composed prefix

**Output**: Backend-facing names with the prefix applied, and consumer-facing names with it stripped

**Steps**:
1. [x] - `p1` - Compose nested prefixes into a single effective prefix ending with a separator - `inst-pt-compose`
2. [x] - `p1` - On write, prepend the prefix to the cache key, lock name, election name, or service name - `inst-pt-write`
3. [x] - `p1` - On read, strip the prefix from returned keys, instance names, and event keys - `inst-pt-read`
4. [x] - `p1` - **IF** the primitive is service discovery - `inst-pt-sd`
   1. [x] - `p1` - Leave metadata keys and values unchanged - `inst-pt-sd-meta`

### Polling Diff

- [x] `p1` - **ID**: `cpt-cf-clst-algo-scoping-polyfill-poll-diff`

**Input**: A prefix and a polling interval

**Output**: Synthesized changed/deleted events approximating a prefix watch

**Steps**:
1. [x] - `p1` - **FOR EACH** interval tick - `inst-pd-tick`
   1. [x] - `p1` - List current keys under the prefix - `inst-pd-list`
   2. [x] - `p1` - Diff against the previous listing - `inst-pd-diff`
   3. [x] - `p1` - Emit changed events for new/updated keys and deleted events for removed keys - `inst-pd-emit`

## 4. States (CDSL)

Not applicable — scoping wrappers and the polyfill are stateless translations and a polling task; they introduce no entity lifecycle.

## 5. Definitions of Done

### Per-primitive Scoping Wrappers

- [x] `p1` - **ID**: `cpt-cf-clst-dod-scoping-polyfill-wrappers`

The system **MUST** provide scoping wrappers for all four primitives that prepend a validated prefix on the write path and strip it on the read path, compose cleanly when nested, and — for service discovery — scope only the service name, never metadata keys or values.

**Implements**:
- `cpt-cf-clst-flow-scoping-polyfill-scoped-names`
- `cpt-cf-clst-algo-scoping-polyfill-prefix-translate`

**Touches**:
- Entities: ScopedCacheBackend, ScopedLeaderElectionBackend, ScopedDistributedLockBackend, ScopedServiceDiscoveryBackend

### Polling Prefix-Watch Polyfill

- [x] `p1` - **ID**: `cpt-cf-clst-dod-scoping-polyfill-polling`

The system **MUST** provide an opt-in polling prefix-watch that synthesizes prefix-watch events on backends declaring no native support, documents its cost, and stops its polling task when the watch is dropped.

**Implements**:
- `cpt-cf-clst-flow-scoping-polyfill-prefix-watch`
- `cpt-cf-clst-algo-scoping-polyfill-poll-diff`

**Touches**:
- Entities: PollingPrefixWatch

## 6. Acceptance Criteria

- [x] Scoping composes (per-module then per-shard) and is invisible inside consumer code; the backend sees the composed prefix.
- [x] An invalid prefix is rejected at scope construction with an invalid-name error.
- [x] For service discovery, scoping applies to the service name only; metadata keys and values are unchanged.
- [x] The polling polyfill synthesizes changed/deleted prefix-watch events on backends without native support and documents its cost.
- [x] Dropping a polyfilled watch stops its polling task.
