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
  - [Pagination, and secret mode](#pagination-and-secret-mode)
  - [What stays out of the filter](#what-stays-out-of-the-filter)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Upward-rooted collection, tenant predicate as a gate (CHOSEN)](#upward-rooted-collection-tenant-predicate-as-a-gate-chosen)
  - [Own-rows-only, or tenant predicate as a SQL clamp](#own-rows-only-or-tenant-predicate-as-a-sql-clamp)
  - [Project `tenant_closure`, or aggregate across descendants](#project-tenant_closure-or-aggregate-across-descendants)
- [Revisit Triggers](#revisit-triggers)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-upward-collection-read`

## Context and Problem Statement

[ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) introduces a credential-record collection (`cpt-cf-credstore-fr-list-credentials`). DESIGN §4.4 records that the gear advertises **no PEP capabilities**: the PDP hands it pre-expanded, flat tenant predicates and resolves any subtree grant on its own side, on the premise that every operation is a point operation in the caller's own tenant. A collection read breaks that premise on its face — a useful listing must show **inherited** entries, rows that come from other tenants, which a flat `owner_tenant_id` predicate would filter out. It also raises a second problem: the listing must show one entry per reference, the one a secret read would resolve, so several rows collapse into one item — and nothing in the platform reduces rows inside cursor pagination today.

## Decision Drivers

- **D1** — keep the no-projection contract: no `tenant_closure`, no co-located Account Management database, no new PEP capability.
- **D2** — the listing must show inheritance, or it defeats the requirement it exists for.
- **D3** — one authorization model for the point and the collection reads.
- **D4** — pagination must not lose or duplicate a reference's winner across a page boundary.
- **D5** — fail closed on anything unexpected: an unknown predicate, an unsupported field, a structured subtree predicate.

## Considered Options

1. **Upward-rooted collection, tenant predicate as a gate** — rooted at the caller's tenant, spanning it and its ancestor chain; the flat tenant predicate gates the request rather than clamping rows.
2. **Own-rows-only collection** — no ancestors, no inheritance shown.
3. **Tenant predicate applied as a SQL clamp** — as other gears do.
4. **Declare `tenant_hierarchy` and project `tenant_closure`** — accept subtree predicates, let SQL expand them.
5. **Aggregated listing across descendants** — one call returns a whole subtree's catalogues.

## Decision Outcome

**Chosen: option 1.** The collection is **rooted at the caller's tenant and walks upward only** — its rows come from that tenant and its ancestor chain, never from descendants. The flat tenant predicate remains a **gate** on the caller's own tenant, not a row clamp. Cross-tenant listing is not provided: a parent needing a child's catalogue acts in that child's context, as it already does for the point read. DESIGN §4.4's "no LIST" is restated as **"no downward listing"**; the collection needs no closure table because it walks the chain the Tenant Resolver already supplies for every point read.

### Barriers are deliberately bypassed for the ancestor chain

Building the chain requires knowing **all** ancestors, so this lookup is the single place the gear looks past an isolation barrier — deliberate, because a barrier (`self_managed`) isolates *management*, not data a tenant already published downward as `shared` (`cpt-cf-credstore-fr-hierarchical-resolve`). Invariants that keep it narrow: it reads **ancestor identifiers only** (ordinary visibility still gates which rows are seen — `shared` only from an ancestor); it grants **no authority** (the PDP gate and role inheritance still respect barriers); it never traverses **downward**; it is confined to **one call site**. Because a barrier-respecting scope excludes a barrier tenant's ancestors by construction, clamping the tenant dimension from such a scope would silently drop the inherited half of exactly those tenants' catalogues — which is why the tenant dimension must be a gate, not a clamp.

### How authorization applies to a collection

The point read's authorization is reused verbatim (D3), now serving several references at once:

1. The ancestor chain is fetched with barriers ignored, as above.
2. The SQL query is not tenant-clamped: candidate rows follow the point read's visibility rules (own tenant: private/tenant/shared; ancestors: shared only). Candidates are `active` rows and `declared` rows with `fallback: none` ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)); a `declared`/`inherit` row is never a candidate.
3. The PDP gate targets the **request** — does the scope admit the caller's own tenant — not individual rows; hierarchical visibility is the resolver's business, which is what lets an inherited entry appear at all. **A gated-out caller gets an empty page, not a refusal**: there is no operation-level evaluation to refuse from — the PDP resource is the resolved concrete type, so a scope exists only once rows are typed, and a no-grant caller is indistinguishable, by construction, from an empty catalogue (revisit trigger, not a smuggled status-code change).
4. Attribute predicates are pushed into SQL only when **invariant across a reference's chain** — otherwise clamping could change which row wins the reduction before it ever runs (a caller granted only `smtp` could see an ancestor's `smtp` row win where the point read refuses the nearer, ungranted `basic_auth` row it actually resolves to). `secret_type_uuid` is invariant by `cpt-cf-credstore-fr-override-type-consistency`; `sharing`, `updated_at`, `expires_at`, `owner_tenant_id` are not.
5. **Filter in SQL first, reduce in memory, authorize per type.** Step one selects candidate references and their distinct types, clamped by `reference`/`secret_type_uuid`, index-backed on `(tenant_id, secret_type_uuid)`; step two fetches those references' rows whole, unclamped, so reduction sees every row a secret read would see, and the winner is authorized afterward — `list` in metadata mode, `read_secret` per type in secret mode, `list` too whenever a record field rides alongside `secret`. A denied type's references never reach step two, a sound clamp because the type is chain-invariant; a violated invariant drops the reference and raises a metric — a missing entry, never a false one. A short page is part of the contract.
6. A structured `InTenantSubtree` predicate, or any undeclared attribute dimension, still fails closed (D5).

**Reducing a reference to one item.** Reduction sees every visible row of the reference, not only what a clamp admitted — the property step 5 exists to preserve. **Only resolvable rows compete, qualified by `fallback`**: a `declared`/`inherit` row never wins while a resolvable inherited row exists — getting this wrong would report "configured here" for a credential that is actually inherited; a `declared`/`none` row **does** compete and, when nearest, **wins**, reported `suppressed`, with the point read returning 404 rather than falling through ([ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md)); a reference whose only row is `declared` still appears, with its state. **Among resolvable rows, nearest wins**, `private` beating non-`private` at the same depth — the same priority the point read uses, reused rather than restated (D3).

### Pagination, and secret mode

Canonical sort: `reference ASC, id ASC`. Because the sort leads with `reference`, all of one reference's rows are contiguous, so **a cursor always sits on a reference boundary** — a page extends to the end of the group it lands in, reduction picks one winner per group, and no winner can be split across pages (D4). `items.len()` may be smaller than `limit`; clients treat `next_cursor`, not the count, as the "more pages" signal — the same contract Account Management's listing publishes. This is the platform's first row-reducing cursor pagination.

**Secret mode** (`$select=…,secret`, `cpt-cf-credstore-fr-bulk-read-secrets`) is the bounded bulk read moved here from the point-read mechanism [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) defines: the same filter-in-SQL, reduce-in-memory pipeline above, authorizing `read_secret` per distinct type instead of `list` (`list` too, per type, whenever a record field rides alongside `secret`). It sidesteps the boundary rule above entirely: no `cursor`, `limit`, or `$orderby` — a secret read is never paginated or walked — and fetches at most `cap + 1` candidates, failing the whole request closed above the cap rather than truncating. The only selectors are `reference in (...)` and `type eq/in (...)`, the two fields already held chain-invariant above; an item the filter found but the caller may not read is **omitted**, never reported. Full rules, reason codes, and the response envelope: DESIGN §4.3.2.

### What stays out of the filter

`inheritance` is not a column — it is the outcome of reduction — so it is never filterable or sortable; a caller wanting only its own rows reads the page and keeps the `own` items. A caller's `$filter` obeys the same chain-invariance rule as the policy clamp:

| Field | Where it applies | Why |
|---|---|---|
| `reference` | SQL clamp | the grouping key itself |
| `secret_type_uuid` | SQL clamp | invariant by `cpt-cf-credstore-fr-override-type-consistency` |
| `sharing`, `expires_at`, `fallback` | after reduction | vary across one reference's rows |
| `updated_at`, `owner_tenant_id` | not filterable | withheld from every row for which the caller holds none ([ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md)); a mixed page has no total order |
| `inheritance` | never | not a column |

Filtering after reduction is correct, not a compromise: "shared credentials" means "effective row is shared", not "a shared row somewhere up the chain" — it costs short pages, already contractual. Every filterable/orderable field is index-backed; the allowlist and its migration ship together.

### Consequences

- The no-projection contract holds (D1); DESIGN §4.4 is restated as "no downward listing", tenant as a gate, attributes as clamps.
- One authorization path serves both reads (D3); the listing shows inheritance across barriers (D2); a parent still cannot list a child's catalogue directly.
- Short pages and row-reducing pagination are new, load-bearing machinery.

### Confirmation

- E2E: a barrier tenant sees its ancestors' `shared` entries as `inherited` and can read their secrets, but no ancestor `tenant`/`private` row; a caller whose scope excludes its own tenant gets an empty page, never a row from another tenant.
- E2E: a reference spanning three tenants of a chain yields exactly one item, including when a page boundary falls inside that group; a `declared`+`fallback:none` winner yields `suppressed` and the point read 404s.
- E2E: `$filter` on `inheritance`, `owner_tenant_id`, `updated_at` are rejected; `secret_type_uuid` narrows in SQL, `sharing` narrows after reduction; secret mode additionally rejects `limit`/`cursor`/`$orderby` and fails closed above the cap.
- Unit: the winner computed over a `secret_type_uuid`-clamped set equals the winner over the unclamped set.

## Pros and Cons of the Options

### Upward-rooted collection, tenant predicate as a gate (CHOSEN)

- Good: no closure table, no capability, no projection; reuses the point read's authorization verbatim.
- Good: shows inheritance, the requirement's whole purpose.
- Bad: the tenant dimension is handled differently from every other gear — documented so a future reader does not "fix" it with a clamp.
- Bad: row reduction inside pagination is unprecedented in this platform.

### Own-rows-only, or tenant predicate as a SQL clamp

- Good: trivially correct — one row per item, identical to every other gear, no reduction, no boundary rule.
- Bad: both drop exactly the inherited rows the listing exists to show; a SQL clamp built from a barrier-respecting scope would drop everything inherited for a barrier tenant. Defeats D2.

### Project `tenant_closure`, or aggregate across descendants

- Good: subtree predicates become executable in SQL, and a descendant listing — a partner reviewing every customer's catalogue in one screen — comes free.
- Bad: the projection table, its sync, and the co-location requirement DESIGN §4.4 avoided, plus a cross-tenant response shape the gear has never had — unnecessary for a read that never expands downward. Deferred, not rejected — see Revisit Triggers.

## Revisit Triggers

- Operators cannot tell a missing `list` grant from an empty catalogue — needs an operation-level evaluation against the base type, which this gear does not authorize against today.
- A product need appears for a parent to review descendants' catalogues in one response, or the PDP starts sending structured subtree predicates as a matter of course.
- `toolkit-db` grows a window-function API — reduction could move into SQL and the invariance criterion would stop being load-bearing.
- Row-reducing pagination proves fragile; the fallback is an own-rows-only listing plus a point read per inherited reference.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3.2, §4.4
- `cpt-cf-credstore-fr-list-credentials`, `-fr-get-credential`, `-fr-inheritance-status`, `-fr-authz-action-split`, `-fr-hierarchical-resolve`, `-fr-secret-shadowing`, `-fr-bulk-read-secrets`, `-nfr-tenant-isolation`.
- Depends on `cpt-cf-credstore-fr-override-type-consistency` for the type clamp's selectivity — not load-bearing for correctness, since the winner is authorized after reduction.
- Builds on [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md) (one entity, one `$select`-driven read mechanism), [ADR-0008](0008-cpt-cf-credstore-adr-suppression-fallback.md) (the `declared`/`none` competing-winner rule), [ADR-0009](0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md) (fields withheld from a non-own row), and [ADR-0010](0010-cpt-cf-credstore-adr-type-scoped-authorization.md) (per-type authorization).
- Pagination follows `guidelines/DNA/REST/PAGINATION.md`; the reference-boundary cursor rule is new here, not a platform pattern.
