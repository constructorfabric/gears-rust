# Batch rejected for exceeding the size cap

A `:batch` may carry at most 100 events. A larger batch is rejected `413`.

## Setup

- Topic `acme.orders.v1` registered.

## Request

```http
POST /v1/events:batch HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "events": [ "...101 event objects..." ]
}
```

(The `events` array carries 101 entries; abbreviated here.)

## Expected response

- `413 Content Too Large` (`PD`)

```json
{
  "type": "https://errors.cf.core/events/batch-too-large",
  "title": "BatchTooLarge",
  "status": 413,
  "detail": "batch carries 101 events; maximum is 100",
  "instance": "/v1/events:batch"
}
```

## Side effects

- No event from the batch is admitted.
