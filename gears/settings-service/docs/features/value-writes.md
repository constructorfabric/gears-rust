<!-- Created: 2026-09-06 by Constructor Tech -->
<!-- Updated: 2026-09-06 by Constructor Tech -->

# Feature: Validate and Set Values

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-value-writes`

- [ ] `p1` - `cpt-cf-settings-service-feature-value-writes`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Validate a Value Without Storing It](#validate-a-value-without-storing-it)
  - [Set a Value](#set-a-value)
  - [Set Several Values in One Call](#set-several-values-in-one-call)
  - [Revert a Value](#revert-a-value)
  - [Clone a Value From Another Scope](#clone-a-value-from-another-scope)
  - [Remove a Value](#remove-a-value)
  - [Report Cascading Impact](#report-cascading-impact)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Write Gate Order](#write-gate-order)
  - [Step-Up Verification](#step-up-verification)
  - [Commit One Change](#commit-one-change)
  - [Value State Tag](#value-state-tag)
  - [Bounded Impact Walk](#bounded-impact-walk)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Value Writer Operations](#value-writer-operations)
  - [Two Gates, in Order](#two-gates-in-order)
  - [Step-Up Verifier Port and Default Binding](#step-up-verifier-port-and-default-binding)
  - [Per-Change Atomicity](#per-change-atomicity)
  - [Stale-Write Rejection](#stale-write-rejection)
  - [Commit, Evict, Publish](#commit-evict-publish)
  - [Bulk Set](#bulk-set)
  - [Bounded, Non-Blocking Impact](#bounded-non-blocking-impact)
  - [Secret Values Route Through the Secret Manager](#secret-values-route-through-the-secret-manager)
  - [Write Outcomes Are Observable](#write-outcomes-are-observable)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Delivers the write path: a read-only check of what a value would do, and set, batch, revert, clone and remove operations that validate inline, refuse a stale write, take effect when the caller sets them, and commit each change together with its audit record. It is where the `StepUpVerifier` port acquires its default OIDC/JWKS binding.

### 1.2 Purpose

A change must not reach a live platform by accident. The guard is that the caller sets it on purpose, that the service validates it and refuses a stale write, and that where the declaration demands it a human has just proved they are present. There is no pending state and no separate activation step: a value operation takes effect when the caller sets it, and a client that lets an administrator collect several changes keeps that collection on its own side and sends it as one batch.

Two gates are kept apart deliberately. Authorization asks *may this caller write this setting here* and applies to every caller. Elevated confirmation asks *has a human just proved they are present*, which only a human can answer, so a service principal is refused outright on a declaration that requires it rather than asked for a ceremony it cannot perform. Authorization is always decided first: an unauthorized caller is refused without step-up being consulted.

The commit is per change, in one transaction with its audit record — a value live with no record of who set it is exactly the window that would open if the two were committed apart — and the order after the commit is fixed: commit, then evict the local cache, then publish. No consumer can observe an invalidation for a value that is not yet stored.

Step-up is verified locally. The token the caller presents is checked against the identity provider's published JWKS, its subject against the session, its authentication time against a freshness window, and its assurance claims against what the deployment requires. The provider is never called on the write path, so its availability is not a per-write failure mode. The verifier is a port this gear declares and binds; a binding that cannot fail is not a binding, and the only sanctioned non-verifying one lives in the test harness.

**Requirements**: `cpt-cf-settings-service-fr-set-value`, `cpt-cf-settings-service-fr-validate-before-set`, `cpt-cf-settings-service-fr-live-read-activation`, `cpt-cf-settings-service-fr-tenant-overrides`, `cpt-cf-settings-service-nfr-reliability-validated-set`, `cpt-cf-settings-service-nfr-ops-set-monitoring`

**Principles**: `cpt-cf-settings-service-principle-write-scope`, `cpt-cf-settings-service-principle-inform-not-block`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Sets, reverts, clones and removes values at platform scope and at any tenant, re-authenticating where the declaration requires it |
| `cpt-cf-settings-service-actor-tenant-admin` | Does the same within its own subtree, when its own effective access is `overridable` |
| `cpt-cf-settings-service-actor-service-writer` | Writes without step-up where the declaration permits a machine writer, and is refused before validation where it does not; the SDK path for it is R2 |
| `cpt-cf-settings-service-actor-authn-resolver` | Issues the session and step-up tokens; the step-up token is verified locally against the provider's JWKS, never by calling the resolver on the write path |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides `write` on the setting's key, and `read` at the source scope of a clone |
| `cpt-cf-settings-service-actor-tenant-resolver` | Answers whether the target lies within the caller's subtree, and supplies the descendants the impact report walks |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.5 Validate and Set Values, §5.4 Defaults & Revert, §5.6 Multi-Tenant Overrides & Cascading Inheritance, §6.1 (Reliability, Operational Visibility)
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Value Writer, including *Two gates, two questions*, *Stale-write rejection*, *Set atomicity model*, *Step-up contract*, *Step-up verification is a swappable `StepUpVerifier` plugin*), §4.3 (REST API — Setting Values (writes), Set Rules, Bulk Set Rules, Revert & Clone Rules), §4.4 (Events Emitted), §4.6 (Sequence: Validate and set a value), §4.8 (Authorization Model), §4.9 (Gear init), §7 (Feature Metrics)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.8
- **Dependencies**: entry 2.5 for the current effective value the validate report and the impact preview compare against, and for the cache the commit evicts; entry 2.6, since every change commits its audit record in the same transaction and refuses without it; entry 2.7 for the caller's effective access; entry 2.4 for the Type Validator and the `setting_values` table; entry 2.3 for the declaration, its scope class and its `requires_step_up`.
- **Sequences**: `cpt-cf-settings-service-seq-validate-and-set`
- **Not applicable**: The service-principal SDK write `set_value` is R2; this feature enforces the gate table's machine column — refusal before validation on a declaration that requires step-up — for any caller the platform identifies as a service principal. Dependency Groups, and their all-or-nothing set, are R3. Consumer `change_notification` delivery and the cross-replica `cache_invalidate` broadcast are owned by the Settings Activation and are not bound in R1: the Change Publisher port is called, the local eviction is the write's only cache effect, and a second host would be stale for at most `cache_ttl_seconds`. Secret plaintext storage is the Secret Manager's (entry 2.9); this feature routes to its port and refuses a secret write as unavailable while nothing is bound to it. The platform-wide elevated session that replaces the token check is R2 and lands behind the same port.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-settings-service-usecase-review-and-set`, `cpt-cf-settings-service-usecase-configure-setting-type-aware`

