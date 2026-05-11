# JOIN rejected for too many interests

A subscription may declare at most 64 interests (`MAX_INTERESTS_PER_SUBSCRIPTION`). Exceeding the cap is rejected `400 TooManyInterests`.

## Setup

- Group `{group_id}` exists.

## Request

```http
POST /v1/subscriptions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "consumer_group": "{group_id}",
  "client_agent": "worker/1.0.0",
  "interests": [ "...65 interest entries..." ]
}
```

(The `interests` array carries 65 entries; abbreviated here.)

## Expected response

- `400 Bad Request` (`PD`)
- Wire-level code `TooManyInterests`.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
  "title": "Invalid Argument",
  "status": 400,
  "detail": "interests count 65 exceeds the maximum of 64",
  "instance": "/v1/subscriptions",
  "trace_id": "<request-trace-id>",
  "context": {
    "constraint": "interests count 65 exceeds the maximum of 64"
  }
}
```

## Side effects

- No subscription is created.

> Sibling caps share this validation envelope (same `400 PD`, different `title`): `TooManyTypes` (>32 per interest), `ExpressionTooLong` (>4096 bytes), `BadTypePattern` (wildcard violates GTS §10), `NoTypesMatched`, `TypeNotInTopic`, `UnknownFilterEngine`, `CompiledFilterTooLarge` (>64 KiB). Each is its own follow-up scenario; this file is the representative.
