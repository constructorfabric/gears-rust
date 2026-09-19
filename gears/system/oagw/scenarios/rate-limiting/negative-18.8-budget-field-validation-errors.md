# Budget field validation errors

## Scenario A: allocated/shared mode without `total`

```json
{ "rate_limit": { "sustained": { "rate": 100, "window": "minute" }, "budget": { "mode": "allocated" } } }
```

(Same for `"mode": "shared"`.)

Expected:
- `400 Bad Request`
- Body text includes `budget.total is required`

## Scenario B: `overcommit_ratio` out of range

```json
{ "rate_limit": { "sustained": { "rate": 100, "window": "minute" }, "budget": { "mode": "allocated", "total": 100, "overcommit_ratio": 0.5 } } }
```

(Same failure for `overcommit_ratio: 2.5` — above the maximum.)

Expected:
- `400 Bad Request`
- Body text includes `overcommit_ratio must be between`
- Valid range is `[1.0, 2.0]` (see [ADR-0004 Schema Changes](../../docs/ADR/0004-rate-limiting.md#schema-changes)).

## What to check

- `mode: "unlimited"` does **not** require `total` — creation succeeds with only `{ "mode": "unlimited" }`.
- These are upstream-creation-time (and update-time) validation errors, distinct from the runtime budget-exceeded rejection in [positive-18.9](positive-18.9-budget-allocated-hierarchy-enforcement.md).
