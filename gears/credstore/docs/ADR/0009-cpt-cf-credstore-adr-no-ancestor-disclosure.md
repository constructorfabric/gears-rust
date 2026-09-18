---
status: accepted
date: 2026-09-18
---

Created:  2026-09-18 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0009: An Inherited Entry Discloses Nothing About the Ancestor

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What a response carries, and for which row](#what-a-response-carries-and-for-which-row)
  - [The `ETag`: strong for an own row, weak otherwise](#the-etag-strong-for-an-own-row-weak-otherwise)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep `owner_tenant_id` and `is_inherited` (status quo)](#keep-owner_tenant_id-and-is_inherited-status-quo)
  - [Drop `owner_tenant_id` and `is_inherited`; `inheritance` is the only signal (CHOSEN)](#drop-owner_tenant_id-and-is_inherited-inheritance-is-the-only-signal-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-no-ancestor-disclosure`

## Context and Problem Statement

Today's metadata carries `owner_tenant_id` and `is_inherited`; for an inherited credential the former names an **ancestor** tenant. The gear can name that tenant only because it reads the ancestor chain from the Tenant Resolver with barriers ignored ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)) — a privilege the gear holds and the caller does not. Handing that identifier to the caller uses the gear's trusted position to disclose what the position was granted for: a confused-deputy shape, and one that reaches ancestors above an isolation barrier a barrier-respecting traversal would never show the caller.

## Decision Drivers

- **D1** — the caller must learn nothing about an ancestor that it could not already obtain through its own authorized reads.
- **D2** — a boolean cannot distinguish "mine, and nothing above" from "mine, and shadowing something above" — the distinction that decides what happens on delete.
- **D3** — a CAS validator must exist for whichever row the caller may actually write, and never leak another tenant's write activity.

## Considered Options

- **Status quo** — keep `owner_tenant_id` and `is_inherited`.
- **Chosen** — drop both; `inheritance` (`own`/`inherited`/`overridden`/`suppressed`) is the only hierarchy signal, and every row-specific field (`owner_id`, `fallback`, `version`, `updated_at`) is shown only for the caller's own row.

## Decision Outcome

**Chosen: drop `owner_tenant_id` and `is_inherited`.** `inheritance` answers what either field answered, and answers it better (D2). The tenant identifier is dropped for a reason worth stating rather than for tidiness: the gear learned it only by looking past an isolation barrier on the caller's behalf, and what it learned that way is not the caller's to keep. Nothing is permanently lost — an entitled caller can walk Account Management upward, one authorized read per level, and reconstruct where a credential comes from; what is removed is the shortcut that skipped that authorization.

### What a response carries, and for which row

`owner_id` (creating subject), `fallback`, `version`, and `updated_at` are shown only for the caller's own row — `declared` or `active` — and omitted from an inherited entry, for the same reason as the tenant identifier: they describe another tenant's write activity, useless to a caller that cannot write that row anyway. `inheritance` and `status` describe two different things and can disagree on purpose: `status: declared` next to `inheritance: inherited` reads as "you removed your own value here, and what you get today is an ancestor's."

### The `ETag`: strong for an own row, weak otherwise

A caller whose tenant holds a row — `declared` or `active` — keeps the strong `"<id>.<version>"` validator and both `version`/`updated_at`, whatever the reference resolves to. A caller with no row at all gets a weak, opaque `W/"…"` validator derived from whichever ancestor's row currently wins (its id and version, hashed — never the raw id): it still changes when that ancestor writes, so change detection under `read` still works, but RFC 9110 requires the strong comparison for `If-Match`, so it can never be used to write (D3).

### Consequences

- The collection read mixes items with and without `updated_at`, which is why [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) offers no ordering or filtering on it.
- An operator investigating provenance uses their own rights to walk the chain; the field that used to shortcut this is gone.
- **Revisit trigger**: this rests entirely on the ancestor chain staying unpublished — the Tenant Resolver has no HTTP API today. If one is added and exposes `get_ancestors` to callers, the argument weakens and re-adding `owner_tenant_id` costs nothing.

### Confirmation

- Contract: a `Credential` for which the caller's tenant holds no row carries neither `version` nor `updated_at`, and a weak `ETag` refused by `If-Match`.
- E2E: a caller holding a `declared` row under a reference that resolves to an ancestor's secret sees `inheritance: inherited`, `status: declared`, its own `version`, and a strong `ETag` a guarded `PATCH` accepts.
- Contract: no response, at any authorization level, ever carries an ancestor's tenant id.

## Pros and Cons of the Options

### Keep `owner_tenant_id` and `is_inherited` (status quo)

- Good: no reconstruction cost for an operator — the ancestor tenant is one field away.
- Bad: discloses an ancestor tenant the caller has no independent route to, including across an isolation barrier (D1) — the confused-deputy shape this ADR exists to close.
- Bad: `is_inherited` cannot express "mine, shadowing something above" (D2).

### Drop `owner_tenant_id` and `is_inherited`; `inheritance` is the only signal (CHOSEN)

- Good: nothing is disclosed beyond what the caller's own authorized reads could already reconstruct (D1).
- Good: `inheritance`'s four states are strictly more informative than the boolean they replace (D2).
- Bad: provenance investigation costs several authorized requests instead of one field, paid by whoever holds the rights to make them.

## More Information

- Pending confirmation with the Tenant Resolver's owners: is the upward chain meant to stay unavailable to a minimally-privileged caller once that gear grows an HTTP API, or is it planned to be public? The answer changes nothing about this split, only whether the revisit trigger above ever fires.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2
- `cpt-cf-credstore-fr-inheritance-status` — `inheritance` as the sole hierarchy signal.
- `cpt-cf-credstore-nfr-tenant-isolation` — no ancestor identity crosses an isolation barrier through a credential response.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) (the one item shape) and [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) (the barrier-bypassing ancestor-chain lookup this ADR bounds the disclosure of).
