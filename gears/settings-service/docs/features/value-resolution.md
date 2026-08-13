<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Feature: Effective Value Resolution, Defaults and Cache

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-value-resolution`

- [ ] `p1` - `cpt-cf-settings-service-feature-value-resolution`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve Effective Value](#resolve-effective-value)
  - [Resolve Effective Values in Bulk](#resolve-effective-values-in-bulk)
  - [Read Effective Source Trail](#read-effective-source-trail)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Scope Class Resolution Dispatch](#scope-class-resolution-dispatch)
  - [Needs-Review Fallthrough](#needs-review-fallthrough)
  - [Cache Lookup and Population](#cache-lookup-and-population)
  - [Cache Invalidation](#cache-invalidation)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Resolver Operations](#resolver-operations)
  - [Scope Class Resolution](#scope-class-resolution)
  - [Single Source of Ancestry](#single-source-of-ancestry)
  - [Effective Value Shape and Inheritance Trail](#effective-value-shape-and-inheritance-trail)
  - [Defaults and Revert Semantics](#defaults-and-revert-semantics)
  - [Flagged Override Is Never Served](#flagged-override-is-never-served)
  - [Distinct Resolution Outcomes](#distinct-resolution-outcomes)
  - [Read-Path Cache](#read-path-cache)
  - [Cache Time-to-Live Backstop](#cache-time-to-live-backstop)
  - [Hierarchy-Change Invalidation](#hierarchy-change-invalidation)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Resolves the effective value of a setting for a scope by dispatching on its scope class, returns the value together with the source and the inheritance trail that produced it, and serves the whole thing from a local in-process cache with eviction on apply and a time-based backstop.

### 1.2 Purpose

This is the feature that makes the service useful to anything other than an administrator. Every consuming gear reaches configuration through it, on the pull read path, which is why it is also the feature most later work waits on.

Three properties matter more than the walk itself.

**A successful read always carries a value.** Every declaration has a non-null Schema Default, so all three scope-class algorithms terminate in one and there is no fourth outcome for "declared, but nothing to serve". A consumer that needs to know whether an administrator actually set something reads `source`, not the value — because a setting whose type admits `null` may legitimately be set to `null`, which is indistinguishable by inspection from a `null` default.

**A flagged value is never served, and never fails the read either.** When an override no longer validates against its current type, the resolver skips it and continues to the nearest valid ancestor or the Schema Default. The consumer gets a usable value rather than an error for a state it did not create, while the flagged override stays visible to the administrator who can fix it.

**Not-found is deliberately two different things.** A retired declaration resolves as a distinct positive fact, so a gear reading through its own upgrade window can tell "the platform withdrew this setting" from "this key was never declared" and drop the dependency rather than retry. A genuinely absent declaration conflates two sub-cases the service cannot distinguish — the owning gear has not registered yet, or the key never existed — and it must not guess between them.

**Requirements**: `cpt-cf-settings-service-fr-cascading-inheritance`, `cpt-cf-settings-service-fr-defaults-revert`, `cpt-cf-settings-service-nfr-performance-read-cache`, `cpt-cf-settings-service-nfr-efficiency-live-read`

**Principles**: `cpt-cf-settings-service-principle-single-ancestry-source`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-internal-caller` | Reads effective values in process through the reader SDK on the hot path |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads effective values and the inheritance trail for scopes within its own subtree |
| `cpt-cf-settings-service-actor-platform-admin` | Reads the full administrative view, including per-entry setter identity on the trail |
| `cpt-cf-settings-service-actor-tenant-resolver` | Sole source of tenant ancestry; supplies the ancestor id chain the cascading walk reads over |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.6 Multi-Tenant Overrides and Cascading Inheritance
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `EffectiveValue`, Enum `EffectiveSource`), §4.2 (Component: Value Resolver, Component: Cache and Invalidation), §4.5 (Service-to-Service Pattern), §4.6 (Interactions and Sequences)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.5
- **Dependencies**: entry 2.4 typed value validation, since the resolution chain terminates in a validated Schema Default and every value it walks is a validated typed value; entry 2.3 for the declaration model and its scope class; entry 2.1 for persistence, the reader trait, and the error taxonomy.
- **Sequences**: `cpt-cf-settings-service-seq-effective-value-read`
- **Not applicable**: The cross-replica cache-invalidation broadcast and its bounded-staleness guarantee are apply-side and belong to a later wave. Tenant override writes, the cascading-impact warning, staging, and Apply are out of scope. Secret plaintext resolution is owned by the Secret Manager; this feature returns a secret-trait value in its masked handle form. The revert **action** is a staged value change; only the resolution semantics of defaults are here.

