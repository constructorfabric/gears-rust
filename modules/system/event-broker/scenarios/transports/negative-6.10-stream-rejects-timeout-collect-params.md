# Stream rejects legacy timeout / collect params

The merged `:stream` endpoint takes only `subscription_id`. The pre-merge long-poll parameters `timeout` and `collect` no longer exist; supplying them is rejected `400` (or ignored — implementation choice; the OpenAPI spec defines neither). This scenario asserts the reject behavior.

## Setup

- Run [Cold JOIN](../subscriptions/positive-2.1-cold-join-fresh-group.md) + [SEEK](../positions/positive-3.1-pre-stream-seek-earliest.md) → seeded `{sub_id}`.

## Request

```http
GET /v1/events:stream?subscription_id={sub_id}&timeout=20&collect=50 HTTP/1.1
Host: broker.example.com
Authorization: Bearer <tenant-token>
Accept: multipart/mixed
```

## Expected response

- `400 Bad Request` (`PD`) — unknown query parameters `timeout`, `collect`.

```json
{
  "type": "https://errors.cf.core/events/bad-request",
  "title": "BadRequest",
  "status": 400,
  "detail": "unknown query parameters: timeout, collect — the stream endpoint accepts only subscription_id",
  "instance": "/v1/events:stream"
}
```

## Side effects

- No stream established. (These params were removed by the `merge-poll-and-stream-endpoints` change; a streaming transport has no per-request timeout/collect window.)
