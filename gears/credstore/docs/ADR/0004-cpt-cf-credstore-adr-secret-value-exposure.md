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
  - [Naming: `credentials`, and the path key](#naming-credentials-and-the-path-key)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A1 and A3: secret and metadata on separate addresses/fields](#a1-and-a3-secret-and-metadata-on-separate-addressesfields)
  - [A2: `credentials` collection, one item shape, secret under `$select` (CHOSEN)](#a2-credentials-collection-one-item-shape-secret-under-select-chosen)
  - [B1: secret only at its own address](#b1-secret-only-at-its-own-address)
  - [B2–B5: query flag, header, permission-shaped, or paginated collection](#b2b5-query-flag-header-permission-shaped-or-paginated-collection)
  - [B6: secret under `$select`, on the item and the collection (CHOSEN)](#b6-secret-under-select-on-the-item-and-the-collection-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-secret-value-exposure`

## Context and Problem Statement

Shipped: `GET /credstore/v1/secrets/{ref}` returns the secret unconditionally; there is no collection read (DESIGN §4.4: "no LIST"). New requirements need readers who must **not** see secrets — a catalogue/audit view, an administrator who rotates without reading — and readers who need several secrets at once in one round-trip. Two questions, answered together: is the addressable thing the **credential record**, secret reached through it, or the **secret**, metadata a suffix? And by what mechanism is a secret asked for, bounded rather than turned into a bulk-disclosure primitive?

## Decision Drivers

- **D1** — correctness over continuity; the shipped shape is an input to weigh, not a constraint to preserve.
- **D2** — enumerating, reading metadata, and reading a secret are distinct privileges; PDP cost stays bounded, one evaluation per distinct type.
- **D3** — a refusal is the canonical 404, per item in bulk too; a bulk secret read may never exceed the caller's scope or be paginated/walked.
- **D4** — a secret-blind writer still needs a CAS validator from some response it can legally read.
- **D5** — a read's disclosure is decided by `$select`, not by path — one address now serves every projection.
- **D6** — one shape per address: `$select` narrows which fields a response carries, it never forks the schema.

## Considered Options

**Axis A — resource modelling:**

- **A1** — `secrets` collection; secret on the item, metadata at a `/meta` suffix.
- **A2** — `credentials` collection; one item at `/credentials/{ref}`, the secret a field of it under `$select` rather than a suffix of the address.
- **A3** — `secrets` collection; metadata on the item, secret at a `value`-named field.

**Axis B — how a secret is asked for:**

- **B1** — only at its own address (a sub-resource).
- **B2** — one address, secret opted in by a query parameter.
- **B3** — one address, secret opted in by a request header.
- **B4** — one address, the secret field present if and only if permissions allow.
- **B5** — secrets carried by the paginated metadata collection.
- **B6** — secret under `$select=secret`, one sparse projection of one item shape, on the single item exactly as on the collection.

## Decision Outcome

**Chosen: A2 + B6.** The addressable entity is the **credential** (`gts.cf.core.credstore.credential.v1~`, collection `credentials`, path key `{ref}`). `GET /credentials/{ref}` and `GET /credentials` return one item shape; `secret` is a field of it, present only when `$select` names it, one reference exactly as several at once. There is no secret address of its own: `GET /credentials/{ref}/secret` is withdrawn in favour of `GET /credentials/{ref}?$select=reference,type,expires_at,secret`. How a secret is *written* is Axis C, decided separately in [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md).

The deciding arguments are D6 (`$select` projects one schema, it does not fork it) and D5 (the point read now uses exactly the mechanism the collection already needed), with D2 and D3 confirming the bound on bulk disclosure.

### Read actions follow the projection

A request naming only record fields evaluates `list` (collection) or `read` (point read); a request naming `secret` evaluates `read_secret` as well, per distinct type. The envelope fields `reference`, `type`, `expires_at` are readable under either action, since a secret is unusable without them; no other administrative field rides along with `read_secret` alone — a caller holding only `read_secret` and selecting `sharing` or `inheritance` is refused exactly as for `sharing` alone. The action is evaluated once, against the concrete type, before either representation is assembled: a denial is the canonical 404 on the point read and an omission on the collection. Full endpoint table (headers, preconditions, status codes) and request/response examples: DESIGN §4.3.2.

### Naming: `credentials`, and the path key

"Credential" is the typed, tenant-scoped entry; "secret" the value it holds — the collection follows the record, converging with the platform's `credentials-storage` vocabulary and dropping *"`GET /secrets` returns no secrets."* The GTS base type is renamed alongside, `…secret.v1~` → `…credential.v1~`: no shipped grant matches a new operation, so the authorization break is structural, not a policed convention; `secret_type_uuid` and every derived type follow (constant now, a migration once rows exist — DESIGN §8). `{ref}` stays the caller-chosen `SecretRef`, never the row UUID: the reference is the stable name a consumer hard-codes, the row id a generation id kept out of every response so the `ETag` remains the one CAS validator source.

### Consequences

- A default read can never carry a secret — structural, since the default projection is exactly the record fields, not an omission to trust.
- `$select`, per-item `read_secret`, a collection cap and an audit selector on `$select` replace the address as the disclosure boundary.
- Type is the only scope axis; no metadata write moves a credential between grants (`fr-override-type-consistency`).
- Cost: every current HTTP consumer of the secret changes its request, not only its URL (D1); `Cache-Control: no-store` becomes load-bearing rather than incidental.

### Confirmation

- Contract: `Credential` carries `secret` as optional, present only when `$select` names it, absent (not empty) otherwise — one schema for the point read and the collection.
- E2E: a role holding `read`/`write_secret` but not `read_secret` never receives `secret`; a `$select` naming a field outside the allowlist plus `secret` is refused with 400.

## Pros and Cons of the Options

### A1 and A3: secret and metadata on separate addresses/fields

- Bad (A1, secret on the item at `/secrets/{ref}`, metadata at `/meta`): the entity URL returns the payload by default, the safe view is the exception, and the list item and point item have different schemas (D6).
- Bad (A3, secret at a `value` field on `/secrets/{ref}`): the same structural issue with muddled vocabulary — "secret" names the record, "value" names the secret.

### A2: `credentials` collection, one item shape, secret under `$select` (CHOSEN)

- Good: a default read cannot carry a secret — the default projection is exactly the record fields.
- Good: one schema for the list item and the point record (D6); `ETag` reaches metadata-only readers (D4).
- Bad: renames the collection and withdraws the secret's own read address.

### B1: secret only at its own address

- Good: privileges, audit and cache policy align with paths; fixed schema per address.
- **Rejected on reflection — this ADR's first draft chose B1.** It is revisited because B1's guarantees (D6, D5) survive intact under `$select`: the client must still name `secret` before it appears, and a denial is still explicit; a second address bought nothing a client-declared projection did not already buy for the collection.

### B2–B5: query flag, header, permission-shaped, or paginated collection

- Bad (B2, query flag): one address, two response schemas — `secret` becomes optional in the generated spec (D6); superseded by B6, the same intent through the platform's own projection mechanism.
- Bad (B3, header): invisible to path-based policy, easy to miss in access logs; correct caching needs `Vary` on it.
- Bad (B4, permission-shaped): silent degradation — a revoked grant returns a thinner 200, not a refusal; two PDP evaluations on the hot path (D3, D2).
- Bad (B5, values on the paginated collection): paginated, filterable and secret-bearing at once is a walkable catalogue dump (D3); the legitimate need is served by B6, capped and unpaginated.

### B6: secret under `$select`, on the item and the collection (CHOSEN)

- Good: one round-trip for "everything I may read", reusing the collection's own `$filter`/`$select`.
- Good: every objection to B5 is answered: no pagination, capped, per-item `read_secret`, a refused item omitted.
- Bad: the path no longer distinguishes disclosure by itself; `$select` plus an audit selector on the query replace it.

## More Information

Full endpoint table, request/response examples, and PDP wiring: DESIGN §4.3.2, §4.4.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §4.4
- `cpt-cf-credstore-fr-credential-record`, `-fr-get-credential`, `-fr-list-credentials` — the one-entity model and its two reads, one item shape.
- `cpt-cf-credstore-fr-read-secret`, `-fr-authz-action-split` — `$select=secret` and the `read`/`list` vs `read_secret` split.
- Builds on [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md) (fence per returned secret); built on by [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md), [ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md), [ADR-0010](0010-cpt-cf-credstore-adr-type-scoped-authorization.md); depends on [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md).
