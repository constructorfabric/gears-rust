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
  breaker. The layer works on a tonic `Channel` and on an HTTP client stack alike.

The generated contract clients in `toolkit-contract` put this layer in front of every attempt they
send, per endpoint, when built through wiring (`#[toolkit::consumes]`, `#[toolkit::provides]`, or a
resolving client); a client built from `ClientConfig::new()` has none unless given one. Hand-written
clients can add it with one `.layer(...)`.
