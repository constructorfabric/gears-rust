# Plugin sharing modes across tenant hierarchy

## Scenario A: parent plugins `inherit`, child extends

1. Parent tenant creates upstream with:

```json
{
  "alias": "api.openai.com",
  "plugins": {
    "sharing": "inherit",
    "items": [
      "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1"
    ]
  }
}
```

2. Child tenant binds to the upstream and adds plugins:

```json
{
  "plugins": {
    "items": [
      "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.metrics.v1"
    ]
  }
}
```

Expected effective chain order:
- `[parent.logging, child.metrics]`

## Scenario B: parent plugins `enforce`

Parent:
- `plugins.sharing=enforce` with the required-headers guard requiring `x-tenant-guard`.

A child cannot remove or replace an enforced plugin; it can only add its own:
- A child binding the same `plugin_ref` with another config (`sharing=inherit`) does not replace it: the parent's config stays in effect.
- A `private` child keeps the enforced items and appends its own, also below a middle tenant whose plugins are `inherit` (root `enforce` → middle `inherit` → leaf `private`).
- The binding is accepted (`201`); the protection is applied when the request is merged.

Expected:
- A child's request without `x-tenant-guard` is rejected by the parent's guard: `400`, `invalid_argument`, field violation `guard` / `REQUIRED_HEADER_MISSING`.
- The effective plugins stay `enforce`.

## Scenario C: parent plugins `private`

Parent:
- `plugins.sharing=private`

Expected:
- Child does not inherit parent plugins.

## Scenario D: route plugins under enforced upstream plugins

Parent upstream:
- `plugins.sharing=enforce` with the required-headers guard requiring `x-tenant-guard`.

The child binds the alias and adds a route on its own upstream with `plugins.sharing=private` (a request-id transform).

Expected:
- The route keeps the enforced guard and appends its own plugins: a request without `x-tenant-guard` is rejected (`400`, `REQUIRED_HEADER_MISSING`).
- A `private` route still replaces the upstream plugins nobody enforces.
- An `inherit` or `enforce` route naming an enforced `plugin_ref` does not replace its config.

## Scenario E: a child enforces the same plugin as the parent's route

Parent upstream:
- Shares something (example: `auth.sharing=inherit`), so the child's upstream merges with it.
- Route `POST /echo` with `plugins.sharing=enforce`: the required-headers guard requiring `x-tenant-guard`.

The child binds the alias with upstream `plugins.sharing=enforce` naming the same guard `plugin_ref` with `config={}`, and:
1. sends `POST /echo` without a route of its own (the parent's route is matched);
2. then adds its own route `POST /echo` (it shadows the parent's route).

An ancestor route's enforced plugins apply at its owner's level, before anything a descendant sets. For a `plugin_ref` enforced at several levels, the level closest to the root keeps its config; within a tenant, its upstream comes before its route.

Expected, in both steps:
- The parent route's guard config stays in effect: a child request without `x-tenant-guard` is rejected (`400`, `invalid_argument`, field violation `guard` / `REQUIRED_HEADER_MISSING`); with the header it reaches the upstream.
- The SDK's `resolve_proxy_target` returns the parent route's guard in the effective plugins, and so needs `read` on the parent's route: without it the call is refused with `403` `permission_denied` on the route resource, as for the returned route. The proxy needs `proxy:invoke` only.
