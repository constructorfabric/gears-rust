# Circuit breaker: half-open probes close the circuit on recovery

After `timeout_seconds` an open circuit lets `success_threshold` requests through as probes. When every probe succeeds the circuit closes; one failed probe opens it again for another `timeout_seconds`.

## Setup

An upstream with:

```json
{
  "circuit_breaker": {
    "failure_threshold": 2,
    "timeout_seconds": 1,
    "success_threshold": 1
  }
}
```

and a route `GET /v1`. Open the circuit as in [18.9](negative-18.9-circuit-opens-after-consecutive-failures.md): two `503` answers in a row.

## Scenario A: the probe succeeds

1. Wait `timeout_seconds` (1 s).
2. Send a request; the upstream has recovered and answers `200`.
3. The upstream fails once more (`503`), then send another request.

Expected:
- Step 2 reaches the upstream and returns its `200`: the probe closed the circuit.
- Step 3: the single failure does not open the circuit (the count starts again from 0); the next request reaches the upstream.

## Scenario B: the probe fails

1. Wait `timeout_seconds`.
2. Send a request; the upstream still answers `503`.
3. Send another request.

Expected:
- Step 2 reaches the upstream and returns its `503`.
- Step 3 gets the gateway's `503` with `Retry-After`, as in 18.9: the circuit is open for another `timeout_seconds`.

## Notes

- While half-open, at most `success_threshold` probes are in flight. A request beyond them gets the gateway's `503` with `Retry-After: 1`.
- A probe slot is taken just before the upstream's address is resolved, not while the auth plugin or the CredStore read runs. The fail-fast check before them takes none.
- A probe that ends without an outcome (the client cancels it, or the gateway refuses it before the upstream is called) frees its slot for the next request.
- A WebSocket upgrade answered `101` is a successful probe.