Throughout, `tenant` omitted means the caller's own tenant, which for a platform administrator is the root tenant and therefore platform scope; every operation on a value targets the caller's own tenant or a descendant within its subtree, and a target outside it, or a standalone descendant, is rejected `403`.

### Validate a Value Without Storing It

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-validate`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- A report saying whether the value is valid, what the effective value and its source currently are, and — for a `cascading` setting — which descendants the change would affect, in pages; the same answer for the same inputs, and nothing written

**Error Scenarios**:
- The caller may not read the setting at that scope, or the setting is hidden from it
- The target is outside the caller's subtree

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/settings/{key}/validate?tenant={tenant_id} with the candidate `value` and an optional `limit` for the impact page - `inst-vw-val-1`
2. [x] - `p1` - Authorize `read` on the setting's key; **IF** deny or cannot be obtained → **RETURN** `403`; no step-up is consulted, since nothing is written - `inst-vw-val-2`
3. [x] - `p1` - Confirm the target is within the caller's subtree and not standalone; **IF** not → **RETURN** `403` - `inst-vw-val-3`
4. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** the caller's effective access is `hidden` → **RETURN** `404` - `inst-vw-val-4`
5. [x] - `p1` - Validate the value through the Type Validator against the declaration's `value_type_id`, including the size cap and numeric canonicality, collecting field-level detail rather than stopping at the first fault - `inst-vw-val-5`
6. [x] - `p1` - Resolve the current effective value and its source at the target through the Value Resolver - `inst-vw-val-6`
7. [x] - `p1` - **IF** the scope class is `cascading` → invoke the bounded impact walk for the target and the candidate value - `inst-vw-val-7`
8. [x] - `p1` - **RETURN** `200` with `valid` and any violations, the current effective value and source, and the impact page; the call stores nothing, emits no audit record, and is never a prerequisite for a write - `inst-vw-val-8`

### Set a Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-set`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The value stored at the target scope and effective on the next read, the response carrying the old value, the new value, the scope and the new tag

