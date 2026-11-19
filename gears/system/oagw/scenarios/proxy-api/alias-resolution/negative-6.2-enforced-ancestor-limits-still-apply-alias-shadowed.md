# Enforced ancestor limits apply when alias is shadowed

## Scenario A: child shadows the alias

### Setup

- Parent upstream `alias=api.example.com`:
  - `rate_limit.sharing=enforce` with high cap (example: 1000/min)
- Child upstream shadows same alias:
  - sets its own rate_limit lower (example: 100/min)

A child cannot write a limit while an ancestor enforces one (`400`, `negative-9.9` Scenario D): the child's limit exists because it was set before the parent switched to `enforce`, which is checked at proxy time, not at write time.

### Inbound requests

- Send >100 requests/min from child tenant.

### Expected behavior

- Effective rate limit for child is `min(parent_enforced, child)`.
- Requests beyond effective cap return `429` (gateway error source).
- If enabled, rate-limit headers reflect the effective limit.

## Scenario B: enforce holds down a chain

### Setup

Three tenants bind the same alias: top → middle → leaf.

- Top: `rate_limit.sharing=enforce`, 3/min.
- Middle: `rate_limit.sharing=inherit`, 2/min (stricter, so its value wins the `min`).
- Leaf: `rate_limit.sharing=private`, 100/min.

The leaf and the middle bind first, the top last, so no write is refused.

### Expected behavior

- The leaf's effective limit is 2/min: the middle's value won the `min` against an enforced limit, so it stays enforced and the leaf's `private` limit cannot replace it.
- The leaf's third request in the minute returns `429` (gateway error source).

## Scenario C: a child's route shadows the parent's route

### Setup

- Parent upstream `alias=api.example.com` shares something (example: `auth.sharing=inherit`), so the child's upstream merges with it.
- Parent route `POST /echo` with `plugins.sharing=enforce`: the required-headers guard requiring `x-tenant-guard`.
- Child upstream binds the same alias, and adds its own route `POST /echo` on it (example: `plugins.sharing=private` with a request-id transform).

The child's request matches its own route, which wins; the parent's route is the one the parent would select for the same request on its upstream.

### Expected behavior

- The parent route's enforced guard still applies: a child request without `x-tenant-guard` is rejected (`400`, `invalid_argument`, field violation `guard` / `REQUIRED_HEADER_MISSING`); with the header it reaches the upstream.
- An enforced parent route limit is not applied across route shadowing yet: it does not change the child's effective limit.
- The parent route's `inherit` and `private` fields do not apply to the child.
- An encoded `/` that would select another parent route that enforces something (`/echo%2Fadmin` under a parent route `/echo/admin`) is refused with `400` `INVALID_PROXY_PATH`.