## 2. Actor Flows (CDSL)

### Resolve Effective Value

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-value-resolution-resolve`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- An effective value returned with its source, source scope, resolved traits, and inheritance trail

**Error Scenarios**:
- The declaration was retired, reported as a distinct outcome
- No declaration row exists at the key
- The value could not be resolved because a dependency was unavailable

**Steps**:
1. [ ] - `p1` - Caller requests the effective value for a setting key at a scope - `inst-vr-resolve-1`
2. [ ] - `p1` - Consult the cache for the `(key, scope)` entry - `inst-vr-resolve-2`
3. [ ] - `p1` - **IF** a live entry is present → **RETURN** it without touching the database - `inst-vr-resolve-3`
4. [ ] - `p1` - DB: SELECT the declaration for the key - `inst-vr-resolve-4`
5. [ ] - `p1` - **IF** no declaration row exists → **RETURN** the not-found outcome, without guessing whether the owning gear has yet to register or the key never existed - `inst-vr-resolve-5`
6. [ ] - `p1` - **IF** the declaration's status is retired → **RETURN** the distinct retired outcome, and do not return its retained values - `inst-vr-resolve-6`
7. [ ] - `p1` - Invoke scope-class resolution dispatch for the declaration and the requested scope - `inst-vr-resolve-7`
8. [ ] - `p1` - **IF** a dependency needed for the walk is unavailable → **RETURN** the unavailable outcome rather than substituting the Schema Default, which lives in the same database and is equally unreachable - `inst-vr-resolve-8`
9. [ ] - `p1` - Resolve the declaration's trait set for rendering metadata - `inst-vr-resolve-9`
10. [ ] - `p1` - **IF** the setting is secret-backed → return the value in its masked handle form, never plaintext - `inst-vr-resolve-10`
11. [ ] - `p1` - Populate the cache entry for `(key, scope)` with the resolved value and its source trace - `inst-vr-resolve-11`
12. [ ] - `p1` - **RETURN** the effective value carrying `key`, `scope`, `value`, `source`, `source_scope`, `traits`, and the inheritance trail - `inst-vr-resolve-12`

### Resolve Effective Values in Bulk

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-value-resolution-resolve-bulk`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- A result per requested key, each independently successful or failed, sharing one ancestry walk

**Error Scenarios**:
- Individual keys fail without failing the batch

**Steps**:
1. [ ] - `p1` - Caller requests effective values for a set of keys, or for a category, at one scope - `inst-vr-bulk-1`
2. [ ] - `p1` - Obtain the ancestor chain for the scope once and share it across every key in the batch - `inst-vr-bulk-2`
3. [ ] - `p1` - **FOR EACH** requested key - `inst-vr-bulk-3`
   1. [ ] - `p1` - Resolve it independently, reusing the shared ancestry - `inst-vr-bulk-4`
   2. [ ] - `p1` - Record either the resolved effective value or that key's own failure outcome - `inst-vr-bulk-5`
4. [ ] - `p1` - **RETURN** one outcome per key, never collapsing the batch to a single failure because one key failed - `inst-vr-bulk-6`

### Read Effective Source Trail

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-value-resolution-source-trail`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The ordered list of scopes inspected during resolution, identifying which supplied the value

**Error Scenarios**:
- A request for a scope outside the caller's own subtree

**Steps**:
1. [ ] - `p1` - Actor requests the effective source and trail for a key at a scope - `inst-vr-trail-1`
2. [ ] - `p1` - Authorize the read and confirm the requested scope lies within the caller's own subtree - `inst-vr-trail-2`
3. [ ] - `p1` - **IF** the scope lies outside that subtree → **RETURN** a denial - `inst-vr-trail-3`
4. [ ] - `p1` - Perform the resolution walk, recording each scope inspected in order - `inst-vr-trail-4`
5. [ ] - `p1` - Limit the trail to the caller's own ancestor chain from root to self, never including a sibling or descendant scope - `inst-vr-trail-5`
6. [ ] - `p1` - **IF** the caller is an administrative reader → include the per-entry setter identity and timestamp - `inst-vr-trail-6`
7. [ ] - `p1` - **ELSE** omit setter identity, so an ancestor's setter is not exposed to a subordinate tenant through the consumer path - `inst-vr-trail-7`
8. [ ] - `p1` - Derive the value arm of the recency indicator from the resolved row alone, never as a maximum across sibling or descendant scopes - `inst-vr-trail-8`
9. [ ] - `p1` - **RETURN** the source, the scope that provided the value, and the trail - `inst-vr-trail-9`

## 3. Processes / Business Logic (CDSL)

### Scope Class Resolution Dispatch

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-value-resolution-dispatch`