**Error Scenarios**:
- Not authorized to write, or the target outside the subtree
- The setting hidden from the caller
- A tenant-scoped write to a `global` setting, or a tenant caller whose own effective access is not `overridable`
- Step-up required and absent, stale, or bound to another subject; a service principal on a declaration that requires step-up
- The value invalid, oversized, or not canonical
- The value changed since the caller read it
- The secret store, the audit store, or the database unavailable

**Steps**:
1. [x] - `p1` - Actor sends PUT /settings-service/v1/settings/{key}/value?tenant={tenant_id} with `value`, `If-Match`, and the step-up token where the declaration requires it - `inst-vw-set-1`
2. [x] - `p1` - Invoke the write gate order for the caller, the declaration and the target; **IF** any gate refuses → **RETURN** its refusal, nothing having been stored or validated beyond that gate - `inst-vw-set-2`
3. [x] - `p1` - Invoke commit one change with the value and the `If-Match` tag - `inst-vw-set-3`
4. [x] - `p1` - **IF** the change was rejected → **RETURN** its outcome — `400` with field-level detail for an invalid value, `412` for a moved value, `503` for an unavailable dependency — with nothing stored - `inst-vw-set-4`
5. [x] - `p1` - Mint a change set id for the request and carry it on the audit record, so the change is retrievable with any activation tracking that later refers to it - `inst-vw-set-5`
6. [x] - `p1` - **RETURN** `200` with `old_value`, `new_value`, `scope` and the new `etag`; the value is effective on the next read - `inst-vw-set-6`

### Set Several Values in One Call

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-batch`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- One result per change — old value, new value, scope, and success or the error that rejected it — each change standing or falling alone

**Error Scenarios**:
- More than five hundred changes in one request
- Step-up required by any target declaration and not satisfied, refusing the whole request before any item is evaluated

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/settings/batch with a list of changes, each carrying `key`, optional `tenant`, `value` and `if_match`, and the step-up token - `inst-vw-batch-1`
2. [x] - `p1` - **IF** the list carries more than five hundred changes → **RETURN** `400` - `inst-vw-batch-2`
3. [x] - `p1` - Verify step-up **once** for the request when any target declaration requires it; **IF** it fails → **RETURN** the refusal with nothing evaluated - `inst-vw-batch-3`
4. [x] - `p1` - Mint one change set id for the request - `inst-vw-batch-4`
5. [x] - `p1` - **FOR EACH** change, in order - `inst-vw-batch-5`
   1. [x] - `p1` - Invoke the remaining write gates for its key and target, then commit one change - `inst-vw-batch-6`
   2. [x] - `p1` - Record its outcome: the old and new value and scope on success, or the error that rejected it; a failing change stores nothing and does not stop the others - `inst-vw-batch-7`
6. [x] - `p1` - Evict the local cache for every committed change, then publish the committed keys under the change set id - `inst-vw-batch-8`
7. [x] - `p1` - **RETURN** `200` with one entry per change; the status reflects that every item was answered, and the caller reads the outcomes - `inst-vw-batch-9`

### Revert a Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-revert`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The override at the target scope cleared, the response carrying the value the scope now falls back to: the nearest ancestor override for a tenant scope of a `cascading` setting, otherwise the Schema Default

**Error Scenarios**:
- The write gates refuse, or `If-Match` is absent or stale
- No override exists at the target scope

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/settings/{key}/value/revert?tenant={tenant_id} with `If-Match` and the step-up token where required - `inst-vw-rev-1`
2. [x] - `p1` - Invoke the write gate order; **IF** any gate refuses → **RETURN** its refusal - `inst-vw-rev-2`
3. [x] - `p1` - Compute the fallback the scope will resolve to once its override is gone, through the Value Resolver — the same fallback `validate` reports beforehand - `inst-vw-rev-3`
4. [x] - `p1` - Invoke commit one change as a removal of the scope's own row, guarded on `If-Match`; **IF** rejected → **RETURN** its outcome - `inst-vw-rev-4`
5. [x] - `p1` - **RETURN** `200` with the resolved fallback and its source; the Schema Default is untouched by the revert - `inst-vw-rev-5`

