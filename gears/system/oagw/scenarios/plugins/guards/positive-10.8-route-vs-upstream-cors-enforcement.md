# Route vs. upstream CORS enforcement

This is a distinct merge axis from tenant-hierarchy CORS sharing (see [ADR-0006 § Hierarchical Configuration](../../../docs/ADR/0006-cors.md#hierarchical-configuration)) — it combines a route's own `cors` with its upstream's already-effective `cors`, within the same tenant. See [ADR-0006 § Route vs. Upstream CORS Enforcement](../../../docs/ADR/0006-cors.md#route-vs-upstream-cors-enforcement) for the full rule table.

## Scenario A: route `sharing: inherit` — route wins wholesale, origins unioned

```json
// Upstream
{ "cors": { "enabled": true, "allowed_origins": ["https://app.example.com"], "allowed_methods": ["GET"] } }
// Route
{ "cors": { "sharing": "inherit", "enabled": true, "allowed_origins": ["https://admin.example.com"], "allowed_methods": ["POST"] } }
```

Expected effective config for that route: `allowed_methods: ["POST"]` (the route's value — the upstream's `["GET"]` is **not** merged in), `allowed_origins: ["https://admin.example.com", "https://app.example.com"]` (unioned from both).

## Scenario B: route `sharing: private` — route CORS ignored

Same upstream as Scenario A. Route: `{ "cors": { "sharing": "private", "enabled": true, "allowed_origins": ["https://admin.example.com"] } }`.

Expected: the upstream's effective CORS applies unchanged (`allowed_methods: ["GET"]`, `allowed_origins: ["https://app.example.com"]`) — the route's `cors` block is skipped entirely, as if it weren't configured.

## Scenario C: upstream `sharing: enforce` — sticky, blocks every route override

Upstream: `{ "cors": { "sharing": "enforce", "enabled": true, "allowed_origins": ["https://app.example.com"] } }`. Route: `{ "cors": { "sharing": "inherit", "allowed_origins": ["https://evil-but-configured.com"] } }`.

Expected: the upstream's effective CORS applies unchanged, regardless of the route's own `sharing` value — an already-`enforce`d upstream config is sticky and unconditionally wins over *any* route-level CORS config, including `inherit`.

## What to check

- `sharing: enforce` on the *route* itself behaves the same as `private` here (skipped) — `enforce` only has an effect when set on an *ancestor* (upstream, or a tenant further up) that a descendant is prevented from overriding. A route has no descendants of its own to enforce against.
- The wholesale-replace-except-origins behavior in Scenario A is easy to misread as a full merge — it is not. Only `allowed_origins` is unioned; `allowed_methods`, `expose_headers`, and `allow_credentials` come from the route's config alone when `sharing: inherit`.
