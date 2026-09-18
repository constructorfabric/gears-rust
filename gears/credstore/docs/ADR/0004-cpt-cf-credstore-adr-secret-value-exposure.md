---
status: accepted
date: 2026-09-18
---

Created:  2026-09-08 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0004: Credential: Metadata with a Selectable Secret

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Read actions follow the projection](#read-actions-follow-the-projection)
  - [Naming](#naming)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-secret-value-exposure`

## Context and Problem Statement

Shipped: `GET /credstore/v1/secrets/{ref}` always returns the secret, and there is no collection read (DESIGN §4.4, "no LIST"). New readers must not see secrets — catalogue and audit views, an administrator who rotates without reading — and some need several secrets in one round-trip. Two questions: is the addressable entity the **credential record** or the **secret**, and how is a secret requested without the read becoming a bulk-disclosure primitive?

## Decision Drivers

- **D1** — correctness over continuity with the shipped shape.
- **D2** — enumerating, reading metadata and reading a secret are distinct privileges; one PDP evaluation per distinct type.
- **D3** — refusal is the canonical 404, per item in bulk too; a bulk secret read never exceeds the caller's scope and is never paginated.
- **D4** — a secret-blind writer gets a CAS validator from a response it may read.
- **D5** — `$select`, not the path, decides disclosure: one address serves every projection.
- **D6** — one schema per address: `$select` narrows fields, it never forks the schema.

## Considered Options

Axis A, resource model: **A1** `secrets` collection, metadata at a `/meta` suffix; **A2** `credentials` collection, `secret` a `$select` field of the item; **A3** `secrets` collection, secret in a `value` field.

Axis B, requesting the secret: **B1** own sub-resource address; **B2** opt-in query parameter; **B3** opt-in header; **B4** present if and only if permitted; **B5** carried by the paginated collection; **B6** `$select=secret`, on the item and on the collection alike.

## Decision Outcome

**Chosen: A2 + B6** (D6 and D5 decide; D2 and D3 bound the bulk read). The entity is the **credential**: GTS type `gts.cf.core.credstore.credential.v1~`, collection `credentials`, path key `{ref}`. `GET /credentials/{ref}` and `GET /credentials` return one item shape; `secret` is present only when `$select` names it. `GET /credentials/{ref}/secret` is withdrawn in favour of `GET /credentials/{ref}?$select=reference,type,expires_at,secret`. Writes are decided in [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md).

### Read actions follow the projection

| `$select` names | Point read | Collection |
|---|---|---|
| record fields only, or nothing | `read` | `list` |
| `secret`, at most with `reference`, `type`, `expires_at` | `read_secret` | `read_secret` per distinct type |
| `secret` plus any other field | `read` + `read_secret` | `list` + `read_secret` |

`reference`, `type`, `expires_at` are readable under either action because a secret is unusable without them. The action is evaluated once, on the concrete type, before the response is assembled. Denial is 404 on the point read and an omitted item on the collection. Endpoint table, preconditions, codes, examples: DESIGN §4.3.2.

### Naming

"Credential" is the typed, tenant-scoped entry; "secret" is its value. The collection follows the record (platform vocabulary `credentials-storage`; no "`GET /secrets` returns no secrets"). The GTS base type renames `…secret.v1~` → `…credential.v1~`, so no shipped grant matches a new operation; `secret_type_uuid` and every derived type follow (a constant rename now, a migration once rows exist — DESIGN §8). `{ref}` stays the caller-chosen `SecretRef`; the row UUID never appears in a response, so `ETag` is the only CAS validator source.

### Consequences

- A default read never carries a secret: the default projection is the record fields.
- `$select`, per-item `read_secret`, a collection cap and an audit selector on `$select` replace the address as the disclosure boundary.
- Type is the only scope axis; no metadata write moves a credential between grants (`fr-override-type-consistency`).
- Cost: every HTTP consumer of the secret changes its request, not only its URL (D1); `Cache-Control: no-store` is load-bearing.

### Confirmation

- Contract: `Credential.secret` is optional, present only when selected, absent (not empty) otherwise; one schema for both reads.
- E2E: a role with `read`/`write_secret` but not `read_secret` never receives `secret`; `$select` naming `secret` plus a field outside the allowlist → 400.

## Pros and Cons of the Options

- **A1, A3** — Bad: the entity URL returns the payload by default and list/point items differ in shape (D6); A3 also names the record "secret" and the secret "value".
- **A2 (chosen)** — Good: one shape; the metadata-only `ETag` reaches a secret-blind writer (D4).
- **B1** — Good: privileges, audit and caching align with paths. Rejected although the first draft chose it: `$select` gives the same guarantees, and the collection needed the projection anyway.
- **B2** — Bad: two schemas at one address (D6). **B3** — Bad: invisible to path policy and access logs; caching needs `Vary`. **B4** — Bad: a revoked grant returns a thinner 200, not a refusal; two PDP evaluations (D2, D3). **B5** — Bad: paginated, filterable and secret-bearing is a walkable dump (D3).
- **B6 (chosen)** — Good: one round-trip for "everything I may read": capped, unpaginated, per-item `read_secret`, refused items omitted. Bad: the path alone no longer shows disclosure; `$select` plus the audit selector replace it.

## More Information

DESIGN §4.3.2 (endpoints, examples), §4.4 (PDP wiring).

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §4.4
- `cpt-cf-credstore-fr-credential-record`, `cpt-cf-credstore-fr-get-credential`, `cpt-cf-credstore-fr-list-credentials`, `cpt-cf-credstore-fr-read-secret`, `cpt-cf-credstore-fr-authz-action-split`.
- Builds on [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md); built on by [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md), [ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md), [ADR-0010](0010-cpt-cf-credstore-adr-type-scoped-authorization.md); depends on [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md).