**Input**: A declaration and a requested scope

**Output**: The resolved value with its source and source scope

**Steps**:
1. [ ] - `p1` - **IF** the declaration's scope class is `global` - `inst-vr-disp-1`
   1. [ ] - `p1` - DB: SELECT the platform-scope row for the declaration, identified by a null tenant - `inst-vr-disp-2`
   2. [ ] - `p1` - **IF** the request comes from a tenant scope → serve the platform value read-only, and only when the declaration is tenant-visible - `inst-vr-disp-3`
   3. [ ] - `p1` - **IF** no platform row exists → **RETURN** the Schema Default with the default source - `inst-vr-disp-4`
2. [ ] - `p1` - **IF** the declaration's scope class is `cascading` - `inst-vr-disp-5`
   1. [ ] - `p1` - Ask the tenant resolver for the requested tenant's ancestor ids, ordered root to self - `inst-vr-disp-6`
   2. [ ] - `p1` - DB: SELECT value rows for the declaration where the tenant is null or within the ancestor id set, as one exact-match set query with no prefix or pattern scan - `inst-vr-disp-7`
   3. [ ] - `p1` - Prefer the deepest matching scope, applying needs-review fallthrough as each candidate is considered - `inst-vr-disp-8`
   4. [ ] - `p1` - **IF** the deepest valid match is the requested tenant → set the source to own override - `inst-vr-disp-9`
   5. [ ] - `p1` - **ELSE IF** a valid ancestor match exists → set the source to inherited and record its scope - `inst-vr-disp-10`
   6. [ ] - `p1` - **ELSE** **RETURN** the Schema Default with the default source and a null source scope - `inst-vr-disp-11`
3. [ ] - `p1` - **IF** the declaration's scope class is `local` - `inst-vr-disp-12`
   1. [ ] - `p1` - DB: SELECT only the row for the requested tenant, performing no ancestor walk - `inst-vr-disp-13`
   2. [ ] - `p1` - **IF** absent or flagged → **RETURN** the Schema Default, since a local setting is never inherited - `inst-vr-disp-14`
4. [ ] - `p1` - **RETURN** the resolved value, its source, and its source scope - `inst-vr-disp-15`

### Needs-Review Fallthrough

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough`

**Input**: A candidate override row under consideration during resolution

**Output**: Accept the candidate, or skip it and continue the walk

**Steps**:
1. [ ] - `p1` - **IF** the candidate is not flagged for review → accept it as the resolved value - `inst-vr-nrf-1`
2. [ ] - `p1` - **IF** the candidate is flagged → skip it without serving it and without raising an error to the consumer - `inst-vr-nrf-2`
3. [ ] - `p1` - **IF** the scope class is `cascading` → continue to the next nearest valid ancestor override - `inst-vr-nrf-3`
4. [ ] - `p1` - **IF** the scope class is `local` or `global`, or no valid ancestor remains → fall through to the Schema Default - `inst-vr-nrf-4`
5. [ ] - `p1` - Leave the flagged row in place, excluded from apply until corrected and visible on the administrative listing - `inst-vr-nrf-5`
6. [ ] - `p1` - **RETURN** the accepted value, having never surfaced review state as a consumer-facing error - `inst-vr-nrf-6`

### Cache Lookup and Population

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-value-resolution-cache-read`

**Input**: A setting key and a scope

**Output**: A cached effective value, or a miss that the resolver then populates

**Steps**:
1. [ ] - `p1` - Look up the entry keyed by the pair of setting key and scope - `inst-vr-cache-1`
2. [ ] - `p1` - **IF** no entry exists → **RETURN** a miss - `inst-vr-cache-2`
3. [ ] - `p1` - **IF** the entry is older than the configured time-to-live → evict it and **RETURN** a miss, so a missed invalidation self-heals within that bound - `inst-vr-cache-3`
4. [ ] - `p1` - **RETURN** the entry together with the source trace it was stored with - `inst-vr-cache-4`

### Cache Invalidation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-value-resolution-cache-invalidate`

**Input**: An invalidation request naming a declaration key, and optionally a scope

