# LEAVE an unknown subscription

Terminating a subscription id that never existed (or was already reaped/deleted) returns `404`.

## Request

```http
DELETE /v1/subscriptions/99999999-8888-7777-6666-555555555555 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
```

## Expected response

- `404 Not Found` (`PD`)

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 404,
  "detail": "subscription 99999999-8888-7777-6666-555555555555 not found",
  "instance": "/v1/subscriptions/99999999-8888-7777-6666-555555555555",
  "trace_id": "<request-trace-id>",
  "context": {
    "resource_type": "gts.cf.core.events.subscription.v1~",
    "resource_name": "99999999-8888-7777-6666-555555555555"
  }
}
```

<!-- No "## Side effects" — deleting a non-existent subscription changes nothing. -->
