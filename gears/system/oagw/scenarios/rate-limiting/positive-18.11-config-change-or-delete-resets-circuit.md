# Circuit breaker: a config change or upstream delete resets the breaker

An upstream's breaker belongs to its configuration, alias and endpoints. When any of them changes, the next request on every replica starts with a closed circuit; a write that changes or removes the breaker, and a delete, discard it on the replica that handles the write.

## Setup

An upstream with `"circuit_breaker": {"failure_threshold": 2, "timeout_seconds": 60, "success_threshold": 1}` and a route `GET /v1`. Open its circuit as in [18.9](negative-18.9-circuit-opens-after-consecutive-failures.md).

## Scenario A: unchanged write

`PUT /api/oagw/v1/upstreams/{id}` with the same `circuit_breaker` and endpoints (other fields such as `tags` may change).

Expected: the circuit stays open; the next request gets the gateway's `503`.

## Scenario B: changed, or removed and re-added, breaker or endpoints

`PUT /api/oagw/v1/upstreams/{id}` with another `circuit_breaker` (for example `failure_threshold: 3`), with the breaker removed, or with other endpoints. Or remove the breaker with one `PUT` and add the same `circuit_breaker` back with the next.

Expected:
- `200 OK`; `GET /api/oagw/v1/upstreams/{id}` returns the new `circuit_breaker` as sent.
- The next request reaches the upstream; after the re-add it starts with a closed circuit.

## Scenario C: delete and re-create

`DELETE /api/oagw/v1/upstreams/{id}`, then create an upstream with the same alias and `circuit_breaker`.

Expected: requests to the new upstream reach it; nothing of the old breaker's state is kept.

## Notes

- Every replica compares the breaker's config, alias and endpoints with the upstream's on each request, so a change made on another replica resets the breaker there too, endpoints alone included. A request that resolves no config leaves the breaker alone (under a `private` config a descendant sees none while the owner sees it). A write that changes or removes the breaker, and a delete, drop it on the replica that handles the write; elsewhere, a breaker no request uses is evicted 10 minutes after its `timeout_seconds`. So on another replica, an identical config re-added before that eviction continues the old state, which is open for at most `timeout_seconds`.
- With `"enabled": false` the breaker is removed; enabling it again starts closed.
- Sharing follows the other upstream fields: a descendant without its own `circuit_breaker` uses an ancestor's `inherit` or `enforce` one, with a breaker of its own upstream; a `private` one is not seen by descendants; a descendant cannot set its own under an ancestor's `enforce` (`400`, `cannot override circuit_breaker: ancestor upstream has sharing mode 'enforce'`).
