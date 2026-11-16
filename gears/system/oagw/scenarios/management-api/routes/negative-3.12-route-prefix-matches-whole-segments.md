# Route prefix matches whole path segments

## Setup

Create an HTTP upstream (example: `alias=api.example.com`) and one route on it:

```http
POST /api/oagw/v1/routes HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "upstream_id": "gts.cf.core.oagw.upstream.v1~<upstream-uuid>",
  "match": {
    "http": {
      "methods": ["GET"],
      "path": "/v1/chat",
      "path_suffix_mode": "append"
    }
  }
}
```

## Scenario A: a longer segment name does not match

```http
GET /api/oagw/v1/proxy/api.example.com/v1/chatty HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected response:

- `404 Not Found`, `Content-Type: application/problem+json`, `X-OAGW-Error-Source: gateway`
- `type` = `...not_found...`, `resource_type` = the route schema

These requests do match the route and reach the upstream (`X-OAGW-Error-Source: upstream`):

| Request path after the alias | Outbound path |
|---|---|
| `/v1/chat` | `/v1/chat` |
| `/v1/chat/completions` | `/v1/chat/completions` |
| `/v1/chat.json` | `/v1/chat.json` |
| `/v1/chat:stream` | `/v1/chat:stream` |

The character after the prefix must not continue a segment name (ASCII letter or digit, `-`, `_`, `~`).

## Scenario B: encoded unreserved characters match

```http
GET /api/oagw/v1/proxy/api.example.com/v1/ch%61t HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected: the request matches route `/v1/chat` (encoded unreserved characters are decoded before matching) and reaches the upstream.

## Scenario C: route `/` matches the bare alias

Add a route with `"path": "/"` on the upstream, then:

```http
GET /api/oagw/v1/proxy/api.example.com HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected: the request matches route `/` and reaches the upstream.

## Scenario D: a prefix no request could match is rejected

Create a route whose `path` is `/v1//chat`, `/v1//`, `//`, `/v1/./chat`, `/v1;x`, `/v1/%2e%2e`, `/v1/%zz`, or does not start with `/` (`v1/chat`).

Expected response:

- `400 Bad Request`, `Content-Type: application/problem+json`, `type` = `...invalid_argument...`
- No route is stored.

## Scenario E: a trailing `/` on the route is not forwarded before a segment suffix

Create a route with `"path": "/v1/admin/"`, then:

```http
GET /api/oagw/v1/proxy/api.example.com/v1/admin.json HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected: the request matches route `/v1/admin/` (the trailing `/` gets the same boundary rule) and the upstream receives `/v1/admin.json`, not `/v1/admin/.json`. A request for `/v1/admin/x` is forwarded as `/v1/admin/x`.

## Scenario F: an encoded `/` or `\` cannot skip a narrower route

Create routes `/v1` and `/v1/admin` on the upstream, then:

```http
GET /api/oagw/v1/proxy/api.example.com/v1%2Fadmin HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

As sent, the path is under `/v1` (`%2F` ends the segment). An upstream that decodes `%2F` before routing would serve `/v1/admin` under the `/v1` route's policy.

Expected response:

```http
HTTP/1.1 400 Bad Request
X-OAGW-Error-Source: gateway
Content-Type: application/problem+json

{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "context": {
    "field_violations": [
      { "field": "path", "reason": "INVALID_PROXY_PATH", "description": "..." }
    ]
  }
}
```

The description does not name the other route. The upstream receives nothing.

The same response for `%2f`, `%5C`, `\`, `%EF%BC%8F`, `%EF%BC%BC` and `%EF%B9%A8` (fullwidth and small solidi, which NFKC folds to `/` and `\`) in place of `%2F`, for route `/` in place of `/v1`, for a path that matches no route as sent but one once decoded (only route `/v1/admin`), and the other way round: with routes `/v3` and `/v3/a%2Fb`, a request for `/v3/a/b`.

Not rejected, because reading the separator as `/` selects the same route:

| Request path after the alias | Route | Outbound path |
|---|---|---|
| `/v1/admin%2Fx` | `/v1/admin` | `/v1/admin%2Fx` |
| `/v1%2Fother` | `/v1` | `/v1%2Fother` |

Two routes on one upstream whose prefixes are equal once the separators are read as `/` (`/v3/a/b` and `/v3/a%2Fb`, also `/v3/a/b/` and `/v3/a%2Fb`) leave one of them unreachable: the decoded ranking picks one (the longer prefix, then the higher priority, then the lower route ID), and every request under the other is rejected with this 400. Keep only one of them.
