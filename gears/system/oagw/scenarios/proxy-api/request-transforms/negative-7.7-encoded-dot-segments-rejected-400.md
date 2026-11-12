# Encoded dot-segments rejected (400)

## Setup

Create an HTTP upstream (example: `alias=api.example.com`) with a route `GET /v1` (`path_suffix_mode: append`).

## Request

```http
GET /api/oagw/v1/proxy/api.example.com/v1/%2e%2e/admin HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

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

The upstream receives nothing.

## The same response for

- `.%2e`, `%2E%2E`, `%2e%2e%2f`, double or triple encoding (`%252e%252e`)
- a dot-segment padded with NUL or space, or cut at `;`, `?` or `#`
- Unicode compatibility forms that fold to `.` (NFKC)
- a `;` path parameter (`/v1/a;b`)
- escapes that are not UTF-8 (`%ff`) and a `%` not followed by two hex digits (`/v1/%zz`)

## Not rejected

- Literal `.` and `..` segments are resolved before routing: `/v1/x/../y` is `/v1/y`.
- An encoded `/` inside a segment (`/v1/a%2Fb`) is forwarded as sent, unless reading it as `/` selects another route: that is also `400 INVALID_PROXY_PATH` ([3.12 Scenario F](../../management-api/routes/negative-3.12-route-prefix-matches-whole-segments.md#scenario-f-an-encoded--or--cannot-skip-a-narrower-route)).