### Clone a Value From Another Scope

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-clone`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The effective value resolved at the source scope stored as an explicit override at the target, with no continuing link between the two

**Error Scenarios**:
- The caller may not read at the source or write at the target, or either scope lies outside its subtree
- The setting is `secret`-classified
- `If-Match` absent or stale at the target

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/settings/{key}/value/clone?tenant={to} with `from` in the body, `If-Match` for the target, and the step-up token where required - `inst-vw-clone-1`
2. [x] - `p1` - Authorize `read` on the setting's key at the source and confirm the source is within the caller's subtree; **IF** not → **RETURN** `403`, so a clone cannot lift a value out of a scope the caller may not read - `inst-vw-clone-2`
3. [x] - `p1` - Invoke the write gate order for the target; **IF** any gate refuses → **RETURN** its refusal - `inst-vw-clone-3`
4. [x] - `p1` - **IF** the declaration is `secret`-classified → **RETURN** `400` with reason `SecretNotCloneable`; copying a secret reference would couple the target to the source credential's lifecycle - `inst-vw-clone-4`
5. [x] - `p1` - Resolve the effective value at the source through the Value Resolver - `inst-vw-clone-5`
6. [x] - `p1` - Invoke commit one change at the target with that value, guarded on `If-Match`; **IF** rejected → **RETURN** its outcome - `inst-vw-clone-6`
7. [x] - `p1` - **RETURN** `200` with `old_value`, `new_value`, `scope` and the new `etag` - `inst-vw-clone-7`

### Remove a Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-remove`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The scope's own row removed; resolution falls back exactly as after a revert

**Error Scenarios**:
- The write gates refuse, or `If-Match` is absent or stale

**Steps**:
1. [x] - `p1` - Actor sends DELETE /settings-service/v1/settings/{key}/value?tenant={tenant_id} with `If-Match` and the step-up token where required - `inst-vw-rm-1`
2. [x] - `p1` - Invoke the write gate order; **IF** any gate refuses → **RETURN** its refusal - `inst-vw-rm-2`
3. [x] - `p1` - Invoke commit one change as a removal of the scope's own row, guarded on `If-Match`; **IF** rejected → **RETURN** its outcome - `inst-vw-rm-3`
4. [x] - `p1` - **RETURN** `200` with the resulting effective value and source; declaration removal is a separate, immediate soft-delete and is not reachable here - `inst-vw-rm-4`

### Report Cascading Impact

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-value-writes-impact`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The descendants whose effective value would change, current against new, bounded, with the total and whether the report was truncated; informational, never blocking a write

**Error Scenarios**:
- The caller may not read the setting, or the target is outside its subtree

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/settings/{key}/impact?tenant={tenant_id}&limit={n} with the candidate value - `inst-vw-imp-1`
2. [x] - `p1` - Authorize `read` on the setting's key and confirm the target is within the caller's subtree; **IF** not → **RETURN** `403` - `inst-vw-imp-2`
3. [x] - `p1` - DB: SELECT the declaration; **IF** none or hidden → **RETURN** `404`; **IF** the scope class is not `cascading` → **RETURN** `200` with an empty report, since nothing below inherits - `inst-vw-imp-3`
4. [x] - `p1` - Invoke the bounded impact walk - `inst-vw-imp-4`
5. [x] - `p1` - **RETURN** `200` with `changed`, `total_changed`, `scanned` and `truncated` - `inst-vw-imp-5`

## 3. Processes / Business Logic (CDSL)

### Write Gate Order

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-value-writes-gates`

**Input**: The authenticated caller, the setting key, the target tenant, and whether the caller is a service principal as its security context identifies it

**Output**: Permission to proceed to the commit, or the first refusal

