# Stream with an unknown subscription

Opening `:stream` with a subscription id that never existed returns `404`.

## Request

```http
GET /v1/events:stream?subscription_id=99999999-8888-7777-6666-555555555555 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `404 Not Found` (`PD`)

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "subscription 99999999-8888-7777-6666-555555555555 not found",
  "instance": "/v1/events:stream",
  "trace_id": "<request-trace-id>",
  "context": {
    "resource_type": "gts.cf.core.events.subscription.v1~",
    "resource_name": "99999999-8888-7777-6666-555555555555"
  }
}
```

<!-- No "## Side effects" — no stream established; the consumer must re-JOIN. -->
