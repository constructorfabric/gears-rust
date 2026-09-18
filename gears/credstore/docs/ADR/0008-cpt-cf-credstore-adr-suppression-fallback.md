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
  - [Resolution](#resolution)
  - [Suppression is one request, always](#suppression-is-one-request-always)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-suppression-fallback`

## Context and Problem Statement

A tenant that removes its own secret, or never had one, must be able to say what a reference means from here down: keep inheriting the nearest ancestor's `shared` secret (today's only behaviour), or block resolution — "we do not use this integration here" — propagated to descendants like an ordinary row. Blocking must not require holding a secret, and must work for a tenant with no row of its own.

## Decision Drivers

- **D1** — suppression is set and lifted without ever supplying a secret.
- **D2** — it is a resolution *policy* decided from the metadata row before the backend is touched — not a value, not a saga step.
- **D3** — no window: arming it on an active credential never transiently serves the old value.
- **D4** — a suppressed reference fails like an inaccessible one (canonical 404), not with a distinguishable status.

## Considered Options

- **Option 1 (CHOSEN)** — a `fallback` column (`inherit` / `none`) on the record, consulted only while the record holds no secret.
- **Option 2** — a fifth `status` value, "suppressed".
- **Option 3** — a tombstone at the secret's own address, read by the resolver.

## Decision Outcome

**Chosen: Option 1.** Every record carries `fallback`: `inherit` (default) — while the record has no secret, resolution walks on to the nearest ancestor's `shared` secret; `none` — resolution stops here with the canonical 404 for the tenant and, per `sharing`, its descendants (`inheritance: suppressed`). `fallback` is written with `PUT`/`PATCH {"fallback": …}` under `write`, like `sharing`, never under `write_secret`. While the record is `active` its own secret wins and `fallback` is stored but not consulted, so it can be armed ahead of time.

### Resolution

| Own row | `fallback` | Resolves here and below (per `sharing`) |
|---|---|---|
| `declared` | `inherit` | ancestor's secret, if any |
| `declared` | `none` | 404 — the walk stops here |
| `active` | either | own secret — `fallback` not consulted |

Candidates: `status = active OR (status = declared AND fallback = none)`. A `declared`/`none` row competes and, when nearest, wins; a winner without a secret yields 404. `shared` blocks the whole subtree, `tenant` only that tenant. A descendant reading `suppressed` learns nothing about which tenant blocked or what sits behind it — the same withholding as a plain 404.

### Suppression is one request, always

| Situation | Request | Actions |
|---|---|---|
| own row is `active` | `PATCH {"fallback": "none", "secret": null}` under one `If-Match` — one row update, no window (D3) | `write` + `write_secret` |
| no own row | `PUT {"type": …, "sharing": …, "fallback": "none", "secret": null}` with `If-None-Match: *` — inserts the row `declared`/`none` | `write` only; `write_secret` is not evaluated |
| lift without a secret | `PATCH {"fallback": "inherit"}` | `write` |
| write a secret to a suppressed record | `PATCH {"secret": …}` — the record becomes `active`/`overridden`; `fallback` stays stored | `write_secret` |

### Consequences

- Suppression composes with the write verbs of [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md) instead of adding an address or a status; a policy-only administrator arms and lifts it under `write` alone.
- The collection read ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)) treats a `declared`/`none` row as a competing, winning candidate — the one exception to "a `declared` row never competes".
- Storage: `fallback SMALLINT NOT NULL DEFAULT 1 CHECK (fallback IN (1, 2))`, the same convention as `status`/`sharing` (DESIGN §4.7).

### Confirmation

- E2E: a `declared`/`none` record resolves to 404 in its own tenant and, when `sharing: shared`, in descendants; the ancestor's record is untouched and still served outside that subtree.
- E2E: `PATCH {"fallback": "none", "secret": null}` under one `If-Match` makes the reference resolve to nothing at once; no read during or after returns the old secret.
- E2E: `PUT {"fallback": "none", "secret": null}` with `If-None-Match: *` on a reference with no own row succeeds under `write` alone, with no `write_secret` evaluation.

## Pros and Cons of the Options

- **Option 1 (chosen)** — Good: decided from the row the resolver already reads, no second source of truth (D2). Bad: a suppressed and an inaccessible reference are both a plain 404, indistinguishable on the wire — accepted (D4).
- **Option 2, a fifth `status`** — Bad: cannot express the *armed* state (active, suppression waiting for the secret to go) without a permission crossover: `write` discarding the current secret to flip the status, or `write_secret` setting a policy it should not touch. `fallback` is orthogonal to `status` for this reason.
- **Option 3, a tombstone** — Bad: the resolver picks the winner from one SQL query without reading the backend, so a tombstone would have to be mirrored into the row anyway; it would also give `write_secret` an administrative power ADR-0004 keeps out of that action.

## More Information

- The `smtp-default` three-tenant scenario (override → rotate → suppress → lift): DESIGN §6.1.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §6.1
- `cpt-cf-credstore-fr-suppression`.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); read by [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) for the `declared`/`none` winner rule.
