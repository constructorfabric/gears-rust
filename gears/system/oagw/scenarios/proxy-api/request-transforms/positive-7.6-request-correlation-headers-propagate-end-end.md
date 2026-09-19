# Request-ID header reaches the upstream

## Setup

By default `headers.request.passthrough` is `none`, so no inbound header (including `X-Request-ID`) reaches the upstream unless explicitly allowed. This scenario requires:

```json
{
  "headers": {
    "request": { "passthrough": "allowlist", "passthrough_allowlist": ["x-request-id"] }
  }
}
```

## Scenario A: client provides X-Request-ID

```http
GET /api/oagw/v1/proxy/httpbin.org/get HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
X-Request-ID: req-abc-123
```

Expected:
- Upstream receives `X-Request-ID: req-abc-123`, forwarded as-is by the passthrough allowlist above. No `TransformPlugin` binding is required for this case.

## Scenario B: client does not provide X-Request-ID

Same request without `X-Request-ID`. The passthrough allowlist above has nothing to forward, so an id must be generated instead.

Expected:
- A generated `x-request-id` reaches the upstream — **only if** the builtin `request_id` `TransformPlugin` (`gts.cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1`) is explicitly bound via `plugins.items[].plugin_ref`. This is not automatic core gateway behavior; without the plugin bound, no `x-request-id` is injected. See [positive-11.9](../../plugins/transforms/positive-11.9-request-id-transform-injects-correlation-id.md) for the plugin's own UUID-generation/preservation contract.

## What to check

- Neither scenario claims the request id is reflected back in the client response or written to an audit log: the `request_id` plugin's `on_response`/`on_error` hooks are no-ops (see [positive-11.9](../../plugins/transforms/positive-11.9-request-id-transform-injects-correlation-id.md)), and no other mechanism in `oagw/src` reflects `x-request-id` into the response or a distinct audit record. Only upstream-bound propagation (Scenario A) or generation (Scenario B) is a verified behavior.
- If the `request_id` plugin is bound *without* the passthrough allowlist above, Scenario A's client-supplied value is already stripped before the transform runs (transforms execute on `outbound_headers`, which passthrough has already filtered) — the plugin then sees no existing header and **generates** a new id instead of preserving the client's. Passthrough and the transform plugin are independent mechanisms; getting Scenario A's "preserve the client value" behavior requires the passthrough allowlist, not the plugin.
