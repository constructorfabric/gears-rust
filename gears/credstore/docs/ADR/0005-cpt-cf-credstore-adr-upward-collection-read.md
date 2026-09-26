---
status: accepted
date: 2026-09-08
---

Created:  2026-09-08 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0005: Upward-Rooted Collection Read Under the No-Projection PEP Contract

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Barriers are deliberately bypassed for the ancestor chain](#barriers-are-deliberately-bypassed-for-the-ancestor-chain)
  - [How authorization applies to a collection](#how-authorization-applies-to-a-collection)
  - [Pagination over a reduced result](#pagination-over-a-reduced-result)
  - [Secret mode](#secret-mode)
  - [What stays out of the filter](#what-stays-out-of-the-filter)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [Revisit Triggers](#revisit-triggers)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-upward-collection-read`

## Context and Problem Statement

[ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) adds a credential collection (`cpt-cf-credstore-fr-list-credentials`). The gear advertises **no PEP capabilities** (DESIGN §4.4): the PDP sends flat, pre-expanded tenant predicates and assumes point operations in the caller's own tenant. A listing must show **inherited** entries from ancestor tenants, which a flat `owner_tenant_id` predicate would drop, and one item per reference — the row a secret read would resolve — which means collapsing rows inside cursor pagination. The platform does neither today.

## Decision Drivers

- **D1** — no `tenant_closure`, no co-located Account Management database, no new PEP capability.
- **D2** — the listing shows inheritance.
- **D3** — one authorization model for the point read and the collection.
- **D4** — a page boundary never loses or duplicates a reference's winner.
- **D5** — fail closed on an unknown predicate, an unsupported field, a structured subtree predicate.

## Considered Options

1. **Upward-rooted collection, tenant predicate as a gate** — the caller's tenant and its ancestors.
2. **Own-rows-only collection** — no inheritance shown.
3. **Tenant predicate as a SQL clamp** — as other gears do.
4. **Declare `tenant_hierarchy`, project `tenant_closure`** — subtree predicates expanded in SQL.
5. **Aggregated listing across descendants.**

## Decision Outcome

**Chosen: option 1.** The collection is rooted at the caller's tenant and walks **upward only**. The flat tenant predicate **gates** the caller's own tenant and never clamps rows. A parent that needs a child's catalogue acts in the child's context, as for the point read. "No LIST" (DESIGN §4.4) becomes **"no downward listing"**. No closure table is needed: the Tenant Resolver already supplies the chain for every point read.

### Barriers are deliberately bypassed for the ancestor chain

The chain needs all ancestors, so this is the one place the gear looks past an isolation barrier. A barrier (`self_managed`) isolates management, not data an ancestor published downward as `shared` (`cpt-cf-credstore-fr-hierarchical-resolve`). The bypass reads ancestor identifiers only (an ancestor's `shared` rows are the only ones visible), grants no authority (the PDP gate and role inheritance still respect barriers), never goes downward, and lives at one call site. A barrier-respecting scope excludes a barrier tenant's ancestors, so clamping rows by it would silently drop the inherited half of that tenant's catalogue.

### How authorization applies to a collection

1. Fetch the ancestor chain, barriers ignored.
2. SQL is not tenant-clamped; visibility follows the point read (own tenant: private/tenant/shared; ancestors: `shared`). Candidates: `active` rows and `declared` rows with `fallback: none` ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)).
3. A `DISTINCT secret_type_uuid` query over the visible set, clamped by `$filter type in (…)` if given, drives the PDP gate per type — `list` in metadata mode, `read_secret` plus `list` when a record field rides with `secret`; a type whose scope does not admit the caller's tenant is not permitted. Result: the **permitted set**. An empty permitted set is an empty page, not a refusal, with no further query.
4. An attribute predicate goes into SQL only if it is **invariant across a reference's chain**; otherwise the clamp could change which row wins (a caller granted only `smtp` would see an ancestor's `smtp` row win over a nearer, ungranted `basic_auth` row). `secret_type_uuid` is invariant by `cpt-cf-credstore-fr-override-type-consistency`; `sharing`, `updated_at`, `expires_at`, `owner_tenant_id` are not.
5. **Filter in SQL, reduce in memory.** Query one: candidate references, clamped by `secret_type_uuid IN (permitted set)` and `reference IN (…)`, index `(tenant_id, secret_type_uuid)`; a reference with no permitted-type row never enters the page or consumes the cursor. Query two: those references' rows, whole and unclamped by type — `cpt-cf-credstore-fr-override-type-consistency` is enforced only upward on write, so reduction must see every row a point read sees. A winner outside the permitted set is an invariant violation (`list_type_invariant_violation`), dropped and counted, never a false entry. Short pages are contractual.
6. A structured `InTenantSubtree` predicate or an undeclared attribute fails closed (D5).

**Reducing a reference to one item.** Only resolvable rows compete. A `declared`/`inherit` row never wins while a resolvable inherited row exists. A `declared`/`none` row competes and, when nearest, wins as `suppressed`; the point read 404s ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)). A reference whose only row is `declared` still appears. Among resolvable rows the nearest wins, `private` before non-`private` at the same depth (D3).

### Pagination over a reduced result

Canonical sort `reference ASC, id ASC` keeps one reference's rows contiguous, so **a cursor always sits on a reference boundary**: a page extends to the end of the group it lands in, reduction picks one winner per group, and no winner is split (D4). `items.len()` may be below `limit`; `next_cursor`, not the count, signals more pages (as in Account Management's listing). This is the platform's first row-reducing cursor pagination.