**Steps**:
1. [x] - `p1` - Authorize `write` on the setting's key through the `PolicyEnforcer` PEP; **IF** deny or cannot be obtained → **RETURN** `403` without consulting step-up - `inst-vw-gate-1`
2. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** the caller's effective access is `hidden` → **RETURN** `404` - `inst-vw-gate-2`
3. [x] - `p1` - **IF** the declaration is retired → **RETURN** the distinct retired outcome; a retired setting takes no value - `inst-vw-gate-3`
4. [x] - `p1` - **IF** the declaration requires step-up **AND** the caller is a service principal → **RETURN** `403` before any validation; a setting that needs a person to confirm it is by definition not one a machine may set - `inst-vw-gate-4`
5. [x] - `p1` - **IF** the declaration requires step-up → invoke step-up verification; **IF** it fails → **RETURN** `401` carrying the RFC 9470 challenge, `WWW-Authenticate: Bearer error="insufficient_user_authentication"` with `max_age` set to the freshness window and `acr_values` where an assurance level is required, so the client learns what to ask the provider for - `inst-vw-gate-5`
6. [x] - `p1` - Confirm through the tenant resolver that the target is the caller's own tenant or a descendant that is not standalone; **IF** not → **RETURN** `403` - `inst-vw-gate-6`
7. [x] - `p1` - **IF** the scope class is `global` **AND** the target is not the root tenant → **RETURN** `409`; nobody, platform administrator included, writes a tenant-scoped value for a `global` setting - `inst-vw-gate-7`
8. [x] - `p1` - **IF** the caller is a tenant caller **AND** its **own** effective access for the setting is not `overridable` → **RETURN** `403`; the target's access does not restrict an authorized ancestor writing there - `inst-vw-gate-8`
9. [x] - `p1` - **RETURN** permission to commit - `inst-vw-gate-9`

### Step-Up Verification

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-value-writes-step-up`

**Input**: The presented step-up token, the session's subject, and the configured freshness window and required assurance

**Output**: Verified, or the reason the token does not prove a recent re-authentication

**Steps**:
1. [x] - `p1` - **IF** no token is presented → **RETURN** not verified - `inst-vw-su-1`
2. [x] - `p1` - Verify the token's signature against the identity provider's published JWKS, fetched and cached in the background; **IF** it does not verify → **RETURN** not verified - `inst-vw-su-2`
3. [x] - `p1` - **IF** the token's `sub` is not the current session's subject → **RETURN** not verified; one person's ceremony does not confirm another's write - `inst-vw-su-3`
4. [x] - `p1` - **IF** `auth_time` is absent, or older than the freshness window — deployment-configured and never longer than five minutes → **RETURN** not verified; this is the claim that separates a re-authenticated token from the morning's session - `inst-vw-su-4`
5. [x] - `p1` - **IF** the deployment requires an assurance level or method **AND** `acr` or `amr` does not meet it → **RETURN** not verified - `inst-vw-su-5`
6. [x] - `p1` - Count the outcome on `settings_step_up_total` by operation and result, and **RETURN** verified; the provider was not called - `inst-vw-su-6`

### Commit One Change

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-value-writes-commit`

**Input**: A declaration, a target tenant, the new value or a removal, the `If-Match` tag the caller presented, the actor and the change set id

**Output**: The committed change with its old and new value and new tag, or the reason it stored nothing

**Steps**:
1. [x] - `p1` - **IF** the change carries a value → validate it through the Type Validator against `value_type_id`, including the size cap and numeric canonicality; **IF** invalid → **RETURN** rejected with field-level detail, nothing stored - `inst-vw-commit-1`
2. [x] - `p1` - Begin one transaction for this change alone; a request of several changes never shares one - `inst-vw-commit-2`
3. [x] - `p1` - DB: SELECT the scope's own row for the declaration, if any, and compute its value state tag; **IF** the presented tag does not match → **RETURN** rejected `412`, nothing stored, the stored value being the other writer's - `inst-vw-commit-3`
4. [x] - `p1` - **IF** the declaration has the `secret` trait **AND** the change carries a value → hand the plaintext to the Secret Manager port and take back a `secret_ref`; **IF** nothing is bound to the port → **RETURN** rejected `503`; plaintext is never written to `value` - `inst-vw-commit-4`
5. [x] - `p1` - DB: INSERT or UPDATE setting_values for `(declaration_id, tenant_id)` with `value` or `secret_ref`, the denormalized `data_classification`, `set_by`, `last_change_at` now, and `needs_review` cleared — a valid re-set or a removal clears the flag — or DELETE the row for a removal; the unique index guards the insert of a first row so two first writers cannot both land - `inst-vw-commit-5`
6. [x] - `p1` - Invoke the audit sink's append in the same transaction with the pre-image and post-image masked by classification, the operation — `create`, `change`, `revert` or `remove` — the actor, the request id and the change set id; **IF** the append fails → roll back and **RETURN** rejected `503` - `inst-vw-commit-6`
7. [x] - `p1` - Commit; **IF** the commit fails → **RETURN** rejected `503` with nothing stored - `inst-vw-commit-7`
8. [x] - `p1` - Evict the local cache for the key at the target, key-wide when the scope class is `cascading`, so descendants re-resolve lazily - `inst-vw-commit-8`
9. [x] - `p1` - Publish `event_value_changed` through the Change Publisher port, and on a rejection `event_value_change_failed` with the reason, so a failed change is a durable notification; count the outcome on `settings_value_writes_total` - `inst-vw-commit-9`
10. [x] - `p1` - **RETURN** the old value, the new value, the scope and the new tag - `inst-vw-commit-10`

