# 429 — rate limited

When a tenant exceeds a rate cap (publish quota, or the per-tenant JOIN cap), the broker returns `429` with a `Retry-After` header and `retry_after_secs` in the body.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{ "id": "f0000000-0000-0000-0000-000000000001", "type": "gts.cf.core.events.event.v1~acme.orders.created.v1", "topic": "gts.cf.core.events.topic.v1~acme.orders.v1", "tenant_id": "<tenant-uuid>", "source": "svc", "subject": "o", "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1", "occurred_at": "2026-05-29T10:11:00Z", "data": { "order_id": "o", "total_cents": 1 } }
```

## Expected response

- `429 Too Many Requests` (`PD`)
- `Retry-After: 30` header; `retry_after_secs: 30` in the body.

```http
HTTP/1.1 429 Too Many Requests
Retry-After: 30
Content-Type: application/problem+json

{
  "type": "https://errors.cf.core/events/rate-limit-exceeded",
  "title": "RateLimitExceeded",
  "status": 429,
  "detail": "tenant publish quota exceeded; retry after 30s",
  "instance": "/v1/events",
  "retry_after_secs": 30
}
```

## Notes

Also fires on `POST /v1/subscriptions` (per-tenant JOIN rate cap, default 60/min). Endpoint-specific instance: [events/negative-4.10](../events/negative-4.10-rate-limited.md).

<!-- No "## Side effects" — throttled request does nothing. -->
