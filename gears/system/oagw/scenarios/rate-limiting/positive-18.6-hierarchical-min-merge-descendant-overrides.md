# Hierarchical min() merge for descendant overrides

## Setup

Parent tenant upstream:

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "sustained": { "rate": 1000, "window": "minute" }
  }
}
```

Child tenant binding sets stricter limit:

```json
{
  "rate_limit": {
    "sustained": { "rate": 100, "window": "minute" }
  }
}
```

The child's limit is written before the parent switches to `enforce`: once an ancestor enforces its limit, a child's own limit is refused (`400`, `negative-9.9` Scenario D).

## Expected behavior

- Effective limit for child is `min(parent, child)`.
- After 100 requests/min, child gets `429`.
- The merged limit stays `enforce`, whichever level's value won: a tenant below the child cannot replace it with a `private` limit (see `negative-6.2` Scenario B).

## Scenario B: route limits do not reset the upstream bucket

An upstream limit and a route limit are separate buckets, both enforced, so the effective limit is `min(upstream, route)`. The route's limit is not merged into the upstream's: the upstream bucket keeps the upstream's limit whichever route matched.

Upstream `rate_limit`: `2/minute`. Two routes, each with an `inherit` limit stricter per second but of a different size: `/v1/models` `10/hour`, `/health` `20/hour`.

Expected:
- `GET /v1/models` → `200`, `GET /health` → `200`.
- `GET /v1/models` → `429` from the upstream bucket: `X-RateLimit-Limit: 2`, `X-RateLimit-Remaining: 0`, `Retry-After` about 30 s. Calling the other route does not give the upstream bucket a fresh capacity.

## Scenario C: a shadowed ancestor route's enforced limit uses its own bucket

A child's route that matches the same request as the parent's route shadows it; the parent route's enforced limit still applies, on the parent route's own bucket, as if that route had matched. It is not merged into the upstream limit.

Parent upstream shares its `auth` (`inherit`), no limit; parent route `POST /echo` with `rate_limit` `enforce` 2/min. Child upstream on the same alias, no limit; child route `POST /echo`, no limit.

Expected:
- Child `POST /echo` → `200` twice (`X-RateLimit-Limit: 2`, `X-RateLimit-Remaining` `1` then `0`).
- Third → `429` from the parent route's bucket: `X-RateLimit-Limit: 2`, `X-RateLimit-Remaining: 0`, `Retry-After` about 30 s.

See also `negative-6.2` Scenario C.
