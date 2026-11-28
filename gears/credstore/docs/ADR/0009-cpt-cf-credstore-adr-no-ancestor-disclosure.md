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
  - [Bounded read (in-process only)](#bounded-read-in-process-only)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-no-ancestor-disclosure`

## Context and Problem Statement

Today's metadata carries `owner_tenant_id` and `is_inherited`; for an inherited credential the former names an **ancestor** tenant. The gear knows that tenant only because it reads the ancestor chain with barriers ignored ([ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md)), a privilege the caller does not hold. Handing the identifier on makes the gear a confused deputy: it discloses ancestors above an isolation barrier that a barrier-respecting traversal would never show.

## Decision Drivers

- **D1** — the caller learns nothing about an ancestor that its own authorized reads would not give it.
- **D2** — a boolean cannot distinguish "mine, nothing above" from "mine, shadowing something above", which decides what happens on delete.
- **D3** — a CAS validator exists for whichever row the caller may write and never leaks another tenant's write activity.

## Considered Options

- **Status quo** — keep `owner_tenant_id` and `is_inherited`.
- **Chosen** — drop both; `inheritance` (`own`/`inherited`/`overridden`/`suppressed`) is the only hierarchy signal; row-specific fields appear only for the caller's own row.

## Decision Outcome

**Chosen: drop `owner_tenant_id` and `is_inherited`.** `inheritance` answers what both fields answered and satisfies D2. The tenant identifier goes because the gear learned it by looking past a barrier on the caller's behalf. Nothing is lost for good: an entitled caller can walk Account Management upward, one authorized read per level; only the shortcut around that authorization is removed.

### What a response carries, and for which row

| Field | Own row (`declared` or `active`) | No own row (inherited entry) |
|---|---|---|
| `inheritance`, `status`, `reference`, `type`, `sharing`, `expires_at` | yes | yes |
| `owner_id`, `fallback`, `version`, `updated_at` | yes | omitted — another tenant's write activity |
| `ETag` | strong `"<id>.<version>"` | weak `W/"…"`, a hash of the winning ancestor row's id and version (never the raw id) |

`inheritance` and `status` may disagree on purpose: `status: declared` with `inheritance: inherited` means "you removed your own value here; today you get an ancestor's". The weak validator still changes when the ancestor writes, so change detection under `read` works, but RFC 9110 requires the strong comparison for `If-Match`, so it can never be used to write (D3).

In-process, the `Secret` of `get_secret` carries the same validator the REST `ETag` carries for the same read: the strong validator of the caller's own row when it holds one, otherwise the opaque weak tag of the winning row. It never carries the id or the version of another tenant's row.

### Bounded read (in-process only)

A consumer that injects a credential on behalf of a tenant, such as the outbound gateway checking that a key belongs to the owner of an upstream's auth configuration or to a tenant below it, needs to know where a value comes from without being told which tenant that is. The SDK method `get_secret_within(ctx, key, bound)` answers exactly that question. The caller names a tenant, `bound`, from its own visible chain; the value is served only when it comes from `bound` or from a tenant below `bound` on that chain. The response names no tenant.

The order is fixed. The PDP decision comes first, as for every read, so a denied caller gets `PermissionDenied` whatever `bound` is. Then `bound` is checked against the part of the caller's chain that is visible with isolation barriers respected: the caller's own tenant, and its ancestors up to and including the first self-managed one. A `bound` outside that part is `InvalidArgument` with `BOUND_NOT_ON_CHAIN`, one answer for an unrelated identifier and for an ancestor above a barrier, so the check discloses nothing about what lies beyond the barrier. Resolution and the PDP's type and reference constraints then run exactly as for `get_secret`, over the full chain, so inheritance still passes through barriers. If there is no value to serve (nothing resolves, a suppressing row, a decisive row of another type, a record without a value, a record the constraints exclude) the answer is the usual `Ok(None)` and `bound` plays no part. Otherwise the row the value comes from is compared with `bound`: a row above it is `FailedPrecondition` with `OWNER_ABOVE_BOUND`, the value is not read from the store, and the read is audited as a failed read, like `SECRET_EXPIRED`. Only after that comes the expiry test, so an expired winner above the bound is `OWNER_ABOVE_BOUND`, while an expired winner within the bound is `SECRET_EXPIRED`. The row that counts is the one the value comes from, not the caller's own row: a caller's own `declared` row that inherits a partner's value is judged by the partner.

This weakens D1 in one narrow way, and the decision accepts it. A caller can probe the levels of its visible chain one by one and learn which of them holds the shared row a value comes from. That caller already reads this value without a bound, and `bound` can only name tenants it sees, so nothing is learned about an ancestor beyond a barrier. The method exists in the SDK only. It has no REST counterpart, because over HTTP the answer would confirm to an arbitrary caller that a tenant belongs to a chain.

### Consequences

- The collection mixes items with and without `updated_at`, which is why [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) offers no ordering or filtering on it.
- An operator investigating provenance walks the chain with their own rights.
- The bounded read adds two codes to the vocabulary, `OWNER_ABOVE_BOUND` and `BOUND_NOT_ON_CHAIN`, and an in-process consumer that needs an ownership check no longer needs a tenant identifier in the response.
- A caller can learn, by probing its visible chain with different bounds, which visible level holds the shared row a value comes from (see "Bounded read").
- **Revisit trigger**: this rests on the ancestor chain staying unpublished — the Tenant Resolver has no HTTP API today. If one exposes `get_ancestors` to callers, re-adding `owner_tenant_id` costs nothing.

### Confirmation

- Contract: a `Credential` for which the caller's tenant holds no row carries neither `version` nor `updated_at`, and a weak `ETag` that `If-Match` refuses.
- E2E: a caller holding a `declared` row under a reference that resolves to an ancestor's secret sees `inheritance: inherited`, `status: declared`, its own `version`, and a strong `ETag` a guarded `PATCH` accepts.
- Contract: no response, at any authorization level, carries an ancestor's tenant id.
- Contract: the `Secret` of `get_secret` carries a strong validator when the caller holds an own row and otherwise a weak one whose text equals the REST `ETag` of the same read and contains no row id.
- Bounded read: a value from a row above `bound` is `OWNER_ABOVE_BOUND` and nothing is read from the store; a `bound` that is not on the visible chain is `BOUND_NOT_ON_CHAIN`, identically for an unrelated id and for an ancestor above a barrier, and only after the PDP decision; an expired winner above `bound` is `OWNER_ABOVE_BOUND`, within it `SECRET_EXPIRED`; a read with no value to serve is `Ok(None)` whatever `bound` is.

## Pros and Cons of the Options

- **Status quo** — Good: the ancestor tenant is one field away for an operator. Bad: discloses an ancestor the caller has no independent route to, including across an isolation barrier (D1); `is_inherited` cannot express "mine, shadowing something above" (D2).
- **Chosen** — see Decision Outcome and Consequences.

## More Information

- To confirm with the Tenant Resolver's owners: will the upward chain stay unavailable to a minimally privileged caller once that gear grows an HTTP API? The answer decides only whether the revisit trigger fires.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.1
- `cpt-cf-credstore-fr-inheritance-status`, `cpt-cf-credstore-nfr-tenant-isolation`.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md).
