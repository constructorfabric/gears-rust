# Mid-stream backward SEEK is rejected

While a subscription's stream is open, SEEK is forward-only (`MAX(stored, requested)` per partition). A request that would move the cursor backward is rejected with `409 SeekBackwardNotAllowed`. (Backward repositioning is only possible pre-stream — see [positive-3.10](positive-3.10-pre-stream-any-value-in-range.md).)

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-1.1-cold-join-fresh-group.md). Subscription id `{sub_id}`; assigned `("acme.orders.v1", 0)`.
- Run [Pre-stream SEEK to exact offset](positive-3.3-pre-stream-seek-exact-offset.md) seeding cursor `500`.
- A [stream](../stream/positive-1.1-stream-multipart-frames.md) is currently open against `{sub_id}` (forward-only enforcement is active).

## Request

```http
POST /v1/subscriptions/{sub_id}/positions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "partition_positions": {
    "gts.cf.core.events.topic.v1~acme.orders.v1:0": 200
  }
}
```

## Expected response

- `409 Conflict` (`PD`)
- Problem Details body identifies the partition, current cursor, and requested value.

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
  "title": "Failed Precondition",
  "status": 409,
  "detail": "cannot move cursor for acme.orders.v1:0 backward from 500 to 200 while stream is open",
  "instance": "/v1/subscriptions/{sub_id}/positions",
  "trace_id": "<request-trace-id>",
  "context": {
    "violations": [
      {
        "type": "backward_seek",
        "subject": "gts.cf.core.events.topic.v1~acme.orders.v1:0",
        "description": "cannot move cursor backward from 500 to 200 while stream is open"
      }
    ]
  }
}
```

## Side effects

- `evbk_group_offsets(group, "acme.orders.v1", 0)` stays at `500` (unchanged — backward move rejected).
