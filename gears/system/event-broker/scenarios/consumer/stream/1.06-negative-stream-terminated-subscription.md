# Stream with a terminated (reaped) subscription

Opening `:stream` with a subscription that existed but was reaped (session timeout) or LEFT returns `410`. The consumer must re-JOIN to continue.

## Setup

- Subscription `{sub_id}` existed but was reaped after its `session_timeout` elapsed with no poll/stream.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `410 Gone` (`PD`)

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
  "title": "Not Found",
  "status": 410,
  "detail": "subscription {sub_id} was terminated (reaped or left); re-JOIN to continue",
  "instance": "/v1/events:stream",
  "trace_id": "<request-trace-id>",
  "context": {
    "resource_type": "gts.cf.core.events.subscription.v1~",
    "resource_name": "{sub_id}"
  }
}
```

## Side effects

- No stream established.
- The group's committed cursors are unaffected — a fresh JOIN + SEEK resolving the committed cursor resumes where the reaped subscription left off.
