# Publish rejected by the per-tenant rate cap

When a tenant exceeds its publish quota, the broker rejects with `429 RateLimitExceeded` and a `Retry-After` header.

## Setup

- The caller's tenant has exhausted its publish quota for the current window.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "id": "f0000000-0000-0000-0000-000000000001",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-throttled",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-05-29T10:11:00Z",
  "data": { "order_id": "order-throttled", "total_cents": 50 }
}
```

## Expected response

- `429 Too Many Requests` (`PD`)
- `Retry-After: 30` header.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~",
  "title": "Resource Exhausted",
  "status": 429,
  "detail": "tenant publish quota exceeded; retry after 30s",
  "instance": "/v1/events",
  "trace_id": "<request-trace-id>",
  "context": {
    "violations": [
      {
        "subject": "publish-quota",
        "description": "tenant publish quota exceeded",
        "retry_after_seconds": 30
      }
    ]
  }
}
```

## Side effects

- No event is admitted.
- `metric publish_rate_limited incremented by 1` for the tenant.
