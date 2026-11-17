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
- Only direct allocations count: an upstream of a tenant below a child that binds the alias itself allocates from that child's budget, not from the parent's. With tenant-a `100/min` → root `60/min` → l1a `50/min`, an unchanged `PUT` of root's upstream is accepted (60 ≤ 100).
- Changing the parent's `sustained.window` with the same `total` re-checks the children: `100/minute` → `100/hour` with a child at `60/minute` is rejected (`400`).

## Scenario B: shared pool

```json
{ "rate_limit": { "budget": { "mode": "shared", "total": 100 }, "sustained": { "rate": 100, "window": "minute" } } }
```

Expected:
- Multiple tenants share the same pool; first-come-first-served. The pool is one bucket of `total` requests per the owner's window, drawn on by every tenant whose limit is merged from the owner (children calling the owner's upstream and children binding the alias without their own `private` limit).
- Each tenant also keeps its own scoped bucket from `sustained` / `burst`.
- With `sustained.rate: 100`, `scope: tenant` and `total: 3`, alternating requests from two child tenants give `200, 200, 200, 429`.
- The pool is charged each request's effective `cost`, so a limit whose effective `cost` exceeds `total` is rejected on write (`400`, see [2.15](../management-api/upstreams/negative-2.15-write-validation-rejected-400.md)): with `total: 3`, a child binding the alias with `cost: 4` and `sustained.rate: 10` (the stricter limit, so the effective one) gets `rate_limit.cost (4) must not exceed budget.total (3) of the shared budget an ancestor's upstream defines for this alias`; `cost: 3` is accepted and admits one request per window (`200, 429`).

## Scenario C: unlimited

```json
{ "rate_limit": { "budget": { "mode": "unlimited" }, "sustained": { "rate": 100, "window": "minute" } } }
```

Expected:
- No budget allocation validation is enforced.