### Secret mode

`$select=…,secret` (`cpt-cf-credstore-fr-bulk-read-secrets`) is the bounded bulk read of [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md): the same pipeline, authorizing `read_secret` per distinct type (plus `list` when a record field rides along). No `cursor`, `limit` or `$orderby`: a secret read is never paginated. At most `cap + 1` permitted candidates are fetched; above the cap the whole request fails closed. Selectors: `reference in (...)` and `type eq/in (...)` only. An item the caller may not read is omitted, never reported; values are read with bounded concurrency (8 in flight). Reason codes and envelope: DESIGN §4.3.2.

### What stays out of the filter

`inheritance` is the outcome of reduction, not a column: never filterable or sortable. A caller's `$filter` follows the same chain-invariance rule as the policy clamp:

| Field | Where | Why |
|---|---|---|
| `reference` | SQL clamp | grouping key |
| `secret_type_uuid` | SQL clamp | invariant by `cpt-cf-credstore-fr-override-type-consistency` |
| `sharing`, `expires_at`, `fallback` | after reduction | vary across a reference's rows |
| `updated_at`, `owner_tenant_id` | not filterable | withheld for non-own rows ([ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md)); a mixed page has no total order |
| `inheritance` | never | not a column |

"Shared credentials" means "the effective row is shared", so post-reduction filtering is correct; its cost is short pages. Every filterable/orderable field is index-backed; the allowlist and its migration ship together.

### Consequences

- D1 holds. Tenant as a gate, attributes as clamps: a documented deviation from other gears, not to be "fixed" later.
- One authorization path for both reads (D3); inheritance shown across barriers (D2); a parent still cannot list a child's catalogue directly.
- Short pages and row-reducing pagination are new, load-bearing machinery.

### Confirmation

- E2E: a barrier tenant sees its ancestors' `shared` entries as `inherited` and can read their secrets, but no ancestor `tenant`/`private` row; a caller whose scope excludes its own tenant gets an empty page.
- E2E: a reference spanning three tenants yields one item, also across a page boundary; a `declared`+`fallback:none` winner yields `suppressed` and the point read 404s.
- E2E: `$filter` on `inheritance`, `owner_tenant_id`, `updated_at` is rejected; `secret_type_uuid` narrows in SQL, `sharing` after reduction; secret mode rejects `limit`/`cursor`/`$orderby` and fails closed above the cap.
- Unit: the winner over a `secret_type_uuid`-clamped set equals the winner over the unclamped set.
- E2E/unit: a reference with no row of a permitted type never appears and never consumes the page; a caller with no permitted type gets an empty page without a candidate query.

## Pros and Cons of the Options

- **Option 1 (chosen)** — Good: no closure table, no capability, one authorization path. Bad: the tenant dimension differs from every other gear (documented on purpose); row reduction inside pagination is new to the platform.
- **Options 2, 3** — Good: trivially correct, one row per item. Bad: drop exactly the inherited rows the listing exists for; a clamp from a barrier-respecting scope drops everything inherited for a barrier tenant (D2).
- **Options 4, 5** — Good: subtree predicates in SQL; a descendant listing for free. Bad: projection table, sync, co-location and a cross-tenant response shape, for a read that never expands downward. Deferred, see Revisit Triggers.

## Revisit Triggers

- Operators cannot tell a missing `list` grant from an empty catalogue; fixing it needs an operation-level evaluation against the base type.
- A product need for a parent to list descendants' catalogues, or the PDP starts sending structured subtree predicates routinely.
- `toolkit-db` grows a window-function API: reduction could move into SQL.
- Row-reducing pagination proves fragile; fallback is an own-rows-only listing plus a point read per inherited reference.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §4.4
- `cpt-cf-credstore-fr-list-credentials`, `cpt-cf-credstore-fr-get-credential`, `cpt-cf-credstore-fr-inheritance-status`, `cpt-cf-credstore-fr-authz-action-split`, `cpt-cf-credstore-fr-hierarchical-resolve`, `cpt-cf-credstore-fr-secret-shadowing`, `cpt-cf-credstore-fr-bulk-read-secrets`, `cpt-cf-credstore-nfr-tenant-isolation`; `cpt-cf-credstore-fr-override-type-consistency` for the type clamp's soundness (a winner outside the permitted set is still dropped after reduction).
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md), [ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md), [ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md), [ADR-0010](0010-cpt-cf-credstore-adr-type-scoped-authorization.md). Pagination follows `guidelines/DNA/REST/PAGINATION.md`; the reference-boundary cursor rule is new here.
