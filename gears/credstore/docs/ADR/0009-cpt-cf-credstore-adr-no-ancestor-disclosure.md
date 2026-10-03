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

### Consequences

- The collection mixes items with and without `updated_at`, which is why [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) offers no ordering or filtering on it.
- An operator investigating provenance walks the chain with their own rights.
- **Revisit trigger**: this rests on the ancestor chain staying unpublished — the Tenant Resolver has no HTTP API today. If one exposes `get_ancestors` to callers, re-adding `owner_tenant_id` costs nothing.

### Confirmation

- Contract: a `Credential` for which the caller's tenant holds no row carries neither `version` nor `updated_at`, and a weak `ETag` that `If-Match` refuses.
- E2E: a caller holding a `declared` row under a reference that resolves to an ancestor's secret sees `inheritance: inherited`, `status: declared`, its own `version`, and a strong `ETag` a guarded `PATCH` accepts.
- Contract: no response, at any authorization level, carries an ancestor's tenant id.

## Pros and Cons of the Options

- **Status quo** — Good: the ancestor tenant is one field away for an operator. Bad: discloses an ancestor the caller has no independent route to, including across an isolation barrier (D1); `is_inherited` cannot express "mine, shadowing something above" (D2).
- **Chosen** — see Decision Outcome and Consequences.

## More Information

- To confirm with the Tenant Resolver's owners: will the upward chain stay unavailable to a minimally privileged caller once that gear grows an HTTP API? The answer decides only whether the revisit trigger fires.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2
- `cpt-cf-credstore-fr-inheritance-status`, `cpt-cf-credstore-nfr-tenant-isolation`.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md).
