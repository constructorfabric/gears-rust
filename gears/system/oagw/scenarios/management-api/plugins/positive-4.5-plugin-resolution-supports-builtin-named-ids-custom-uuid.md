# Plugin resolution: builtin named ids vs custom UUID ids

## Scenario A: attach builtin plugin

Attach builtin plugin by named id (example from design):
- `gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1`

Catalog-only ids (for example `...logging.v1`) are not implemented and are rejected on write; see [negative-2.15](../upstreams/negative-2.15-write-validation-rejected-400.md).

Expected:
- Upstream/route update succeeds.
- Proxy invocation succeeds and plugin behavior is observed (e.g., log emitted).

## Scenario B: attach missing custom plugin UUID

Attach custom plugin id that does not exist in DB:
- `gts.cf.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000`

Expected on write: custom plugins are not implemented yet, so the create or update is rejected with `400 Bad Request`, `type` = `...invalid_argument...`, field violation reason `NOT_IMPLEMENTED`. The same holds for an unknown named id.

A binding stored before this check cannot be created through the API any more. Today the proxy fails such a request with 401 `AUTH_PLUGIN_NOT_FOUND` (auth), 500 (guard), or skips the plugin (transform); #4985 tracks the expected behavior:
- `503 Service Unavailable`
- `Content-Type: application/problem+json`
- Error type `gts.cf.core.errors.err.v1~cf.oagw.plugin.not_found.v1`
- `X-OAGW-Error-Source: gateway`
