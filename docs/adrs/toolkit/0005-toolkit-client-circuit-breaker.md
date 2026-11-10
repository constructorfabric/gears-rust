---
description: "A client-side circuit breaker in a small own crate, `toolkit-resilience`, used as a tower layer that sees every attempt below retry, for REST and gRPC clients alike."
status: proposed
date: 2026-11-06
---

# Own client-side circuit breaker in `toolkit-resilience`, counted per attempt below retry

**ID**: `cpt-toolkit-adr-client-circuit-breaker`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The Breaker](#the-breaker)
  - [Tower Layer](#tower-layer)
  - [Observability](#observability)
  - [Placement](#placement)
  - [Breaker and Retry](#breaker-and-retry)
  - [Configuration and Defaults](#configuration-and-defaults)
  - [Industry Comparison](#industry-comparison)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [recloser or failsafe](#recloser-or-failsafe)
  - [Own crate toolkit-resilience](#own-crate-toolkit-resilience)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A client a gear uses to call another gear has a per-call timeout, retry with backoff, and a
concurrency limit. None of these remembers anything across calls. Retry handles a failure that
clears within one call. It does nothing for a provider that is down or unreachable for minutes:
every call still waits its full timeout, often once per attempt, the caller's concurrency slots
fill with calls that cannot succeed, latency grows up the call chain, and the provider gets full
load the moment it comes back.

[ADR-0003](./0003-toolkit-universal-lazy-layer.md) §3.4 sketched a circuit breaker in
`libs/toolkit/src/clients/`. It was never built.

This ADR decides where the breaker lives, what it counts, and where it sits in the client stack.
How the generated contract clients use it (what counts as a failure for a contract call, a per-method
opt-out, per-endpoint state, configuration) is decided with that integration.

## Decision Drivers

* One mechanism for REST clients, gRPC clients and hand-written clients.
* A tower layer, so it composes with the `toolkit-http` stack ([ADR-0001](./0001-toolkit-hyper-tower-http-client.md)) and with a tonic channel.
* Every attempt is checked and counted, including the attempts of a retried call.
* The caller decides what a failure is. A business error or a canonical Problem means the provider answered; counting it would open breakers all the way up a call chain when one dependency deep down is out.
* An open breaker fails at once and tells the caller when to try again.
* Thresholds are deployment config, not code.
* Small dependency footprint: every SDK that builds a client pulls the crate in.

## Considered Options

* Depend on a standalone breaker crate (`recloser`, `failsafe`) and wrap each call.
* Write a small own crate, `toolkit-resilience`: a breaker core and a tower layer.

## Decision Outcome

Chosen option: "Own crate `toolkit-resilience`" (`cf-gears-toolkit-resilience`). The state machine is
small. The parts specific to this platform have to be written here under every option: what counts
as a failure for REST and gRPC, the per-request bypass, the hook that makes a resolving client look
a failing endpoint up again, and the mapping of a rejection to a canonical error with
`retry_after`. A dependency would cover only the easy part, and `recloser`/`failsafe` wrap a call
or a future rather than a tower service.

### The Breaker

`toolkit_resilience::circuit_breaker::CircuitBreaker` guards one client-to-backend pair. A call
takes a `Permit` with `try_acquire` and records an `Outcome` on it: `Success`, `Failure` or
`Ignore`.

* **Closed**: outcomes are counted in a sliding time window of 10 buckets, so memory does not grow with the call rate. By default the breaker opens when the window holds at least `minimum_calls` and the failure share reaches `failure_ratio`. `with_consecutive_failures(k)` replaces that check: the breaker opens after `k` failures in a row and the ratio is not checked, so alternating failures and successes keep it closed whatever their share.
* **Open**: `try_acquire` fails at once with `Rejected`, which carries `retry_after`, the time left until probes are let through, and the breaker's name.
* **Half-open**: after `open_for`, up to `half_open_calls` probes go through. One failure opens the breaker again; when every probe succeeded it closes with an empty window. Each half-open cycle gets a fresh semaphore, so a late probe from an earlier cycle cannot take a slot of the current one. A caller refused because every slot is taken gets a `retry_after` of 1 s, or `open_for` if that is shorter.
* A permit dropped without an outcome (a cancelled call, a local error before the request left) counts for nothing and returns its half-open slot.
* An outcome recorded after the breaker changed state is ignored: a call that started before the breaker opened says nothing about the backend now.
* The closed-state check is lock-free, through an atomic mirror of the state.
* State changes are logged: `warn` on open, `info` on close.

### Tower Layer

Feature `tower`. `CircuitBreakerLayer::new(breaker, policy)` wraps any `Service`. The policy
implements two traits:

* `Classify<Res, Err>` turns a result into an `Outcome`.
* `Bypass<Req>` picks requests that skip the breaker: never rejected, never counted. It is meant for long polls and feeds, whose timeouts say nothing about the backend.

The breaker decides in `poll_ready`, as tower's `load_shed` does. An open breaker reports ready
without polling the inner service, so a caller that waits for readiness (`ready()`, `oneshot`,
`Buffer`) gets `Rejected` at once, even while the inner service is not ready, and no capacity is
reserved below the breaker for a call that will not be made. An admitted call holds its permit
from `poll_ready` to `call`. A bypassed request that arrives after a refusal is sent to a clone of
the inner service, polled ready first, so the inner service must be `Clone`, as tonic channels and
boxed HTTP stacks are.

`ErrorsAreFailures` is the trivial policy. `on_failure(hook)` runs a hook after every call
classified as a failure, for example to drop a cached endpoint. A rejected call
fails with `Rejected` inside the layer's `BoxError`, and the client maps it to its own error type.

### Observability

A `CircuitBreakerObserver` added with `with_observer` gets these events: attach and detach (the
observer added or removed, the breaker renamed or dropped, with the state at that moment), a state
change (from, to, and a generation that grows with every change), a recorded call (outcome, and
whether it was counted or came too late), and a rejection (state, `retry_after`). Events are
delivered after the breaker has released its lock, so an observer may call back into the breaker.
Events from concurrent calls can therefore arrive out of order; an observer either applies them as
commutative updates or uses the generation to drop stale ones. The generation belongs to one breaker
instance and starts over in a new one, so an observer that compares generations keeps them per
instance (from its `on_attach` to its `on_detach`), not per name: a breaker rebuilt under the same
name would otherwise have its changes dropped as stale. Every method has a no-op default, so
a new event is not a breaking change. A consumer that keeps its own metrics, such as the OIDC
plugin's IdP availability gauge, derives them from these events.

The `metrics` module names the metrics of every pattern in the crate under one scheme:
`resilience.<pattern>.<measurement>`, with shared attributes `resilience.name` (the instance, from
configuration, so its cardinality stays bounded), `resilience.outcome`, `resilience.state` and
`resilience.previous_state`. A later pattern (retry, bulkhead) adds its own module and reuses these
attributes. State is an up-down counter in which each breaker counts 1 in its current state, not a
gauge holding a state code: the updates commute, so out-of-order events, delta temporality and a
breaker dropped and rebuilt under the same name all leave it correct. For the breaker:

| Metric | Kind, unit | Attributes |
|---|---|---|
| `resilience.circuit_breaker.calls` | counter, `{call}` | name; outcome `success`, `failure`, `ignored`, `rejected` |
| `resilience.circuit_breaker.state` | up-down counter, `{breaker}` | name; state `closed`, `open`, `half_open` |
| `resilience.circuit_breaker.transitions` | counter, `{transition}` | name; previous state, state |

Feature `otel` adds `OtelCircuitBreakerMetrics`, an observer that records these through the global
meter provider (scope `cf-gears-toolkit-resilience`) or an explicit meter. It
is off by default, like the `otel` features of the other client crates: a client that exports no
metrics takes no OpenTelemetry dependency. The exporter (OTLP) is the application's.

### Placement

The breaker sees each attempt, below retry:

* **REST.** An attempt-layer slot in `toolkit-http`, at `Retry → Auth → Attempt → ErrorMapping → Timeout`. It runs after credentials are attached, so a failure to get a token is not taken for a backend failure. It runs above the timeout, so a timed-out attempt reaches it as a timeout.
* **gRPC.** The layer wraps the tonic channel, under tonic's own call handling.

A caller's own timer around an attempt must not fire before the transport timeout, or the breaker
sees a dropped permit instead of a timeout. Such a timer needs a small grace over the transport
timeout.

### Breaker and Retry

Retry and the breaker answer different questions. Retry asks whether a failure clears within one
call. The breaker asks whether the backend has been failing across calls. Retry on a 503 with
backoff alone still sends every new call to a dead backend and waits out its timeouts; with many
callers, the backoff also spreads the load out instead of taking it off.

An open breaker's rejection is not transient: a retry loop stops at once instead of being refused
again for each attempt. Counting is per attempt, so a call that makes three attempts against a dead
backend records three failures, and the breaker opens sooner.

### Configuration and Defaults

`CircuitBreakerConfig`:

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | `false` makes a breaker that never rejects or counts |
| `failure_ratio` | 0.5 | failure share in the window that opens the breaker, in `(0, 1]` |
| `minimum_calls` | 10 | calls the window must hold before the ratio is checked |
| `window` | 30 s | length of the sliding window |
| `open_for` | 10 s | time open before probes are let through |
| `half_open_calls` | 1 | probes per half-open cycle |
| `consecutive_failures` | none | if set, open after this many failures in a row instead of the ratio |

With feature `serde`, the config deserializes from an object whose keys are all optional, with
humantime durations. Unknown keys are rejected, and `validate` rejects a ratio out of range and zero
counts or durations, so a bad config is a boot error.

The defaults are smaller than Resilience4j's (10 calls, not 100; 10 s open, not 60 s). Gear-to-gear
traffic per endpoint is often low, and a client that finds a failing endpoint looks it up in the
directory again, so a short open period with one probe recovers sooner at little cost.

### Industry Comparison

| | Where it is declared | Trip condition (defaults) | Open time | Half-open | Rejection |
|---|---|---|---|---|---|
| Resilience4j | `@CircuitBreaker(name = "…")` names an instance; thresholds in config | `failureRateThreshold` 50 % once `minimumNumberOfCalls` (100) are recorded; also a slow-call threshold | `waitDurationInOpenState` 60 s | `permittedNumberOfCallsInHalfOpenState` 10 | `CallNotPermittedException` |
| MicroProfile Fault Tolerance | `@CircuitBreaker` on the method carries the thresholds; MP Config can override them | `failureRatio` 0.5 over a rolling window of `requestVolumeThreshold` (20) calls | `delay` 5 s | `successThreshold` 1 | `CircuitBreakerOpenException` |
| Polly v8 | strategy added to a resilience pipeline in code | `FailureRatio` (0.1) over `SamplingDuration` (30 s), once `MinimumThroughput` (100) calls are seen; `ShouldHandle` classifies | `BreakDuration` 5 s | trial call after the break | `BrokenCircuitException` |
| gobreaker (Go) | `Settings` per breaker in code | `ReadyToTrip`, default more than 5 consecutive failures; `IsSuccessful` classifies | `Timeout` 60 s | `MaxRequests` (1) | `ErrOpenState`, `ErrTooManyRequests` |
| Envoy | per cluster in proxy config; per upstream host | outlier detection: `consecutive_5xx` 5 | `base_ejection_time` 30 s, growing with each ejection | host returns to the load-balancing pool | host is ejected from the pool, no request error; Envoy's `circuit_breakers` are concurrency limits |
| gRPC | not built in | — | — | — | `UNAVAILABLE` is the status for "try again"; service-config `retryThrottling` and xDS outlier detection are the nearest mechanisms |
| `toolkit-resilience` | thresholds in config; the caller's policy classifies and bypasses | `failure_ratio` 0.5 once `minimum_calls` (10) are seen in a 30 s window, or `consecutive_failures` | `open_for` 10 s | `half_open_calls` 1 | `Rejected { retry_after }` |

### Consequences

* Good, because REST, gRPC and hand-written clients share one breaker and one layer; a hand-written client adds it with one `.layer(...)`.
* Good, because classification stays with the caller, so business errors and Problems are not counted.
* Good, because the crate depends only on tokio, parking_lot, thiserror and tracing, plus optional ones behind features: tower and pin-project-lite (`tower`), serde and toolkit-utils (`serde`), opentelemetry (`otel`).
* Good, because every pattern's metrics follow one naming scheme, and consumers with their own metrics plug in through observers.
* Bad, because the platform maintains the code: a few hundred lines plus tests.
* Breaker state is per process and per breaker; nothing is shared across replicas.
* The breaker name is the metric attribute, so names must come from configuration. A name per resolved endpoint address (for example `gear@ip:port`) grows the series with every redeploy.
* A layer sees what reaches it as a response head or an error. A gRPC status sent in trailers after the response headers, or a stream that fails after its head, is not classified.

### Confirmation

* `cargo test -p cf-gears-toolkit-resilience --all-features`:
  * `src/circuit_breaker/tests.rs`: minimum calls, ratio, consecutive failures, window expiry, probe slots, late results, dropped permits, config parsing and validation; observer events and their order, generations, attach/detach on rename, removal and drop, an observer calling back into the breaker.
  * `tests/layer.rs`: the tower layer, the failure hook, bypass, a rejection reaching observers, an open breaker failing a call while the inner service is not ready, a bypassed request after a refusal waiting for the inner service.
  * `src/metrics/otel.rs`: the OpenTelemetry instruments with an in-memory exporter, including state counts under concurrent transitions and a breaker rebuilt under the same name.

## Pros and Cons of the Options

### recloser or failsafe

* Good, because both are small and established.
* Bad, because they wrap a call or a future, not a tower service. Used at the generated method level, the breaker would sit outside the retry loop and count calls rather than attempts; otherwise a tower adaptor has to be written here.

### Own crate toolkit-resilience

* Good, because the semantics (outcomes, ignored late results, rejection with `retry_after`) are owned and tested here.
* Good, because hand-written clients can use the same layer.
* Bad, because the platform maintains the code.

## More Information

* Follow-ups:
  * metric names for the next patterns (retry, bulkhead), added with their implementations;
  * steering calls away from an endpoint whose breaker is open, instead of failing them fast;
  * the `toolkit-http` attempt slot and the generated contract clients (#5777).
* Code: `libs/toolkit-resilience` (`README.md`, `src/circuit_breaker/`, `src/metrics/`).

## Traceability

- **HTTP client**: [ADR-0001](./0001-toolkit-hyper-tower-http-client.md)
- **Lazy clients**: [ADR-0003](./0003-toolkit-universal-lazy-layer.md) §3.4, which this ADR replaces

This decision directly addresses:

* #4771 — a circuit breaker for outgoing calls.
* #5561 — the circuit-breaker part of the client resilience design.
