---
status: accepted
date: 2026-09-18
---

Created:  2026-09-18 by Constructor Tech
Updated:  2026-10-02 by Constructor Tech

# ADR-0010: Six Actions on the Credential Type; Type and Reference Are the Scope Axes

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Permissions as GTS instances; the type returned as a PDP constraint](#permissions-as-gts-instances-the-type-returned-as-a-pdp-constraint)
  - [Per-instance grants by reference](#per-instance-grants-by-reference)
  - [Type consistency along a reference's chain](#type-consistency-along-a-references-chain)
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
- **D3** — the PDP resource is known before authorization runs, without reading the row: a constant base type for an existing credential, the requested type for a create.

## Considered Options

- **Status quo** — the type plus an editable `category` as a second scope axis.
- **Chosen** — the type alone; `category` is removed.

## Decision Outcome

**Chosen: the type, plus the reference for per-instance grants; no editable field.** The GTS base type is renamed `gts.cf.core.credstore.secret.v1~` → `…credential.v1~` ([ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)) and carries six actions: `list`, `read`, `write`, `delete` on the record; `read_secret`, `write_secret` on the secret. There is no synonym for the old `read`: after the rename no shipped grant matches any new operation, so the break is structural. Plain verbs follow the platform convention; the `_secret` suffix follows the compound-action pattern (`set_reaction`) and names the secret, whether reached through the item's field or through `$select` on the collection. `read` is the floor the other actions imply and `delete` rides with `write` in practice, but both stay separate atoms for audit and downward grants.

With `category` gone, the type is the purpose axis (a single instance is addressed by its reference, below): an application granted `read_secret` on a subtype receives exactly that subtype's credentials, and a consumer that needs "its own" credentials declares a derived type instead of filing records under a mutable label. `type` is immutable after create (D2), so no write can move a credential between grants.

### Permissions as GTS instances; the type returned as a PDP constraint

A permission is a GTS instance `gts.cf.toolkit.authz.permission.v1~cf.core.credstore.<name>.v1` whose `resource_type` is the base type or a concrete descendant. An operation on an **existing** credential evaluates its action on the **base type**, once per needed action, and declares the credential type as a supported PDP property next to the tenant. The PDP matches the caller's grants — base type, concrete descendant or wildcard — and answers with the tenant constraint plus a constraint on that property: the set of credential types the grants cover for the action (as the deterministic UUIDs the row stores), omitted when they cover every type. The constraint is compiled to a predicate on the stored type column and applied to the row lookup itself, so a row of an uncovered type is simply not found, and the number of PDP calls never depends on how many types exist (hundreds are expected) or which ones a tenant holds; the data-independent refusal stays a plain PDP denial. A **create** takes its type from the request and has no row to constrain, so it evaluates the requested **concrete** type. On the collection the type constraint becomes the SQL clamp `secret_type_uuid IN (…)` ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)). A PDP that cannot express the type set is type-blind and grants every type in the tenant or denies; type-scoped grants need a PDP that answers a base-type request with the constraint. **Override-type-consistency keeps the clamp sound**: a non-private create must carry the type of the credential it overrides and of every non-private record of the same reference below it (`fr-override-type-consistency`), so `secret_type_uuid` is the same along one reference's chain of non-private records and the clamp keeps or drops a reference's whole group; `private` records are exempt and may differ in type. Where types differ (a private record, or see Type consistency below), the reduction reads the group unclamped and drops a winner outside the permitted set ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)), so a divergence costs a short page, never a wrong row.

### Per-instance grants by reference

The gear declares a second supported PDP property next to the type: the credential's **reference**. For one evaluation per action on the base type the PDP may answer with constraints on the type, the reference or both, as alternatives (OR) of conjunctions (AND): "type A, or reference x" and "type A and reference x" are both expressible, so the gear keeps the constraint structure and does not flatten it into sets. Example: application `email-sender` holds `read_secret` on `reference in ["smtp-password"]` and receives that one credential and no other.

A per-instance grant is expressed by reference, **not by record id**: a re-created record gets a new id, so an id-based grant would silently stop matching after a delete and create, while the reference names the same credential across generations. Reference values are compared as strings, exactly and case-sensitively; a UUID-shaped grant value matches the reference in its lowercase hyphenated form. The constraints are applied to the row lookup in the store, and the tenant dimension stays a gate: only a constraint that affirms the caller's own tenant counts, and any other restriction the gear cannot evaluate fails closed.

- **Create** has no row to constrain, so the PDP's constraints on the concrete type are evaluated against the request: the requested reference must be admitted, otherwise the create is refused exactly like a PDP refusal (403), before any lookup, so a refusal never discloses whether the name is taken.
- **Replace, patch and delete** look the row up under the PDP's constraints; a row the constraints do not admit is answered as a missing row.
- **Inheritance.** The decisive row of a reference is chosen from all visible rows, unclamped. If the constraints do not admit that decisive row, the point read answers a miss (404) and the collection omits the item; the read never falls through to an ancestor's value, which would serve a different credential than the one the resolution picked.

### Type consistency along a reference's chain

A non-private (`tenant` or `shared`) create is refused when the requested type differs from the type the reference resolves to for its creator among non-private records (`TYPE_MISMATCH_WITH_INHERITED`, naming that type), and when any descendant tenant of the creator, isolation barriers included, holds a non-private record of the same reference with another type in any status (`TYPE_MISMATCH_WITH_DESCENDANT`, naming neither the tenant nor the type). A `private` record is exempt: its owner alone reads it and chose its type, so it is never checked, never counted, and may carry any registered type; it is ignored when a non-private record is checked. Checking both directions closes the ordinary sequential ways to break a consumer's type contract: an override created with another type, and an ancestor creating or re-creating a name its descendants already hold with another type. It is a check at creation, not a maintained invariant: two creates of one reference in an ancestor and a descendant that overlap in time can both pass, and a tenant moved under a new parent would not be checked. The downward check discloses one bit to a caller permitted to create the type — that some descendant, possibly behind an isolation barrier, holds the name with another type — and lets descendants holding a name with one type stop their ancestor from creating it with another; both are accepted, because inheritance crosses barriers and a mismatch there is real.

### Consequences

- A permission's resource type accepts GTS wildcards (e.g. `gts.cf.core.credstore.credential.v1~*`). A wildcard on the base type is an operator's tool, not an application's: every subtype registered later is granted the moment it exists, so grants on secret types name concrete types or an explicit set in practice.
- A bare shape type (`api_key`, `generic`) cannot be scoped per purpose; a purpose needs its own subtype. Registering one needs no credstore release.
- **Grant reissuance is a pre-deployment step, not a rolling one.** Because no shipped grant matches any new action, every existing policy against `gts.cf.core.credstore.secret.v1~` (`read`/`write`/`delete`) must be re-issued against `credential.v1~`'s six actions (`read` → `read` + `list`; `write` → `write`; `delete` → `delete`; a grant that also needs the secret adds `read_secret`/`write_secret` explicitly, never inferred) before this ADR's code deploys — the same PDP policy owner (platform or tenant admin, per the grant's own scope) who issued the old grant reissues it. There is no dual-grant window: since the actions genuinely differ (three vs. six, with the secret split out), a policy engine cannot honor both simultaneously without over- or under-granting, so this ships in the same stop-the-world window as the `m0002` data migration (DESIGN §8), not as an independent rollout.

- **Deployment prerequisite.** The shipped tenant-resolver PDP plugin returns tenant constraints only; it is blind to type and reference and grants every credential in the tenant. The shipped static PDP does the same unless it is configured with property grants, which exist for development and tests. Type- and reference-scoped grants take effect in production only with a PDP that answers a base-type request with constraints on the credential-type and reference properties; a PDP that denies a base-type request for a caller holding grants on derived types only would lock that caller out.

### Confirmation

- E2E: an application granted `read_secret` on one type receives exactly that type's credentials for a `$filter` scoped to it, and an empty result for a type it is not granted.
- Unit: every operation on an existing credential issues one PDP evaluation per needed action on the base type, regardless of how many credential types the tenant holds; a type-restricted scope hides rows of other types (404/409) in the SQL lookup, and a flat PDP denial is 403 whether or not the record exists.
- E2E: a metadata write that would change a record's type is refused unconditionally, not only when it would create a chain mismatch.
- Unit: a reference constraint admits and hides rows by name in the point read, the collection (with and without `secret` selected), replace, patch, delete and the removal of a value; a create of a reference the constraint does not admit is 403; a decisive child override the constraints do not admit is a miss and the ancestor's value is never served.
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
