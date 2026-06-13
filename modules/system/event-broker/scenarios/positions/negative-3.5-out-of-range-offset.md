# SEEK rejects an out-of-range offset

An integer SEEK value is interpreted as the last-processed offset and must fall within `[RF - 1, HWM]`. A value below `RF - 1` is rejected with `400 InvalidInitialPosition`.

> Discharges the consumer-seek-semantics deferred test §5.4 (out-of-range rejection path).

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md). Subscription id `{sub_id}`; assigned `("acme.orders.v1", 0)`.
- Partition `("acme.orders.v1", 0)` has `RF = 100`, `HWM = 5000` → valid range `[99, 5000]`.

## Request

```http
POST /v1/subscriptions/{sub_id}/positions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "partition_positions": {
    "gts.cf.core.events.topic.v1~acme.orders.v1:0": 5
  }
}
```

## Expected response

- `400 Bad Request` (`PD`)
- Problem Details body identifies the offending partition, the requested value, and the valid range.

```json
{
  "type": "https://errors.cf.core/events/invalid-initial-position",
  "title": "InvalidInitialPosition",
  "status": 400,
  "detail": "offset 5 for acme.orders.v1:0 is below the valid range [99, 5000]",
  "instance": "/v1/subscriptions/{sub_id}/positions",
  "topic": "gts.cf.core.events.topic.v1~acme.orders.v1",
  "partition": 0,
  "requested": "5",
  "valid_range": [99, 5000]
}
```

## Side effects

- `evbk_group_offsets(group, "acme.orders.v1", 0)` is absent — the rejected request commits nothing (validation is per-request atomic; no partition in the body is applied if any is out of range).
