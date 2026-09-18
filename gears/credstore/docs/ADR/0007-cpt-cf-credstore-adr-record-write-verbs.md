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
  - [The secret-less record: reached only on purpose](#the-secret-less-record-reached-only-on-purpose)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [C1: secret sub-resource with its own `PUT`/`DELETE`](#c1-secret-sub-resource-with-its-own-putdelete)
  - [C2: `PUT` (full replace) and `PATCH` (merge) on the record (CHOSEN)](#c2-put-full-replace-and-patch-merge-on-the-record-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-record-write-verbs`

## Context and Problem Statement

[ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) merges the credential record and its secret into one entity for reads. Writes must decide the same question on the other side: does the secret get its own write address, or does it travel with the record's own write verbs? Whichever is chosen must still let a value-blind administrator edit metadata (`sharing`, `expires_at`) without ever supplying or destroying a secret, must create a record and its secret atomically, and must let suppression (arming `fallback: none` and clearing the secret) happen without a window in which the wrong value is served.

## Decision Drivers

- **D1** — atomic create: record and secret land together, never a window where a record exists without a decided secret state.
- **D2** — a value-blind edit must be expressible without resending or destroying the secret.
- **D3** — no equality oracle: a `write_secret` holder without `read_secret` must not be able to detect "unchanged" by watching the version.
- **D4** — one resource, one validator: no second representation to define a precondition against.

## Considered Options

- **C1** — the secret has its own sub-resource with its own `PUT`/`DELETE`.
- **C2** — no secret write address; a secret is written only as part of a write to the record, via `PUT` (full replace) and `PATCH` (merge).

## Decision Outcome

**Chosen: C2.** The record has two write verbs, `PUT` and `PATCH`; the secret has none of its own. There is no `POST`: `PUT` with `If-None-Match: *` is already create-only and idempotent, so a separate creation verb would name the same call twice.

### PUT: whole replace, secret tri-state

`PUT /credentials/{ref}` is a whole-credential replace — `sharing` is required, `type` is required on create and immutable after, and the optional fields take their defaults when absent (`fallback` → `inherit`, `expires_at` → none). `secret` has no default and is **tri-state**: **absent** → 400 `SECRET_REQUIRED`, so a forgotten secret never creates or replaces a record by accident; **a string** → written under the [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md) protocol, atomically with the row; **explicit `null`** → no secret is written — on create the row is inserted `declared`; on replace of an `active` row the secret is removed in the same transaction `PATCH {"secret": null}` uses; on replace of an already-`declared` row nothing about the secret changes. `write` is always required; `write_secret` is required only when `secret` is a string or a `null` that removes an existing secret — not when `null` creates or leaves a value-less row.

### PATCH: merge, never creates

`PATCH /credentials/{ref}` is an RFC 7396 JSON Merge Patch (`Content-Type: application/merge-patch+json`): a field present is applied, a field absent is untouched, `secret: null` removes the secret (record becomes `declared`). It never creates — no own record → 404. `write` is required for metadata keys present, `write_secret` for a `secret` key present (string or `null`), both when both are present, evaluated before either half is applied. This is what lets a secret-blind administrator send `PATCH {"sharing": …}` under `write` alone, and what lets `fallback` and `secret` change together in one request for suppression (ADR-0008).

### No-op rule, and why a secret write always bumps

A `PATCH` (or a `PUT` with `secret: null` against an already-`declared` row) carrying no secret change and whose metadata equals the current record is a no-op: 204, unchanged `ETag`, no version bump, validation still runs. A body that writes or removes a secret is **never** a no-op — `PUT` with a string secret always writes and bumps, `PATCH {"secret": …}` (string or `null`) always writes and bumps, identical bytes included. Comparing submitted bytes against the stored fingerprint to skip the write would be an equality oracle: a `write_secret` holder without `read_secret` could submit guesses and watch whether the version moved (D3). Under immutable versions (ADR-0006) a re-write of identical bytes is simply a new version — which is also how a client recovers a corrupted entry.

### The secret-less record: reached only on purpose

`declared` — a record with no secret — is reached only two ways: `PATCH {"secret": null}` on an `active` record, or `PUT` with an explicit `"secret": null`, on create or on replace of an `active` row. Never by omission: an absent `secret` key is rejected before either write path runs, so a forgotten secret never creates a record by accident, string or none. Creation is atomic either way — a string secret is written under a fresh `value_id` and the row created in one transaction (ADR-0006); a `null` create touches nothing outside the row (no `value_id`, no fence, no gc entry) because there is no backend write to protect against a crash.

### Consequences

- Metadata can be edited without ever touching the secret — the value-blind administrator role exists because of this (D2).
- A "declarer" role (`write` without `write_secret`) can create a secret-less record or edit existing metadata, but can never bring a new reference into being *with* a secret.
- Suppressing an inherited credential with no row of one's own is one request: `PUT` with `secret: null` creates the blocking row directly (ADR-0008).
- Cost: a write's required action(s) are derived from its body, not its address — the one deliberate exception to path-based authorization in this design (ADR-0004 decides reads the opposite way, by projection).

### Confirmation

- E2E: `PUT` with `If-None-Match: *` and no `secret` key → 400 `SECRET_REQUIRED`; the same with `secret: null` → 201, `declared`, no backend call, `write` alone suffices.
- E2E: authorization of a `PATCH` follows its body — a `write`-only caller may send `{"sharing": …}` but not `{"secret": …}`, and conversely for `write_secret`-only.
- E2E: a `PUT`/`PATCH` writing or removing a secret always bumps `version`, even for identical bytes; a metadata-only no-op never does.

## Pros and Cons of the Options

### C1: secret sub-resource with its own `PUT`/`DELETE`

- Good: symmetric with a per-address read (ADR-0004's rejected B1); no body inspection to authorize a write.
- Bad: creating a credential is two requests with no atomicity between them — a client that stops after the first leaves an unintended secret-less record.
- Bad: the sub-resource has no representation of its own for a precondition to bind to, so `If-Match`/`If-None-Match` need a redefinition against the record's validator instead of a direct RFC 9110 reading.
- Bad: suppressing an active credential without a window needs an explicit ordering rule across two requests.

### C2: `PUT` (full replace) and `PATCH` (merge) on the record (CHOSEN)

- Good: creation is atomic — one `PUT`, one write (ADR-0006), record and secret together (D1).
- Good: suppressing an active credential is atomic too — one `PATCH` carrying both `fallback` and `secret: null`.
- Good: one resource, one validator, no deviation from RFC 9110 to state (D4).
- Good: `PATCH`'s merge semantics are RFC 7396 as published, not bespoke, and are exactly what lets a value-blind edit coexist with a secret on one resource (D2).
- Bad: a write's required action(s) come from its body rather than its address.
- Bad: secret writes can no longer be rate-limited or audited by path separately from metadata writes, since both share `PUT`/`PATCH /credentials/{ref}`.

## More Information

Full precondition table (create/replace/rotate/suppress request shapes), status codes: DESIGN §4.3.2.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2
- `cpt-cf-credstore-fr-write-credential-record`, `-fr-write-secret` — the two write verbs and the body-derived action split.
- `cpt-cf-credstore-fr-authz-action-split` — `write` vs `write_secret`, evaluated from the body.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) (the one-entity model) and [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md) (the write protocol under a string secret).
- Built on by [ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md) (suppression as one merge-patch request).
