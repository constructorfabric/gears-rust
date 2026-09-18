---
status: accepted
date: 2026-09-18
---

Created:  2026-09-18 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0008: Suppression: `fallback` on the Tenant's Own Record

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What each combination means for resolution](#what-each-combination-means-for-resolution)
  - [Suppression is one request, always](#suppression-is-one-request-always)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A fifth `status` value](#a-fifth-status-value)
  - [A tombstone at the secret's own address](#a-tombstone-at-the-secrets-own-address)
  - [`fallback` on the own record (CHOSEN)](#fallback-on-the-own-record-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-suppression-fallback`

## Context and Problem Statement

A tenant that removes its own secret, or never had one, must be able to say what a reference means from here down: keep inheriting the nearest ancestor's `shared` secret (today's only behaviour), or block resolution outright — "we deliberately do not use this integration here", propagated to descendants exactly as an ordinary row is. Blocking must be expressible without holding a secret at all, including by a tenant with no row of its own under the reference.

## Decision Drivers

- **D1** — suppression must be settable and liftable without ever supplying a secret.
- **D2** — suppression is a resolution *policy*, decided from the metadata row alone, before the backend is ever touched — not a value, and not a saga step.
- **D3** — no window: arming suppression on an active credential must not transiently serve the old value or leave an ambiguous state.
- **D4** — a suppressed reference must fail exactly like an inaccessible one (canonical 404), not with a distinguishable status.

## Considered Options

- **Option 1 (CHOSEN)** — a `fallback` column (`inherit` / `none`) on the record, consulted only while the record holds no secret.
- **Option 2** — a fifth `status` value, "suppressed".
- **Option 3** — a tombstone written at the secret's own address, read by the resolver.

## Decision Outcome

**Chosen: Option 1.** Every record carries `fallback`: `inherit` (default) — while the record has no secret, the reference stays transparent and resolution walks to the nearest ancestor's `shared` secret; `none` — while the record has no secret, resolution stops at this row and yields the canonical 404 for the tenant and, per `sharing`, its descendants (`inheritance: suppressed`). `fallback` is set through `PUT`/`PATCH {"fallback": …}`, always under `write` — the same action as `sharing` — and never through `write_secret`: deciding what happens in a secret's *absence* is a metadata question by construction. While the record is `active` its own secret always wins and `fallback` is stored but not consulted, so it can be armed ahead of time.

### What each combination means for resolution

| Own row | `fallback` | What resolves here and below (per `sharing`) |
|---|---|---|
| `declared` | `inherit` | ancestor's secret, if any |
| `declared` | `none` | 404 — the walk stops here |
| `active` | either | own secret — `fallback` not consulted |

Resolution candidates: `status = active OR (status = declared AND fallback = none)`. A `declared`/`inherit` row never competes and never shadows; a `declared`/`none` row competes and, when nearest, wins — a winner with no secret yields 404 rather than letting the walk fall through. Propagation follows `sharing` exactly as any other row: `shared` blocks the whole subtree, `tenant` blocks only that tenant. A descendant reading `suppressed` learns that the nearest resolvable row has no secret and blocks the walk — nothing about which tenant, or what sits behind it (the same withholding a plain 404 already does).

### Suppression is one request, always

Suppressing an **active** own credential: `PATCH {"fallback": "none", "secret": null}` under one `If-Match` — `fallback` and the secret removal apply in the same row update, so there is no window in which the old secret is still served (D3). Suppressing with **no own row at all**: `PUT {"type": …, "sharing": …, "fallback": "none", "secret": null}` under `If-None-Match: *` inserts the row `declared`/`none` directly, needing only `write` — no secret was ever named, so `write_secret` is never evaluated. Writing a secret to a suppressed record is not a conflict: `PATCH {"secret": …}` makes it `active`/`overridden`, and `fallback` stays stored but stops being consulted. Lifting suppression without a secret is `PATCH {"fallback": "inherit"}` alone.

### Consequences

- Suppression composes with the write verbs of [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) rather than adding an address or a status; a policy-only administrator can arm and lift it under `write` alone.
- The collection read ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)) must treat a `declared`/`none` row as a competing, winning candidate — it is the one exception to "a `declared` row never competes".
- Storage: `fallback SMALLINT NOT NULL DEFAULT 1 CHECK (fallback IN (1, 2))` — the same SMALLINT-with-CHECK convention as `status`/`sharing` (DESIGN §4.7).

### Confirmation

- E2E: a `declared`/`none` record resolves to 404 in its own tenant and, when `sharing: shared`, in descendants; the ancestor's record is untouched and still served outside that subtree.
- E2E: `PATCH {"fallback": "none", "secret": null}` under one `If-Match` makes the reference resolve to nothing at once — no read, during or after, ever returns the old secret.
- E2E: `PUT {"fallback": "none", "secret": null}` with `If-None-Match: *` against a reference with no own row succeeds under `write` alone, with no `write_secret` evaluation.

## Pros and Cons of the Options

### A fifth `status` value

- Bad: cannot express the *armed* state (active, suppression waiting for the secret to be removed) without a permission crossover — either `write` discarding the current secret to flip the status, or `write_secret` setting a policy it has no business touching.
- Rejected: `fallback` is orthogonal to `status` for exactly this reason.

### A tombstone at the secret's own address

- Bad: the resolver reads no backend for candidate rows — one SQL query decides the winner (ADR-0004) — so anything resolution depends on must live in the metadata row already queried; a tombstone would have to be mirrored there anyway, doubling the source of truth.
- Bad: hands `write_secret` an administrative power (deciding what a reference means) that ADR-0004 keeps out of that action.
- Rejected for both reasons.

### `fallback` on the own record (CHOSEN)

- Good: orthogonal to `status`, so the armed state needs no permission crossover (D1).
- Good: decided entirely from the row the resolver already reads (D2); no second source of truth.
- Good: suppressing an active credential is one atomic merge-patch (D3).
- Bad: a suppressed reference and an inaccessible one are both a plain 404 — an operator cannot tell them apart from the wire, by design (D4).

## More Information

- The `smtp-default` three-tenant suppression scenario (override → rotate → suppress → lift) is walked step by step in DESIGN §6.1.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §6.1
- `cpt-cf-credstore-fr-suppression` — the requirement this ADR answers in full.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) (one entity) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) (the merge-patch verb suppression relies on).
- Read by [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md): the collection's reduction rule for a `declared`/`none` winner.
