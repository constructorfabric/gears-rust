# JOIN rejected for an unauthorized topic

A JOIN whose interest names a topic the calling principal cannot `consume` is rejected `403`.

## Setup

- Group `{group_id}` exists. Topic `acme.restricted.v1` is registered but the caller's principal lacks `consume` on it.

## Request

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "consumer_group": "{group_id}",
  "client_agent": "worker/1.0.0",
  "interests": [ { "topic": "gts.cf.core.events.topic.v1~acme.restricted.v1", "tenant_id": "<tenant-uuid>", "types": ["gts.cf.core.events.event.v1~acme.restricted.*"] } ]
}
```

## Expected response

- `403 Forbidden` (`PD`)
- Wire-level code `TopicNotAuthorized`; the offending topic is in the body.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
  "title": "Permission Denied",
  "status": 403,
  "detail": "principal lacks 'consume' on gts.cf.core.events.topic.v1~acme.restricted.v1",
  "instance": "/v1/subscriptions",
  "trace_id": "<request-trace-id>",
  "context": {
    "reason": "principal lacks 'consume' on gts.cf.core.events.topic.v1~acme.restricted.v1"
  }
}
```

## Side effects

- No subscription is created (`evbk_subscription` gains no row); group topology unchanged.
