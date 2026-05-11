# Principal without topic:produce permission → 403

Publishing to a topic requires the `produce` action on that topic's GTS resource. A principal with a valid token but no `produce` grant is rejected before any ingest logic runs.

## Request

```http
POST /v1/events HTTP/1.1
Host: broker.example.com
Authorization: Bearer <token-without-produce-grant>
Content-Type: application/json

{
  "id": "11111111-0000-0000-0000-000000000001",
  "type": "gts.cf.core.events.event.v1~acme.orders.created.v1",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "tenant_id": "<tenant-uuid>",
  "source": "order-service",
  "subject": "order-1",
  "subject_type": "gts.cf.core.events.subject.v1~acme.order.v1",
  "occurred_at": "2026-06-15T10:00:00Z",
  "data": { "order_id": "order-1" }
}
```

## Expected response

- `403 Permission Denied`

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "detail": "principal lacks produce permission on topic gts.cf.core.events.topic.v1~acme.orders.v1",
  "instance": "/v1/events",
  "trace_id": "<request-trace-id>",
  "context": {
    "reason": "missing grant: gts.cf.core.events.topic.v1~acme.orders.v1:produce"
  }
}
```

## Side effects

- No event is stored or enqueued.
- The response does not reveal whether the topic exists, to prevent existence probing by unauthorized principals.
