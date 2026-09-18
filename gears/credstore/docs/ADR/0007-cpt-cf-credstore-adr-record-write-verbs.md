---
status: accepted
date: 2026-09-18
---

Created:  2026-09-18 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0007: Two Write Verbs on One Address: PUT Replaces, PATCH Merges

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [PUT: whole replace, secret tri-state](#put-whole-replace-secret-tri-state)
  - [PATCH: merge, never creates](#patch-merge-never-creates)
  - [No-op rule, and why a secret write always bumps](#no-op-rule-and-why-a-secret-write-always-bumps)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-record-write-verbs`

## Context and Problem Statement

[ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) makes the record and its secret one entity for reads. Writes need the same answer: does the secret get its own write address, or does it travel with the record's verbs? Either way, a secret-blind administrator must edit metadata without supplying or destroying a secret, a record and its secret must be created atomically, and suppression (arming `fallback: none` and clearing the secret) must leave no window in which the wrong value is served.

## Decision Drivers

- **D1** — atomic create: record and secret land together.
- **D2** — a secret-blind edit never resends or destroys the secret.
- **D3** — no equality oracle: a `write_secret` holder without `read_secret` cannot detect "unchanged" by watching the version.
- **D4** — one resource, one validator: no second representation for preconditions.

## Considered Options

- **C1** — the secret has its own sub-resource with `PUT`/`DELETE`.
- **C2** — no secret write address; a secret is written only inside `PUT` (full replace) or `PATCH` (merge) on the record.

## Decision Outcome

**Chosen: C2.** The record has two write verbs, `PUT` and `PATCH`; the secret has none of its own. No `POST`: `PUT` with `If-None-Match: *` is already create-only and idempotent.

### PUT: whole replace, secret tri-state

`PUT /credentials/{ref}` replaces the whole credential: `sharing` required; `type` required on create, immutable afterwards; `fallback` defaults to `inherit`, `expires_at` to none. `secret` has no default:

| `secret` in the body | Effect | Actions |
|---|---|---|
| absent | 400 `SECRET_REQUIRED` — a forgotten secret never creates or replaces a record | — |
| string | written under the [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md) protocol, atomically with the row | `write` + `write_secret` |
| explicit `null` | create → row inserted `declared`, no backend call; replace of `active` → secret removed in the same transaction as `PATCH {"secret": null}`; replace of `declared` → secret untouched | `write`; `write_secret` only when an existing secret is removed |

`declared` is therefore reached only on purpose — by this `null` or by `PATCH {"secret": null}` — never by omission.

### PATCH: merge, never creates

`PATCH /credentials/{ref}` is an RFC 7396 JSON Merge Patch (`Content-Type: application/merge-patch+json`): a present field is applied, an absent field is untouched, `secret: null` removes the secret (the record becomes `declared`). It never creates: no own record → 404. Actions follow the body — `write` for metadata keys, `write_secret` for a `secret` key (string or `null`), both when both are present, evaluated before either half is applied. So a secret-blind administrator sends `PATCH {"sharing": …}` under `write` alone, and suppression changes `fallback` and `secret` in one request ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)).

### No-op rule, and why a secret write always bumps

A body with no secret change whose metadata equals the current record is a no-op: 204, unchanged `ETag`, no version bump, validation still runs. A body that writes or removes a secret is **never** a no-op, identical bytes included: skipping the write on a fingerprint match would be an equality oracle (D3). Under immutable versions a re-write of identical bytes is just a new version — also how a client recovers a corrupted entry.

### Consequences

- Metadata is editable without touching the secret (D2); a "declarer" role (`write` without `write_secret`) can create a secret-less record or edit metadata, but never bring a new reference into being *with* a secret.
- Suppressing an inherited credential with no row of one's own is one request: `PUT` with `secret: null` creates the blocking row ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)).
- Cost: a write's actions derive from its body, not its address — the one exception to path-based authorization here (reads derive theirs from the projection, ADR-0004).

### Confirmation

- E2E: `PUT` with `If-None-Match: *` and no `secret` key → 400 `SECRET_REQUIRED`; with `secret: null` → 201, `declared`, no backend call, `write` alone suffices.
- E2E: a `write`-only caller may `PATCH {"sharing": …}` but not `{"secret": …}`, and conversely for `write_secret`-only.
- E2E: a `PUT`/`PATCH` that writes or removes a secret always bumps `version`, even for identical bytes; a metadata-only no-op never does.

## Pros and Cons of the Options

- **C1** — Good: symmetric with a per-address read (ADR-0004's rejected B1); no body inspection to authorize. Bad: create takes two requests with no atomicity, so a client that stops after the first leaves an unintended secret-less record (D1); the sub-resource has no representation for `If-Match`/`If-None-Match` to bind to (D4); windowless suppression needs an ordering rule across two requests.
- **C2 (chosen)** — Good: see Decision Outcome and Consequences. Bad: secret writes share `PUT`/`PATCH /credentials/{ref}` with metadata writes, so they cannot be rate-limited or audited by path separately.

## More Information

Precondition table (create / replace / rotate / suppress shapes) and status codes: DESIGN §4.3.2.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2
- `cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-write-secret`, `cpt-cf-credstore-fr-authz-action-split`.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md); built on by [ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md).
