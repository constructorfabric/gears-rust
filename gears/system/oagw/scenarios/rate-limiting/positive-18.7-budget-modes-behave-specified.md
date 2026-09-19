# Budget modes: allocated/shared/unlimited

## Scenario A: allocated budget

Parent:

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "budget": { "mode": "allocated", "total": 100, "overcommit_ratio": 1.0 },
    "sustained": { "rate": 100, "window": "minute" }
  }
}
```

Children request allocations that sum to > total.

Expected:
- Creation/update rejected when `sum(children) > total * overcommit_ratio`.

## Scenario B: shared pool

```json
{ "rate_limit": { "budget": { "mode": "shared", "total": 100 }, "sustained": { "rate": 100, "window": "minute" } } }
```

Expected:
- Multiple tenants share the same pool; first-come-first-served.

## Scenario C: unlimited

```json
{ "rate_limit": { "budget": { "mode": "unlimited" }, "sustained": { "rate": 100, "window": "minute" } } }
```

Expected:
- No budget allocation validation is enforced.
- Omitting `budget` entirely defaults to `mode: "unlimited"` — the same no-validation behavior, not a rejection.

## Related

- [negative-18.8](negative-18.8-budget-field-validation-errors.md): field-shape validation (`total` required for allocated/shared, `overcommit_ratio` range).
- [positive-18.9](positive-18.9-budget-allocated-hierarchy-enforcement.md): concrete sum-based enforcement mechanics for `allocated` mode across a tenant hierarchy (multi-child sums, overcommit ceiling, update-time revalidation, cross-window rate normalization).
