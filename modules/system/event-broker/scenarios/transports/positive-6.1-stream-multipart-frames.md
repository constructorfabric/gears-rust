# Stream multipart frames

A seeded subscription opens the long-lived `multipart/mixed` stream. The broker emits one event per multipart part, in offset-monotonic order per `(topic, partition)`, interleaved with heartbeat frames on idle.

## Setup

- Run [Cold JOIN against a fresh group](../subscriptions/positive-2.1-cold-join-fresh-group.md). Subscription id `{sub_id}`; assigned `("acme.orders.v1", 0)`.
- Run [Pre-stream SEEK to earliest](../positions/positive-3.1-pre-stream-seek-earliest.md) — cursor seeded at `99`, so emission begins at offset `100`.
- Three events exist on `("acme.orders.v1", 0)` at offsets `100`, `101`, `102`.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id} HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `200 OK`
- `Content-Type: multipart/mixed; boundary=<...>`
- `Transfer-Encoding: chunked`
- Each `MP` part has `Content-Type: application/json` and a top-level `kind` of `event` | `heartbeat` | `advisory` | `topology`.
- The first three `event` parts carry offsets `100`, `101`, `102` in strict order; each part carries exactly one event (no batching).

```
--<boundary>
Content-Type: application/json

{ "kind": "event", "event": { "offset": 100, "partition": 0, "type": "...orders.created.v1", "data": { ... } } }
--<boundary>
Content-Type: application/json

{ "kind": "event", "event": { "offset": 101, "partition": 0, ... } }
--<boundary>
Content-Type: application/json

{ "kind": "event", "event": { "offset": 102, "partition": 0, ... } }
--<boundary>
Content-Type: application/json

{ "kind": "heartbeat" }
```

## Side effects

- `subscription {sub_id} next frame is event with offset 100` (offset-monotonic per partition).
- `subscription {sub_id} emits heartbeat within PT5S` once the backlog is drained (default heartbeat cadence).
- Streaming does not itself advance the cursor — `evbk_group_offsets(group, "acme.orders.v1", 0)` stays at `99` until the consumer acks via [positions](../positions/positive-3.8-mid-stream-forward-ack.md).