**Output**: Evicted cache entries

**Steps**:
1. [ ] - `p1` - **IF** a specific scope is named → evict the entry for that key and scope on this instance - `inst-vr-inv-1`
2. [ ] - `p1` - **IF** the affected declaration is `cascading` → evict every cached scope for that key, because an ancestor change alters descendants' effective values and they must re-resolve lazily on next read - `inst-vr-inv-2`
3. [ ] - `p1` - **WHEN** a tenant hierarchy change is signalled, such as a re-parent or a mid-chain insertion → evict the cached entries of the affected subtree for every cascading declaration, since an effective value is a function of the ancestor chain and no apply need be involved - `inst-vr-inv-3`
4. [ ] - `p1` - Record that the tenant resolver publishes no such hierarchy signal today, so until it does the time-to-live is the only backstop and the post-re-parent staleness window equals it - `inst-vr-inv-4`
5. [ ] - `p1` - **RETURN** having evicted locally only; converging peer replicas is apply-side and out of scope here - `inst-vr-inv-5`

## 4. States (CDSL)

Not applicable. `EffectiveValue` is computed on each read and never persisted, so it has no lifecycle of its own. The review state that influences the walk belongs to `SettingValue` and is modelled in entry 2.4. Cache entries are evicted rather than transitioned, and their eviction rules are captured as processes above.

## 5. Definitions of Done

### Resolver Operations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-operations`

The system **MUST** expose single-key resolution, bulk resolution, and effective-source inspection. Bulk resolution **MUST** share one ancestry walk per scope and **MUST** return an independent outcome per key so that one failing key never fails the batch.

**Implements**:
- `cpt-cf-settings-service-flow-value-resolution-resolve`
- `cpt-cf-settings-service-flow-value-resolution-resolve-bulk`
- `cpt-cf-settings-service-flow-value-resolution-source-trail`

**Touches**:
- Entities: `EffectiveValue`, `EffectiveSource`

### Scope Class Resolution

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-scope-class`

Resolution **MUST** dispatch on the declaration's scope class: a `global` setting reads its platform row or the Schema Default and is exposed to tenants read-only under visibility alone; a `cascading` setting resolves nearest-first over its ancestor chain preferring the deepest match; a `local` setting reads only its own scope with no ancestor walk. Every path **MUST** terminate in the Schema Default so a successful read always carries a value.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-dispatch`

**Touches**:
- DB Table: `setting_values`, `setting_declarations`
- Entities: `EffectiveValue`

### Single Source of Ancestry

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-ancestry`

The cascading walk **MUST** obtain ancestry from the tenant resolver and **MUST NOT** reconstruct the hierarchy from stored scope values. The value row's scope column **MUST** be read as an id, never parsed as a path, and the query **MUST** be an exact-match set lookup rather than a prefix or pattern scan, so a tenant re-parent requires no stored-scope rewrite.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-dispatch`

**Touches**:
- DB Table: `setting_values`
- Entities: `EffectiveValue`

### Effective Value Shape and Inheritance Trail

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-shape`

The resolved result **MUST** carry the key, the requested scope, the value, the source, the scope that supplied it, the resolved trait set, and the inheritance trail. The trail **MUST** be limited to the caller's own ancestor chain and **MUST NOT** include a sibling or descendant scope. Per-entry setter identity and timestamp **MUST** appear only on the administrative read and never on the consumer path. The recency indicator's value arm **MUST** derive from the resolved row alone.

**Implements**:
- `cpt-cf-settings-service-flow-value-resolution-source-trail`

**Touches**:
- Entities: `EffectiveValue`, `EffectiveSource`

### Defaults and Revert Semantics

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-defaults`

The Schema Default **MUST** terminate every resolution chain, **MUST** remain independent of any override, and **MUST** survive an override being set and later removed. A consumer distinguishing a configured value from an untouched one **MUST** be able to do so from the source alone, because a type admitting `null` makes the value itself unable to carry that signal.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-dispatch`

**Touches**:
- Entities: `EffectiveSource`

### Flagged Override Is Never Served

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-fallthrough`

A value flagged for review **MUST NOT** be served and **MUST NOT** produce a consumer-facing error. Resolution **MUST** continue past it to the nearest valid ancestor override or the Schema Default, and the flagged row **MUST** remain in place, excluded from apply and visible to administrators.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-needs-review-fallthrough`

**Touches**:
- DB Table: `setting_values`
- Entities: `EffectiveValue`

### Distinct Resolution Outcomes

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-outcomes`

