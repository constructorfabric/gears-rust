# Request ID transform plugin injects/preserves correlation id

## Setup

Bind the builtin `request_id` transform plugin. Transforms run on `outbound_headers` *after* header passthrough has already been applied — by default (`passthrough: none`) the client's `X-Request-ID` would already be stripped before the plugin ever sees it, so Scenario B below also requires passing it through:

```json
{
  "headers": {
    "request": { "passthrough": "allowlist", "passthrough_allowlist": ["x-request-id"] }
  },
  "plugins": {
    "sharing": "private",
    "items": [
      {
        "plugin_ref": "gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1",
        "config": {}
      }
    ]
  }
}
```

## Scenario A: no client `x-request-id` → plugin injects a UUID

```http
POST /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
```

Expected: the upstream receives `x-request-id: <uuid>` — a freshly generated, valid UUID.

## Scenario B: client provides `x-request-id` → preserved unchanged

```http
POST /api/oagw/v1/proxy/<alias>/echo HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-Request-ID: e2e-trace-abc123
```

Expected: the upstream receives `x-request-id: e2e-trace-abc123` unchanged (not overwritten).

## What to check

- Correlation-id propagation is **not** automatic gateway behavior — it only happens when the `request_id` `TransformPlugin` is explicitly bound via `plugins.items[].plugin_ref`, same as any other transform plugin. See [positive-7.6](../../proxy-api/request-transforms/positive-7.6-request-correlation-headers-propagate-end-end.md) for the general end-to-end correlation-header contract this plugin fulfills.
- Binding the plugin without the passthrough allowlist above only produces Scenario A's behavior (generation) — the plugin would never observe the client's header in Scenario B and would generate a new id instead of preserving it. Passthrough (delivering the header to the transform stage) and the plugin (deciding to preserve vs. generate) are independent, both-required mechanisms for Scenario B.
- Neither scenario reflects the request id back in the client response or an audit record — the plugin's `on_response`/`on_error` hooks are no-ops.