### Value State Tag

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-value-writes-etag`

**Input**: The target scope's own value row, or its absence

**Output**: The ETag a write at that scope must present

**Steps**:
1. [x] - `p1` - **IF** the scope has its own row → derive the tag from the row's normalized UTC `last_change_at` - `inst-vw-etag-1`
2. [x] - `p1` - **ELSE** derive an absent-state tag, stable for the pair and distinct from every row tag, so a write may create the first row only against the caller's knowledge that none existed - `inst-vw-etag-2`
3. [x] - `p1` - **RETURN** the tag; the administrative read returns this same tag in `ETag` for the requested scope, distinct from the recency `last_change_at` in its body, which is the leak-safe maximum over the declaration and the resolved row and may belong to an ancestor - `inst-vw-etag-3`

### Bounded Impact Walk

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-value-writes-impact`

**Input**: A `cascading` declaration, the requesting scope, the candidate value, and a `limit`

**Output**: The first `limit` affected descendants in traversal order, the total count, the number scanned, and whether the report was truncated

**Steps**:
1. [x] - `p1` - Clamp `limit` to its default of one hundred when absent and to five hundred at most - `inst-vw-imp-walk-1`
2. [x] - `p1` - Walk the requesting scope's descendants breadth-first through the tenant resolver, stopping at a node budget of five thousand scanned - `inst-vw-imp-walk-2`
3. [x] - `p1` - **FOR EACH** descendant → **IF** it is standalone or below a standalone tenant → skip it, counting it neither in the list nor in the total, since a bare count still discloses that it exists and differs - `inst-vw-imp-walk-3`
4. [x] - `p1` - Resolve the descendant's current effective value and the value it would have under the candidate; **IF** they differ → count it, and record it while the list holds fewer than `limit` entries - `inst-vw-imp-walk-4`
5. [x] - `p1` - **RETURN** the list in traversal order without ranking, `total_changed`, `scanned`, and `truncated` when either the budget or `limit` was hit; a truncated report reads as "at least this many" and never blocks the write - `inst-vw-imp-walk-5`

## 4. States (CDSL)

Not applicable. A value has no lifecycle of its own beyond being present or absent at a scope; the review state a write clears belongs to `SettingValue` and is modelled in entry 2.4.

## 5. Definitions of Done

### Value Writer Operations

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-operations`

The system **MUST** expose validate, set, batch, revert, clone, remove and impact over `/settings-service/v1/settings/{key}` as the design's REST surface specifies, with `tenant` naming the target and defaulting to the caller's own tenant, every write targeting the caller's own tenant or a descendant within its subtree, and every value operation taking effect when the caller performs it — no pending state and no separate activation step.

**Implements**:
- `cpt-cf-settings-service-flow-value-writes-validate`
- `cpt-cf-settings-service-flow-value-writes-set`
- `cpt-cf-settings-service-flow-value-writes-batch`
- `cpt-cf-settings-service-flow-value-writes-revert`
- `cpt-cf-settings-service-flow-value-writes-clone`
- `cpt-cf-settings-service-flow-value-writes-remove`
- `cpt-cf-settings-service-flow-value-writes-impact`

**Constraints**: `cpt-cf-settings-service-constraint-effective-on-next-read`

**Touches**:
- API: `POST /settings-service/v1/settings/{key}/validate`
- API: `PUT /settings-service/v1/settings/{key}/value`
- API: `POST /settings-service/v1/settings/batch`
- API: `POST /settings-service/v1/settings/{key}/value/revert`
- API: `POST /settings-service/v1/settings/{key}/value/clone`
- API: `DELETE /settings-service/v1/settings/{key}/value`
- API: `GET /settings-service/v1/settings/{key}/impact`
- Entities: `SetResult`, `ValidationReport`, `ImpactReport`

### Two Gates, in Order

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-gates`