A retired declaration **MUST** resolve as a distinct retired outcome rather than as not-found, and its retained values **MUST NOT** be returned. A key with no declaration row **MUST** resolve as not-found without the service guessing whether the owning gear has yet to register or the key never existed. A key made stale by a category rename **MUST** be indistinguishable from one that never existed, since no alias or key history is retained. An unresolvable dependency **MUST** surface as unavailable rather than as a substituted default.

**Implements**:
- `cpt-cf-settings-service-flow-value-resolution-resolve`

**Constraints**: `cpt-cf-settings-service-constraint-effective-on-next-read`

**Touches**:
- Entities: `EffectiveValue`

### Read-Path Cache

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-cache`

The system **MUST** provide a local in-process cache keyed by setting key and scope, storing the resolved value with its source trace, consulted before any database read and populated on miss. Eviction **MUST** be key-wide for a cascading declaration so descendants re-resolve lazily.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-cache-read`
- `cpt-cf-settings-service-algo-value-resolution-cache-invalidate`

**Touches**:
- Entities: cache entry

### Cache Time-to-Live Backstop

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-value-resolution-cache-ttl`

The cache **MUST** own a configurable time-to-live and **MUST** evict entries older than it as a backstop, so a missed invalidation self-heals within that bound rather than persisting indefinitely. This cache is the definition site for that knob; other components reference it rather than defining their own.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-cache-read`

**Touches**:
- Entities: cache entry

### Hierarchy-Change Invalidation

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-value-resolution-hierarchy-invalidation`

The cache **MUST** evict the affected subtree's cascading entries on a tenant hierarchy change such as a re-parent or a mid-chain insertion, because an effective value is a function of the ancestor chain and can change with no apply involved. The tenant resolver publishes no such signal today, so this **MUST** be documented as depending on that signal, with the time-to-live as the interim backstop.

**Implements**:
- `cpt-cf-settings-service-algo-value-resolution-cache-invalidate`

**Touches**:
- Entities: cache entry

## 6. Acceptance Criteria

- [ ] A `global` setting with a platform row resolves to that row's value with a platform source scope
- [ ] A `global` setting with no platform row resolves to its Schema Default
- [ ] A `global` setting is served to a tenant read-only when tenant-visible, and is not served when it is not
- [ ] A `cascading` setting with an override at the requested tenant resolves as an own override
- [ ] A `cascading` setting with no own override but an ancestor override resolves as inherited and names the ancestor scope
- [ ] A `cascading` setting with overrides at two ancestors resolves to the deeper of the two
- [ ] A `cascading` setting with no override anywhere resolves to its Schema Default with a null source scope
- [ ] A `local` setting resolves only from its own scope and never inherits from an ancestor
- [ ] A `local` setting with no own value resolves to its Schema Default
- [ ] Resolution issues an exact-match set query over ancestor ids, with no prefix or pattern scan against the scope column
- [ ] Ancestry comes from the tenant resolver, and no code path reconstructs the hierarchy from stored scope values
- [ ] A flagged override at the requested scope is skipped and the nearest valid ancestor value is served instead
- [ ] A flagged override with no valid ancestor falls through to the Schema Default
- [ ] A flagged override is never returned to a consumer and never produces a consumer-facing error
- [ ] A flagged override remains present in storage and appears on the administrative listing
- [ ] A retired declaration resolves as the retired outcome, distinct from not-found, and its retained values are not returned
- [ ] A key with no declaration resolves as not-found, and the response does not assert which sub-case applies
- [ ] A key made stale by a category rename is indistinguishable from a key that never existed
- [ ] An unreachable dependency yields the unavailable outcome rather than a substituted Schema Default
- [ ] A setting whose type admits `null` and is explicitly set to `null` is distinguishable from an unset one by source alone
- [ ] A bulk read returns one outcome per key, and a single failing key leaves the other results intact
- [ ] A bulk read performs one ancestry lookup per scope rather than one per key
- [ ] A second read of the same key and scope is served from cache without a database query
- [ ] Applying a change to a cascading declaration evicts every cached scope for that key
- [ ] A cache entry older than the configured time-to-live is treated as a miss and re-resolved
- [ ] The inheritance trail contains only the caller's own ancestor chain, and never a sibling or descendant scope
- [ ] Setter identity appears on the administrative trail and is absent from the consumer result
- [ ] A tenant admin requesting a trail for a scope outside its subtree is denied
