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
  - [Permissions as GTS instances, evaluated on the concrete type](#permissions-as-gts-instances-evaluated-on-the-concrete-type)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-type-scoped-authorization`

## Context and Problem Statement

The shipped resource type `gts.cf.core.credstore.secret.v1~` has three actions (`read`/`write`/`delete`). [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) split reads and writes into a record half and a secret half, so six actions are needed. The shipped, editable `category` field was a second, informal scope axis next to the type. What may a permission target besides the type, and may an editable field ever change which grants apply to a record?

## Decision Drivers

- **D1** — enumerating, reading metadata, reading a secret, and their write counterparts are six separately grantable privileges.
- **D2** — no metadata write may move a credential from one reader's grant into another's (`fr-override-type-consistency` depends on this).
- **D3** — the PDP resource is resolvable before authorization runs, from data the row already carries.

## Considered Options

- **Status quo** — the type plus an editable `category` as a second scope axis.
- **Chosen** — the type alone; `category` is removed.

## Decision Outcome

**Chosen: type alone.** The GTS base type is renamed `gts.cf.core.credstore.secret.v1~` → `…credential.v1~` ([ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)) and carries six actions: `list`, `read`, `write`, `delete` on the record; `read_secret`, `write_secret` on the secret. There is no synonym for the old `read`: after the rename no shipped grant matches any new operation, so the break is structural. Plain verbs follow the platform convention; the `_secret` suffix follows the compound-action pattern (`set_reaction`) and names the secret, whether reached through the item's field or through `$select` on the collection. `read` is the floor the other actions imply and `delete` rides with `write` in practice, but both stay separate atoms for audit and downward grants.

With `category` gone, the type is the whole purpose axis: an application granted `read_secret` on a subtype receives exactly that subtype's credentials, and a consumer that needs "its own" credentials declares a derived type instead of filing records under a mutable label. `type` is immutable after create (D2), so no write can move a credential between grants.

### Permissions as GTS instances, evaluated on the concrete type

A permission is a GTS instance `gts.cf.toolkit.authz.permission.v1~cf.core.credstore.<name>.v1` whose `resource_type` is the base type or a concrete descendant. The PDP evaluates against the credential's **full concrete type**, resolved before the call (D3), so a policy can target any registered type without a base-type gate. On the collection this is one evaluation per distinct type among the candidate rows, folded into a `secret_type_uuid IN (…)` clamp ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)). **Override-type-consistency makes the clamp sound**: an override carries the type of the credential it overrides (`fr-override-type-consistency`), so `secret_type_uuid` is invariant along one reference's chain and the clamp can only keep or drop a reference's whole group, never change which row wins.

### Consequences

- A permission's resource type accepts GTS wildcards (e.g. `gts.cf.core.credstore.credential.*`). A wildcard on the base type is an operator's tool, not an application's: every subtype registered later is granted the moment it exists, so grants on secret types name concrete types or an explicit set in practice.
- A bare shape type (`api_key`, `generic`) cannot be scoped per purpose; a purpose needs its own subtype. Registering one needs no credstore release.

### Confirmation

- E2E: an application granted `read_secret` on one type receives exactly that type's credentials for a `$filter` scoped to it, and an empty result for a type it is not granted.
- E2E: a metadata write that would change a record's type is refused unconditionally, not only when it would create a chain mismatch.
- Contract: no permission targets `category`; the field exists neither on the wire nor in storage.

## Pros and Cons of the Options

- **Status quo** — Good: an operator can regroup credentials without registering a type. Bad: an editable field deciding who may read a secret is a live escalation path — change `category`, inherit a different grant (D2).
- **Chosen** — see Decision Outcome and Consequences.

## More Information

Permission catalog and example policies: DESIGN §5.4, §4.4.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §5.4, §4.4
- `cpt-cf-credstore-fr-authz-action-split`, `cpt-cf-credstore-fr-override-type-consistency`.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); depended on by [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md).
