# ToolKit Resilience

Client-side resilience for calls a gear makes to remote dependencies.

Design: [ADR toolkit/0005](../../docs/adrs/toolkit/0005-toolkit-client-circuit-breaker.md).

## Circuit breaker

`circuit_breaker::CircuitBreaker` guards one client-to-backend pair. When enough calls in a sliding
time window fail at the transport level, it opens and rejects calls at once with `Rejected`, which
carries the time until it lets probes through. After `open_for` a few probes go through; if they all
succeed, it closes.

- The caller decides what a failure is: each call takes a `Permit` and records an `Outcome` on it.
  A permit dropped without an outcome counts for nothing.
- `CircuitBreakerConfig` defaults: open when at least half of at least 10 calls in the last 30
  seconds failed, stay open 10 seconds, then one probe. `with_consecutive_failures(k)` trips on `k`
  failures in a row instead.
- Feature `serde`: the config deserializes from an object with optional keys and humantime
  durations (`{ failure_ratio: 0.5, minimum_calls: 10, window: 30s, open_for: 10s,
  half_open_calls: 1 }`), rejecting unknown keys and invalid values.
- Feature `tower`: `CircuitBreakerLayer` runs the breaker as a tower middleware. A policy
  implementing `Classify` and `Bypass` classifies results and picks requests that skip the
  breaker. The breaker decides in `poll_ready`, so an open breaker fails a call without waiting
  for the inner service to become ready. The layer works on a tonic `Channel` and on an HTTP
  client stack alike.

A client adds the breaker with one `.layer(...)`.

## Observers and metrics

`CircuitBreaker::with_observer` attaches a `CircuitBreakerObserver`, which is told when it is
attached and detached (breaker dropped, observer removed, breaker renamed), and about state changes,
recorded calls and rejections. Events arrive after the breaker's lock is released, so an observer
may call back into the breaker; keep it quick and do not panic, it runs on the caller's path.

Events from concurrent calls can arrive out of order. Apply state changes as commutative updates
(one unit moves from `from` to `to`, as the OTel observer does), or drop events older than the
latest `StateChange::generation`. Generations are per breaker instance and start over in a new one,
so keep them per instance (from `on_attach` to `on_detach`), not per name.

Feature `otel`: `metrics::otel::OtelCircuitBreakerMetrics` is an observer that records OpenTelemetry
metrics. `with_observer(OtelCircuitBreakerMetrics::global())` uses the global meter provider (scope
`cf-gears-toolkit-resilience`); `OtelCircuitBreakerMetrics::new(&meter)` takes an explicit meter.

Metric names live in `metrics`, one scheme for every pattern in this crate:
`resilience.<pattern>.<measurement>`, with shared attributes in `metrics::attr`.

| Metric | Kind, unit | Attributes |
|---|---|---|
| `resilience.circuit_breaker.calls` | counter, `{call}` | `resilience.name`; `resilience.outcome`: `success`, `failure`, `ignored`, `rejected` |
| `resilience.circuit_breaker.state` | up-down counter, `{breaker}` | `resilience.name`; `resilience.state`: `closed`, `open`, `half_open` |
| `resilience.circuit_breaker.transitions` | counter, `{transition}` | `resilience.name`; `resilience.previous_state`, `resilience.state` |

- `resilience.name` is the breaker's name. Give it from configuration (one per client and target),
  never from a request, or the series multiply. An unnamed breaker is reported as `unnamed`.
- Each breaker counts 1 in its current state from the moment the observer is attached until the
  breaker is dropped, so summing over `resilience.name` gives the number of breakers per state,
  and a breaker rebuilt under the same name leaves no stale `open`.
- A result that comes after the breaker changed state is counted as `ignored`.
