# Tags support discovery and filtering

## Setup

Create upstream with tags.

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "api.openai.com", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "api.openai.com",
  "tags": ["openai", "llm", "chat"]
}
```

## List with an unsupported parameter (today)

Tag filtering is not implemented yet. The list endpoints accept `limit` and `offset` (routes also `upstream_id`); any other parameter is refused rather than ignored.

```http
GET /api/oagw/v1/upstreams?$filter=tags has 'llm' HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected response:

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
        "field": "$filter",
        "reason": "UNSUPPORTED_QUERY_PARAM",
        "description": "query parameter '$filter' is not supported; supported: limit, offset"
      }
    ],
    "resource_type": "gts.cf.core.oagw.upstream.v1~"
  }
}
```

`trace_id` is present when the request is traced. The same holds for `$orderby`, `tags` and any other key: one violation per key, in the order sent. On `GET /api/oagw/v1/routes` the body is the same with `"instance": "/oagw/v1/routes"`, `"resource_type": "gts.cf.core.oagw.route.v1~"` and `supported: upstream_id, limit, offset`.

## List/filter (planned)

Once filtering is implemented, the request above returns `200` and the results include the upstream with `tags` containing `llm`.