Authorization **MUST** be decided before step-up is consulted, and an unauthorized caller **MUST** be refused without it. A declaration that requires step-up **MUST** refuse a service principal before validation, **MUST** require an interactive caller's fresh step-up token, and **MUST** answer a missing or stale token with `401` and the RFC 9470 challenge. A tenant-scoped write to a `global` setting **MUST** be refused, and a tenant caller **MUST** be refused unless its own effective access is `overridable`, the target's access never restricting an authorized ancestor.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-gates`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`, `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- Entities: `PolicyEnforcer`, `StepUpVerifier`, `TenantAccess`

### Step-Up Verifier Port and Default Binding

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-step-up-verifier`

The system **MUST** verify step-up through the gear's `StepUpVerifier` port, whose default binding checks the presented token locally — signature against the identity provider's JWKS, `sub` against the session, `auth_time` within the configured freshness window of at most five minutes, and `acr`/`amr` against the required assurance — without calling the provider on the write path. The JWKS endpoint and the freshness window **MUST** be deployment configuration loaded at gear init. No binding that cannot fail **MUST** be reachable outside the test harness, and with no binding bound every write to a declaration that requires step-up **MUST** refuse while reads keep serving.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-step-up`

**Constraints**: `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- Entities: `StepUpVerifier`, gear configuration

### Per-Change Atomicity

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-atomicity`

Every change **MUST** commit in its own transaction together with its audit record and, for a secret, the `secret_ref` the Secret Manager returned, so a value is never live without a record of who set it, and a change that fails **MUST** store nothing while changes already committed stay committed. No transaction **MUST** span more than one change.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-commit`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- DB Table: `setting_values`, `audit_records`
- Entities: `SettingValue`, `AuditRecord`

### Stale-Write Rejection

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-stale`

Every write **MUST** be guarded on the tag the caller presents: the target scope's own row `last_change_at` when a row exists, or the absent-state tag when none does. A value that moved in between **MUST** be refused `412` and store nothing, two writers racing to create a first row **MUST** leave exactly one row and one audit record, and a resubmission after a lost response **MUST** either land, because the first did not, or be refused `412`, because it did.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-etag`
- `cpt-cf-settings-service-algo-value-writes-commit`

**Constraints**: `cpt-cf-settings-service-constraint-optimistic-concurrency`

**Touches**:
- DB Table: `setting_values`

### Commit, Evict, Publish

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-ordering`

A change **MUST** count as set only once it is durably committed, and the order after the commit **MUST** be: evict the local cache — key-wide for a `cascading` setting — then publish through the Change Publisher port. No consumer **MUST** be able to observe an invalidation or a change event for a value that is not yet stored. A valid re-set or a removal **MUST** clear the row's `needs_review` flag.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-commit`

**Constraints**: `cpt-cf-settings-service-constraint-effective-on-next-read`

**Touches**:
- Entities: cache entry, Change Publisher

### Bulk Set

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-batch`

A batch **MUST** carry at most five hundred changes, **MUST** verify step-up once for the request when any target requires it, **MUST** evaluate and commit each change on its own with no atomicity across changes, and **MUST** answer with one entry per change carrying the old value, the new value, the scope, and success or the error that rejected it.

**Implements**:
- `cpt-cf-settings-service-flow-value-writes-batch`

**Touches**:
- API: `POST /settings-service/v1/settings/batch`

### Bounded, Non-Blocking Impact

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-impact`

