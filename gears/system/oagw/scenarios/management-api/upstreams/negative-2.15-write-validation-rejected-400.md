# Write-time validation rejected (400)

Values the gateway could never serve are rejected on create and update, through the Management API, the SDK and types-registry provisioning alike. Nothing is stored. Upstreams and routes stored before these checks keep serving; updating one requires fixing it first.

## Scenario A: upstream fields

### Request (example: port 0)

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "10.0.0.1", "port": 0 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "internal-api"
}
```

### Expected response

A rule without a field is a format error: `detail` and `context.format` carry the same text. `trace_id` is present when the request is traced.

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "endpoint[0] port must be between 1 and 65535",
  "instance": "/oagw/v1/upstreams",
  "trace_id": "<trace id>",
  "context": {
    "format": "endpoint[0] port must be between 1 and 65535",
    "resource_type": "gts.cf.core.oagw.proxy.v1~"
  }
}
```

### The same for

Each is a format error like the one above, with this text in `detail` and `context.format`:

| Body | `detail` |
|---|---|
| `"tags": ["LLM"]` or `["a.b"]` | `tags[0] must match ^[a-z0-9_-]+$` |
| `"alias": "-api"` or `"api:"` | `alias must not start or end with '.', '-' or ':'` |
| `"rate_limit"` with `sustained.rate`, `burst.capacity` or `cost` of `0` | `rate_limit.sustained.rate must be at least 1` (`burst.capacity`, `cost` likewise) |
| `"rate_limit"` whose `cost` exceeds the bucket capacity: `burst.capacity` for a token bucket with a burst, else `sustained.rate` (a sliding window ignores `burst`) | `rate_limit.cost (6) must not exceed rate_limit.burst.capacity (5)` (or `rate_limit.sustained.rate (5)`) |
| `"rate_limit"` with a shared budget whose `total` is below `cost`: `{"cost": 4, "sustained": {"rate": 10, ...}, "budget": {"mode": "shared", "total": 3}}` | `rate_limit.cost (4) must not exceed rate_limit.budget.total (3)` |
| `"circuit_breaker"` with `failure_threshold`, `timeout_seconds` or `success_threshold` of `0`, `enabled` or not | `circuit_breaker.failure_threshold must be at least 1` (`timeout_seconds`, `success_threshold` likewise) |
| `"circuit_breaker"` with `failure_threshold` above 1000, `timeout_seconds` above 86400 or `success_threshold` above 100, `enabled` or not | `circuit_breaker.failure_threshold must be at most 1000` (`timeout_seconds` … `86400`, `success_threshold` … `100`) |
| a child's `"rate_limit"` that wins the merge (`{"cost": 4, "sustained": {"rate": 10, "window": "minute"}}`) under an ancestor's shared budget of `total: 3` (`rate: 100`) | `rate_limit.cost (4) must not exceed budget.total (3) of the shared budget an ancestor's upstream defines for this alias` |

The shared pool is charged the upstream's effective cost (see [18.7](../../rate-limiting/positive-18.7-budget-modes-behave-specified.md) scenario B), so the check uses the merged limit: a child limit that loses the merge to the ancestor's, or a non-enforced `private` one, is not charged to the pool and is accepted. A route's `cost` goes to the route's bucket only.

`_` may start or end an alias (`_api_` is accepted). Trailing dots are stripped first: `api.` is stored as `api`. A port above 65535 does not fit the field and fails request decoding with `422`.

The same rules apply to `PUT /api/oagw/v1/upstreams/{id}`: the update is refused with the same body and the stored upstream is unchanged. The alias rule is the exception: an update cannot change the alias, so it applies to new aliases only.

## Scenario B: route fields

### Request (example: no methods)

```http
POST /api/oagw/v1/routes HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "upstream_id": "gts.cf.core.oagw.upstream.v1~<upstream-uuid>",
  "match": { "http": { "methods": [], "path": "/v1/chat" } }
}
```

### Expected response

A format error, as in scenario A: `400 Bad Request`, `type` = `...invalid_argument...`, `detail` and `context.format` = `match.http.methods must list at least one method`, `context.resource_type` = `gts.cf.core.oagw.proxy.v1~`.

### The same for

| Body | Response |
|---|---|
| `"path": ""` or `"path": "v1/chat"` | a field violation (shape as in scenario C): `field` = `match.http.path`, `reason` = `INVALID_PROXY_PATH`, `description` = ``route path must start with `/` `` |
| `"rate_limit"` with a `0` rate, capacity or cost | a format error: `rate_limit.<field> must be at least 1` |
| `"rate_limit"` whose `cost` exceeds the bucket capacity | a format error, as for upstreams: `rate_limit.cost (6) must not exceed rate_limit.burst.capacity (5)` |

## Scenario C: plugin references

### Request (example: a catalog-only auth plugin)

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "api.example.com", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": { "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1" }
}
```

### Expected response

```http
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "Request validation failed",
  "instance": "/oagw/v1/upstreams",
  "trace_id": "<trace id>",
  "context": {
    "field_violations": [
      {
        "field": "auth.type",
        "reason": "NOT_IMPLEMENTED",
        "description": "auth plugin 'gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1' is not implemented; use one of [\"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1\", \"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1\", \"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1\", \"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1\", \"gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1\"]"
      }
    ],
    "resource_type": "gts.cf.core.oagw.proxy.v1~"
  }
}
```

There is one violation: the first rule that fails.

### The same for

| Body | Field, reason |
|---|---|
| `auth.type` unknown, catalog-only or a custom UUID id | `auth.type`, `NOT_IMPLEMENTED` |
| `plugins.items[].plugin_ref` unknown, catalog-only (for example `...transform_plugin.v1~cf.core.oagw.logging.v1`) or a custom UUID id | `plugins.items.plugin_ref`, `NOT_IMPLEMENTED` |
| an auth plugin in `plugins.items` | `plugins.items.plugin_ref`, `VALIDATION` |
| a guard or transform plugin in `auth.type` | `auth.type`, `VALIDATION` |

Implemented plugins: auth `noop`, `apikey`, `oauth2_client_cred`, `oauth2_client_cred_basic`, `aws_sigv4`; guard `required_headers`; transform `request_id`.

## Scenario D: plain HTTP upstream

With the gear option `allow_http_upstream: false` (the default), create an upstream whose endpoint has `"scheme": "http"`.

Expected: `400 Bad Request`, a field violation (shape as in scenario C) on `server.endpoints.scheme` with reason `HTTP_UPSTREAM_FORBIDDEN`. An upstream stored earlier with such an endpoint is refused by the proxy with the same violation, before its auth plugin runs.

The E2E environment enables plain HTTP for its mock upstream, so this scenario is covered by Rust tests only (`http_endpoints_need_allow_http_upstream`, `http_upstream_is_refused_before_auth`).
