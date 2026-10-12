---
status: superseded by ADR-0006
date: 2026-07-08
---

Created:  2026-07-07 by Virtuozzo International GmbH
Updated:  2026-10-01 by Constructor Tech

# ADR-0003: Value-Fingerprint Fence for the Metadata/Value Dual Write

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-value-fingerprint-fence`

**Superseded by [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md).**

## Context and Problem Statement

A write spans the gear's metadata row and the backend value with no shared transaction, so two concurrent last-writer-wins writes could interleave crosswise and leave a value under a sharing label that a different writer set. Separately, the bare-version `ETag` restarted at 1 for every re-created row, so a stale validator could match a different generation (ABA).

## Decision Drivers

- **D1** — a row and its backend value, written without a shared transaction, must never serve a value under another writer's label.
- **D2** — a validator from a deleted generation must never match a re-created record.

## Considered Options

- **Trust the dual write** — no check.
- **Value-fingerprint fence plus a generation-bound `ETag`** — chosen at the time.

## Decision Outcome

Each row stored `value_fp = HMAC(fence_key, value)` written atomically with the metadata; a read recomputed it and failed closed (404) on a mismatch. The `ETag` became the generation-bound pair `(row id, version)`.

### Consequences

Under immutable value versions with an exact-bytes `get`, a mismatch between a row and the backend is impossible by construction: there is nothing for a fingerprint to detect, so `value_fp`, the fence key and the fence metrics are removed. A backend that returns different bytes violates the plugin contract; the gear does not detect out-of-band tampering. The generation-bound `ETag` is **kept**: the record id is minted at create and never reused. See ADR-0006.

### Confirmation

Superseded: nothing of this decision ships any more; ADR-0006 carries the confirmation of what replaced it.

## Pros and Cons of the Options

- **Trust the dual write** — Good: nothing to store. Bad: a crosswise interleaving serves a wrong value (D1).
- **Fingerprint fence** — Good: a mismatch fails closed. Bad: detects after the fact, needs a fence key and a healing re-write; the generation-bound `ETag` part is kept.
