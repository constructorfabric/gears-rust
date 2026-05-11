# Tenant B cannot JOIN tenant A's anonymous consumer group → 403

Anonymous consumer groups are tenant-bound at creation: the creating tenant's principal is the owner. A different tenant trying to JOIN with the same group identifier is rejected — the broker validates `caller.tenant_id == group.owner_tenant_id`.

## Setup

- Tenant A created group `gts.cf.core.events.consumer_group.v1~{uuid}` via `POST /v1/consumer_groups`. The group is owned by tenant A.
- Tenant B somehow obtained the group's GTS identifier (e.g., it appeared in a log, was guessed, etc.).

## Request (tenant B)

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-b-token>
Content-Type: application/json

{
  "consumer_group": "gts.cf.core.events.consumer_group.v1~{uuid}",
  "client_agent": "adversary/1.0.0",
  "session_timeout": "PT30S",
  "interests": [
    {
      "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
      "tenant_id": "<tenant-b-uuid>",
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
  "detail": "consumer group is owned by a different tenant",
  "instance": "/v1/subscriptions",
  "trace_id": "<request-trace-id>",
  "context": {
    "reason": "anonymous groups are tenant-private; cross-tenant sharing requires a named group with explicit :consume grants"
  }
}
```

## Side effects

- No subscription is created. No group state is modified.
- Tenant A's group and any active subscriptions within it are unaffected.
- The error does not reveal tenant A's identity or any details of the group's contents.
