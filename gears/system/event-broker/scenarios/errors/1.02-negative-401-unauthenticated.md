# 401 — unauthenticated

A request with a missing or invalid bearer token is rejected `401` before any resource logic runs.

## Request

```http
POST /v1/consumer_groups HTTP/1.1
Host: broker.example.com
Content-Type: application/json

{ "description": "no token" }
```

(No `Authorization` header.)

## Expected response

- `401 Unauthorized` (`PD`)

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~",
  "title": "Unauthenticated",
  "status": 401,
  "detail": "missing or invalid bearer token",
  "instance": "/v1/consumer_groups",
  "trace_id": "<request-trace-id>",
  "context": {}
}
```

<!-- No "## Side effects" — rejected before any resource logic. -->
