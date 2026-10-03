# Allocated budget: hierarchy sum enforcement

## Setup

Parent upstream with an inheritable, allocated budget:

```json
{
  "rate_limit": {
    "sharing": "inherit",
    "sustained": { "rate": 100, "window": "minute" },
    "budget": { "mode": "allocated", "total": 100, "overcommit_ratio": 1.0 }
  }
}
```

Descendant tenants create their own upstream bound to the same alias, each with its own `rate_limit.sustained.rate` — no `budget` field on the child.

## Scenario A: children within budget

Child A requests `60/min`, child B requests `50/min` after: `60 + 50 = 110 > 100` → **child B's creation is rejected**.

```
resp.status_code == 400
"budget allocation exceeded" in resp.text
```

Two children whose rates sum to `<= total` (e.g. `40 + 40 = 80 <= 100`) both succeed.

## Scenario B: overcommit_ratio raises the ceiling, does not remove it

With `overcommit_ratio: 1.5` on the parent (ceiling = `100 * 1.5 = 150`):
- Children summing to `80 + 60 = 140 <= 150` → both succeed.
- A further child pushing the sum to `100 + 60 = 160 > 150` → rejected with `400`, `"budget allocation exceeded"`.

## Scenario C: update-time revalidation

Updating an existing child's `sustained.rate` so the new sum exceeds the parent's budget is rejected the same way (`400`, `"budget allocation exceeded"`) — allocation is revalidated on every `PUT`, not just at creation.

## Scenario D: rates are normalized to req/s before comparison

Budget comparison converts every rate to requests/second regardless of each upstream's own `window`. Parent `total: 60` at `window: "minute"` (a `1 req/s` ceiling); a child requesting `2/second` (`2 req/s`) exceeds it and is rejected, even though `2 < 60` as raw numbers.

## Scenario E: allocated budget requires the child to declare a rate limit

Under an `allocated` parent budget, a child upstream that omits `rate_limit` entirely is rejected:

```
resp.status_code == 400
"rate_limit is required" in resp.text
```

## Scenario F: 3-tier nesting — declared cost and granted allowance diverge unchecked

```
Tenant A: upstream, sustained.rate=100/min, budget={mode: allocated, total: 100}
Tenant B (child of A): upstream, sustained.rate=5/min, budget={mode: allocated, total: 1000}
Tenant C1, C2 (children of B): upstream, sustained.rate=400/min each
```

Step-by-step, each a separate request:

1. **Create B's upstream** (`sustained.rate=5`, its own `budget.total=1000`). Validated only against `A`'s budget: `5 <= 100` → **`201 Created`**. `B`'s own `budget.total=1000` is not compared against anything at this point — nothing in this request touches `A`'s ceiling.
2. **Create C1's upstream** (`sustained.rate=400`, no `budget`). Validated only against `B`'s budget (the closest allocated ancestor with this alias): `400 <= 1000` → **`201 Created`**.
3. **Create C2's upstream** (`sustained.rate=400`). Validated against `B`'s budget: `400 + 400 = 800 <= 1000` → **`201 Created`**.

All three requests succeed independently. At no point does any single request compare `B`'s declared `sustained.rate` (5, what `B` cost `A`) against `B`'s own `budget.total` (1000, what `B` grants `C1`/`C2`) — the two numbers are validated by different calls, against different ancestors, and never cross-referenced. `A` believes its subtree needs at most 100/min in total (having admitted `B` at a declared cost of 5); the same subtree can in fact reach 800/min (`C1` + `C2`) with every individual write having passed its own local check.

## What to check

- `mode: "shared"` does not enforce this sum check — a child may bind to a `shared`-budget parent without declaring its own `rate_limit` at all (see [positive-18.7 Scenario B](positive-18.7-budget-modes-behave-specified.md)).
- `mode: "unlimited"` (or no `budget` field at all — the default) skips allocation validation entirely; children may declare any rate.
- See [negative-18.8](negative-18.8-budget-field-validation-errors.md) for the field-shape validation errors that apply before this sum-based enforcement is even reached.
- **Known limitation, not covered by any scenario here**: this enforcement is validate-then-persist, not atomic (`domain/services/management/mod.rs::update_upstream` calls `validate_budget_allocation` and *then*, as a separate step, `self.upstreams.update(...)` — no transaction or compare-and-swap covers both). Two concurrent `PUT`/create requests for distinct sibling upstreams can each validate against the same pre-update sibling total and both persist, landing the combined allocation above the parent's ceiling. Scenarios A-E above are all single-request, sequential checks and do not exercise this race.
- **Also unbounded, not covered by any scenario here**: both `validate_budget_allocation` and `validate_descendants_within_budget` resolve the *entire* descendant subtree via `get_descendants` with no depth or result-size limit (see [0005-cpt-cf-oagw-feature-tenant-hierarchy.md §7](../../docs/features/0005-cpt-cf-oagw-feature-tenant-hierarchy.md)) — a very deep or broad tenant hierarchy makes a single budget-affecting write proportionally expensive, with no enforced ceiling on that cost.
- **Nested allocated budgets (3+ levels, Scenario F): the two numbers involved are not cross-checked, by design or by omission.** `validate_budget_allocation` only ever validates a write against its *closest* ancestor with the same alias — it does not simultaneously walk multiple ancestor tiers. An intermediate tenant's declared `sustained.rate` (its footprint against its own ancestor's budget) and its own `budget.total` (the ceiling it grants its own children) are validated by entirely separate calls that never reference each other. This is not covered by any automated test.
