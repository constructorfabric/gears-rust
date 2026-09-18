---
status: accepted
date: 2026-09-18
---

Created:  2026-09-18 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0010: Six Actions on the Credential Type; the Type Is the Only Scope Axis

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The six actions](#the-six-actions)
  - [Permissions as GTS instances, evaluated on the concrete type](#permissions-as-gts-instances-evaluated-on-the-concrete-type)
  - [Why there is no `category`](#why-there-is-no-category)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep `category` as a second scope axis](#keep-category-as-a-second-scope-axis)
  - [Type as the only scope axis (CHOSEN)](#type-as-the-only-scope-axis-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-type-scoped-authorization`

## Context and Problem Statement

The shipped resource type `gts.cf.core.credstore.secret.v1~` carries three actions (`read`/`write`/`delete`). [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) split reads and writes into a record half and a secret half, which needs six actions somewhere; and the shipped, editable `category` field was a second, informal scope axis alongside the type. Two questions: what scope, besides the type, may a permission target — and does an editable field on the record ever change which grants apply to it?

## Decision Drivers

- **D1** — enumerating, reading metadata, reading a secret, and their write counterparts are six distinct, separately grantable privileges.
- **D2** — no metadata write may move a credential from one reader's grant into another's (`fr-override-type-consistency` depends on this holding).
- **D3** — the PDP resource must be resolvable before authorization runs, from data the row already carries.

## Considered Options

- **Status quo** — the type plus an editable `category` field as a second, informal scope axis.
- **Chosen** — the type alone is the scope axis; `category` is removed.

## Decision Outcome

**Chosen: type alone.** The GTS base type is renamed `gts.cf.core.credstore.secret.v1~` → `…credential.v1~` ([ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)), and it carries six actions instead of three: `list`, `read`, `write`, `delete` on the record; `read_secret`, `write_secret` on the secret. There is no synonym for the old `read` — the rename means no shipped grant matches any new operation, so the break is structural rather than a rule reviewers police.

### The six actions

Plain verbs follow the platform convention for the entity; the `_secret` suffix follows the compound-action pattern (`set_reaction`) and names the secret, whether reached through the item's own field or through `$select` on the collection. `list`/`read` and `read_secret` stay distinct even inside one response: a caller holding only `read_secret` and naming an administrative field is refused exactly as for that field alone (ADR-0004). Two of the six define no role alone in practice — `read` is the floor the others imply, `delete` rides with `write` — but stay separate atoms for audit and for downward grants.

### Permissions as GTS instances, evaluated on the concrete type

A permission is a GTS instance, `gts.cf.toolkit.authz.permission.v1~cf.core.credstore.<name>.v1`, with `resource_type` set to the base type or a concrete descendant. The PDP evaluates against the credential's **full concrete type**, resolved before the call (D3), so a policy can target any registered type without a base-type gate. On the collection this becomes one evaluation per distinct type present among candidate rows, folding into a `secret_type_uuid IN (…)` clamp — the permitted set computed from these per-type decisions ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)). **Override-type-consistency is what makes that clamp sound**: an override must carry the type of the credential it overrides (`fr-override-type-consistency`), so `secret_type_uuid` is invariant across one reference's chain, and a clamp over it can only keep or drop a reference's whole group — never shift which row wins.

### Why there is no `category`

With `category` removed, the type is the entire purpose axis: an application granted `read_secret` on a subtype receives exactly that subtype's credentials, and a consumer that needs "its own" credentials declares a derived type rather than filing records under a mutable label. Since `type` is immutable after create (D2), no write can move a credential between grants the way an editable `category` could — the escalation path is closed structurally, not by convention.

### Consequences

- A permission's resource type accepts GTS wildcard patterns (e.g. `gts.cf.core.credstore.credential.*`); a wildcard on the base type is an operator's tool, not an application's, since every subtype registered later under it is granted the moment it exists.
- A bare shape type (`api_key`, `generic`) cannot be scoped per purpose this way — a purpose needs its own subtype.
- Registering a custom type needs no credstore release, and is granted immediately under a matching wildcard — the dangerous direction for a secret store, so grants on secret types name concrete types or an explicit set in practice.

### Confirmation

- E2E: an application granted `read_secret` on one type receives exactly that type's credentials for a `$filter` scoped to it, and an empty result for a type it is not granted.
- E2E: a metadata write that would change a record's type is refused unconditionally, not only when it would create a chain mismatch.
- Contract: no permission targets `category`; the field does not exist on the wire or in storage.

## Pros and Cons of the Options

### Keep `category` as a second scope axis

- Good: lets an operator regroup credentials without re-registering a type.
- Bad: an editable field deciding who may read a secret is a live escalation path — change `category`, inherit a different grant (D2).
- Rejected.

### Type as the only scope axis (CHOSEN)

- Good: immutable after create, so no write can move a credential between grants (D2).
- Good: one axis to reason about in a policy, one axis the collection's type clamp has to stay sound for (D3).
- Bad: regrouping credentials for a new purpose means declaring a new subtype and reissuing under it, not editing a field.

## More Information

Full permission catalog and example policies: DESIGN §5.4, §4.4.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §5.4, §4.4
- `cpt-cf-credstore-fr-authz-action-split` — the six actions this ADR defines.
- `cpt-cf-credstore-fr-override-type-consistency` — the invariant that makes the type-scoped clamp sound.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) (the type rename) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) (the write half of the split).
- Depended on by [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) for the per-type collection clamp.
