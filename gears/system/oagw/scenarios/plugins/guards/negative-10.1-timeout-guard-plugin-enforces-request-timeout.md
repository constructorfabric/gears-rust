# Gear-level request timeout enforced (not a guard plugin)

## Setup

Request timeout is **gear-level configuration**, not a per-route/per-upstream `GuardPlugin` binding: the `gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1` identifier is cataloged in the types registry for discoverability only and cannot be bound via `plugins.items[].plugin_ref` (see [DESIGN.md](../../../docs/DESIGN.md#plugin-system)). The actual enforcement is the gear's `proxy_timeout_secs` config value (default `30`), applied uniformly to every proxied request.

For this scenario, run OAGW with `proxy_timeout_secs` set low (example: `2`) and point the route at an upstream that intentionally sleeps past that duration.

## Inbound request

Send a request that the upstream intentionally delays beyond the timeout.

```http
GET /api/oagw/v1/proxy/<alias>/slow HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `504 Gateway Timeout`
- `Content-Type: application/problem+json`
- `X-OAGW-Error-Source: gateway`
- `type` corresponds to request timeout (`...timeout.request...`) or configured timeout error type.

## What to check

- Upstream call is cancelled/terminated.
- Audit log records failure with error_type timeout.