The impact report **MUST** walk the target's descendants breadth-first under a node budget of five thousand, **MUST** return the first `limit` changed descendants — default one hundred, at most five hundred — in traversal order together with the total count and a truncation flag, **MUST** omit standalone descendants from both the list and the count, and **MUST NOT** block a write however large the report.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-impact`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/impact`
- Entities: `ImpactReport`

### Secret Values Route Through the Secret Manager

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-secret-routing`

A write to a `secret`-trait declaration **MUST** hand the plaintext to the Secret Manager port and persist only the reference it returns, **MUST** be refused as unavailable while nothing is bound to that port, and **MUST** never write plaintext to `value`. A clone of a `secret`-classified setting **MUST** be refused as `SecretNotCloneable`.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-commit`
- `cpt-cf-settings-service-flow-value-writes-clone`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- DB Table: `setting_values`
- Entities: Secret Manager port

### Write Outcomes Are Observable

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-value-writes-observability`

Every committed change **MUST** publish `event_value_changed` and every rejected one `event_value_change_failed` with its reason, and the service **MUST** expose `settings_value_writes_total` by result, `settings_value_write_failure_ratio` for the platform dashboards, and `settings_step_up_total` by operation and result.

**Implements**:
- `cpt-cf-settings-service-algo-value-writes-commit`
- `cpt-cf-settings-service-algo-value-writes-step-up`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- Entities: Change Publisher, metrics

## 6. Acceptance Criteria

- [x] `validate` with an invalid value reports the violations with field-level detail, stores nothing, and emits no audit record; two identical calls return the same report
- [x] `validate` reports the current effective value and its source, and for a `cascading` setting a paged list of affected descendants
- [x] An unauthorized caller holding a valid step-up token is refused `403` without the token being verified
- [x] An authorized caller without step-up on a declaration that requires it receives `401` with `WWW-Authenticate: Bearer error="insufficient_user_authentication"` and `max_age`, and nothing is stored
- [x] A token whose `auth_time` is older than the freshness window, whose `sub` is another subject, or whose signature does not verify against the JWKS is refused; a fresh token is accepted, and the provider is not called
- [x] A service principal writing a declaration with `requires_step_up = true` is refused `403` before validation; on a declaration with the flag clear the value is committed with its audit record and no step-up is asked for
- [x] A write to a `global` setting at a tenant scope is refused, at the root tenant it succeeds
- [x] A `read_only` tenant's write is refused, an `overridable` ancestor's write at that tenant succeeds and the value is stored at the descendant
- [x] A write to a tenant outside the caller's subtree, or to a standalone descendant, is refused `403`
- [x] A valid set stores the value, and a subsequent read returns it with the `etag` the write returned
- [x] An invalid value is refused `400` with field-level detail and nothing is stored
- [x] A set whose `If-Match` is stale is refused `412`, nothing is stored, and the stored value is the other writer's
- [ ] Of N concurrent sets presenting the same tag exactly one commits, the rest return `412`, and exactly one audit record exists for the stored change
- [ ] Two concurrent first writes at a scope with no row leave exactly one row
- [x] A fault injected between the value write and the audit append leaves neither behind
- [x] After a set, the local cache no longer holds the key at the target, and for a `cascading` setting holds it at no scope
- [x] A batch of mixed changes stores the valid ones, reports the invalid one with its error, and answers with one entry per change; a batch of more than five hundred changes is refused `400`
- [ ] A batch verifies step-up once, and a failed verification stores nothing
- [x] A revert at a tenant scope returns the nearest-ancestor fallback and at the root tenant the Schema Default, which is unchanged
- [ ] A clone stores the source's effective value at the target with no continuing link, is refused `403` when the caller may not read the source, and is refused `SecretNotCloneable` on a secret setting
- [x] A write to a secret-trait declaration with no Secret Manager bound is refused as unavailable, and no plaintext appears in `setting_values`
- [x] A valid set clears `needs_review` on the row
- [ ] The impact report omits standalone descendants from its list and its total, honours `limit` between one and five hundred, stops at the node budget with `truncated` set, and never blocks the write
- [x] A committed change publishes `event_value_changed` and a rejected one `event_value_change_failed`; `settings_value_writes_total` and `settings_step_up_total` count both outcomes
- [x] With no `StepUpVerifier` bound, reads keep serving and every write to a declaration that requires step-up refuses
