# Transform plugin: on_error for gateway errors

`on_error` runs only for gateway-originated errors of the upstream exchange: connection failure, timeout, protocol error, request body limit, response guard rejection. Errors raised before the request is forwarded (validation, auth, request guards, rate limit, CORS) return without it. A WebSocket upgrade does not run it: a failed upgrade returns its gateway error without `on_error`. An upstream error response is not a gateway error: it goes through `on_response`.

## Scenario A: on_error handles a gateway error

### Setup

Create transform plugin with `on_error` phase:

```starlark
def on_error(ctx):
    if ctx.error and not ctx.error.upstream:
        return ctx.respond(400, "{\"error\":\"bad_request\"}")
    return ctx.next()
```

Attach plugin.

### Trigger a gateway error

Example: the upstream endpoint refuses the connection or does not answer within the request timeout.

```http
GET /api/oagw/v1/proxy/<alias>/resource HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

### Expected behavior

- Error is handled by plugin and replaced by custom response.
- Lock expected semantics for status and content-type.

Today a built-in `on_error` gets a copy of the error type, the status the client gets and the detail, and cannot change the response; a custom response (`ctx.respond`) waits for Starlark plugins.

## Scenario B: an upstream error response reaches the client as sent

### Setup

Attach a transform plugin (for example the built-in `request_id` transform) to an upstream whose endpoint answers `500` with a JSON body.

### Trigger

```http
GET /api/oagw/v1/proxy/<alias>/error/500 HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

### Expected behavior

- `on_response` runs, as for any upstream response; `on_error` does not.
- The client gets the upstream's response: status `500`, its `Content-Type` and its body unchanged, not a Problem Details body.
- `X-OAGW-Error-Source: upstream`.
- Metrics count the request in `oagw_requests_total` with status `500`, not in `oagw_errors_total`.

A built-in `on_error` cannot change the response, so the client's response does not show whether it ran: the E2E test checks the response, and the unit test `on_error_runs_only_for_gateway_errors` checks that `on_error` does not run.
