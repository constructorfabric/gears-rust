# Principal without topic:consume permission → 403

JOINing a subscription requires the `consume` action on the topic (and on each event type in the `interests[]`). A principal lacking `consume` is rejected at JOIN time — no subscription is created.

## Setup

- Group `{group_id}` exists (anonymous, owned by the caller's tenant).

## Request

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <token-without-consume-grant>
Content-Type: application/json

{
  "consumer_group": "{group_id}",
  "client_agent": "report-runner/1.0.0",
  "session_timeout": "PT30S",
  "interests": [
    {
      "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "tenant_id": "<tenant-uuid>",
      "types": ["gts.cf.core.events.event.v1~acme.orders.*"]
    }
  ]
}
```

## Expected response

- `403 Permission Denied`

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "detail": "principal lacks consume permission on topic gts.cf.core.events.topic.v1~acme.orders.v1",
  "instance": "/v1/subscriptions",
  "trace_id": "<request-trace-id>",
  "context": {
    "reason": "missing grant: gts.cf.core.events.topic.v1~acme.orders.v1:consume"
  }
}
```

## Side effects

- No subscription is created. No group state is modified.
