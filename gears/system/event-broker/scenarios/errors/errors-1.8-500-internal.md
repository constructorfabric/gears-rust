# 500 — internal error

An unexpected broker invariant failure returns `500` with a Problem Details body that leaks NO internal detail (no stack trace, no internal hostnames). The `instance` correlates to the failed request for log lookup.

## Request

Any request that trips an internal invariant (illustrative — not a contract a client can deliberately trigger):

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{ "id": "...", "type": "gts.cf.core.events.event.v1~acme.orders.created.v1", "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "tenant_id": "<tenant-uuid>", "source": "svc", "subject": "o", "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1", "occurred_at": "2026-05-29T10:12:00Z", "data": { "order_id": "o", "total_cents": 1 } }
```

## Expected response

- `500 Internal Server Error` (`PD`)
- Generic `detail`; no internals exposed.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
  "title": "Internal",
  "status": 500,
  "detail": "an internal error occurred; the request was not processed",
  "instance": "/v1/events#req-7c9f2a",
  "trace_id": "<request-trace-id>",
  "context": {}
}
```

## Notes

- `instance` carries a correlation token (`#req-...`) operators can grep in logs; the actual failure detail lives only in server-side logs.
- A `PartitionHashMismatch` (a CI-bug indicator, per the SDK error model) surfaces here as `500`, not as a distinct caller-facing code.

<!-- No "## Side effects" — on a 500 the broker makes no durability guarantee for this request; the client retries (idempotently). -->
