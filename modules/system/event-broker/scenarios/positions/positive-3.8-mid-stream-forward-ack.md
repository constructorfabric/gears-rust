# Mid-stream forward ack

While a stream is open, the consumer advances its cursor past processed events by SEEKing a higher integer offset. Forward-only enforcement (`MAX(stored, requested)`) applies; an advancing value is accepted. (A backward value would be rejected — see [negative-3.7](negative-3.7-mid-stream-backward-seek.md).)

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md) → `{sub_id}`, assigned `("acme.orders.v1", 0)`.
- Run [Pre-stream SEEK to exact offset](positive-3.3-pre-stream-seek-exact-offset.md) seeding cursor `42`.
- A [stream](../transports/positive-6.1-stream-multipart-frames.md) is open against `{sub_id}`; the consumer has processed through offset `510`.

## Request

```http
POST /v1/subscriptions/{sub_id}/positions HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "partition_positions": {
    "gts.cf.core.events.topic.v1~acme.orders.v1:0": 510
  }
}
```

## Expected response

- `200 OK`

```json
{
  "partition_positions": {
    "gts.cf.core.events.topic.v1~acme.orders.v1:0": 510
  }
}
```

## Side effects

- `evbk_group_offsets(group, "acme.orders.v1", 0)` advances from `42` to `510`.
- On re-JOIN / reconnect, a SEEK resolving the committed cursor resumes from offset `511` (`Cursor + 1`); offsets `43`–`510` are not redelivered.
- A subsequent ack with a *lower* offset while the stream remains open is rejected `409 SeekBackwardNotAllowed` (forward-only).
