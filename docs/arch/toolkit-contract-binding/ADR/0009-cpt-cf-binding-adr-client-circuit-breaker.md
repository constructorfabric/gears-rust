---
status: proposed
date: 2026-11-06
---

# Client-Side Circuit Breaker in Generated Contract Clients

**ID**: `cpt-cf-binding-adr-client-circuit-breaker`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

DESIGN §1.3 marks circuit breaking as "Recommended" for remote-capable contracts (Api / Backend), and
DESIGN §7 *Remote Backend Unavailability* deferred the mechanism to a separate ADR.

Before this change a generated client had a per-call timeout, `#[retryable]` retry with backoff, and a
concurrency limit (`max_concurrent_requests`), but nothing that remembered failures across calls.
A provider that is down for minutes still cost every call its full timeout, often several times
over.

The breaker itself, its crate, state machine, tower layer, config and defaults, is a toolkit
decision: [toolkit ADR-0005](../../../adrs/toolkit/0005-toolkit-client-circuit-breaker.md)
(`cpt-toolkit-adr-client-circuit-breaker`). This ADR records how the generated contract clients use
it: where the contract declares anything, what counts as a failure for a contract call, how state is
kept per endpoint, and how it is configured per dependency.

Note on numbering: `docs/arch/toolkit-oop/ADR/` also has an ADR-0009
(`cpt-cf-adr-instance-addressable-discovery`). Cross references to either use the `cpt-` id.

## Decision Drivers

* **Thresholds are deployment config, not contract.** The contract author knows which methods must
  never be counted (long polls); the operator knows the traffic and the provider's recovery time.
* **Classify by transport outcome, not by domain error.** A business error or a canonical Problem
  means the provider answered. Counting those would open breakers all the way up the call chain when
  one dependency deep down is out.
* **State per endpoint.** One bad instance among several must not cut off the healthy ones, and the
  directory's round-robin must not reset the state.
* **Same behaviour for REST and gRPC** projections of a contract.
* **Fail fast with a canonical error** that tells the caller when to try again.
* **No code in the consumer.** A gear that consumes a contract gets the breaker without writing
  anything.

## Considered Options

* **Option A**: Thresholds on the contract: `#[circuit_breaker(failure_ratio = …, open_for = …)]` on
  the trait or method, config may override (MicroProfile Fault Tolerance style).
* **Option B**: The contract only marks methods to skip, `#[circuit_breaker(skip)]`; every number is
  in config per dependency, in `clients.consumes` / `clients.provides` (Resilience4j style).
* **Option C**: No breaker in generated clients; a gear that wants one wraps its client by hand.

## Decision Outcome

Chosen option: **Option B — `skip` in the contract, numbers in config, on by default.**

The contract author cannot know the traffic, the number of provider instances or the provider's
recovery time of each deployment; the same contract is consumed by gears with very different load.
What the author does know is which methods are expected to time out. Option C leaves every consumer
without protection until someone notices a cascade.

### Where the breaker sits in generated clients

Per attempt, inside the retry loop, so every attempt of a `#[retryable]` call is checked and counted
(toolkit ADR-0005, *Placement*):

* **REST**: `HttpClientBuilder::with_attempt_layer`. Generated clients switch the `toolkit-http`
  retry off and retry in the contract runtime, so the slot sees one attempt at a time.
* **gRPC**: the tonic channel is wrapped (`toolkit_contract::grpc::GrpcChannel`, built by
  `guard_channel`), for `new`, `connect` and `connect_lazy` alike.

The REST unary path had a race: the SDK's own per-attempt timer always fired before the transport
timeout, so a timed-out attempt reached the breaker as a dropped call and counted for nothing.
`send_unary` now lets the transport timeout end the header phase; the outer timer keeps a 50 ms
grace and still bounds the whole attempt, body included.

### Classification (`toolkit_contract::runtime::breaker`, `grpc::GrpcEndpointFailure`)

| Outcome | REST | gRPC |
|---------|------|------|
| **Failure** | connect, TLS and transport errors; transport timeout; 502/503/504 without `application/problem+json` | any channel error (connect, reset, per-call timeout); trailers-only `UNAVAILABLE` or `DEADLINE_EXCEEDED` without `x-toolkit-problem-bin` |
| **Success** (the provider answered) | every other response, including a canonical Problem with status 503, 429 and 500 | every other response, including a canonical Problem and `RESOURCE_EXHAUSTED` |
| **Not seen or not counted** | `Overloaded` (shed by the client's own limiter before the attempt), credential errors, `Unresolved` (no endpoint, no client yet), cancelled calls | `Unresolved`, cancelled calls |

A request marked `#[circuit_breaker(skip)]` bypasses the breaker on both transports.

### Rejection

An open breaker fails the attempt with `TransportError::CircuitOpen { retry_after }` without touching
the network. It is **not transient**: an in-call retry loop or a stream reconnect stops at once
instead of being refused again. It maps to canonical `ServiceUnavailable` with `retry_after` (whole
seconds, at least 1), which the REST server renders as a 503 Problem with `Retry-After`. A caller one
hop further up sees a Problem and counts it as an answer, so the breakers do not cascade.

### State per endpoint

* A `DirectoryResolvingClient` (`<Contract>RestResolvingClient`, `<Contract>GrpcResolvingClient`)
  keeps one client per endpoint URI, up to 16, each with its own breaker named `<gear>@<uri>`. The
  directory's round-robin reuses them instead of rebuilding clients and resetting breakers.
* An attempt classified as a failure calls `EndpointResolver::invalidate(target)`, at most once per
  second per endpoint, so the next call asks the directory again instead of waiting out the
  resolver's cache TTL (1.5 s). When the directory has no live endpoint, all cached clients and their
  breakers are dropped.
* A client wired by `#[toolkit::provides]` (`clients.provides`, one static endpoint) has one breaker,
  named `<gear>:<Contract>`.

### Contract surface: `#[circuit_breaker(skip)]`

The only breaker attribute on a contract. It goes on a REST or gRPC projection method whose
failures say nothing about the provider, such as a long poll that is expected to time out. The
generated client marks the request (`SkipCircuitBreaker` in the request extensions) and the layer
lets it through uncounted. Any other argument is a compile error that points to the config key.

### Configuration

`circuit_breaker` is a `ClientTuning` field, so it exists wherever tuning does. Its keys and
defaults are those of `CircuitBreakerConfig` (toolkit ADR-0005, *Configuration and Defaults*):

```yaml
gears:
  orders:
    clients:
      defaults:
        circuit_breaker: { open_for: "30s" }   # every client of this gear
      consumes:
        billing:
          timeout: "5s"
          circuit_breaker:
            consecutive_failures: 5   # trip on k failures in a row; open_for still 30s
      provides:
        payment_api:
          transport: rest
          endpoint: "http://payments:8080"
          circuit_breaker: { enabled: false }
```

`clients.defaults` is merged under each entry key by key (ADR-0004). A missing key keeps the
defaults; `{ enabled: false }` turns the breaker off for that client. An
invalid value fails config parsing, which is a boot error.

**On by default.** Every client built through wiring gets a breaker: `#[toolkit::consumes]` and
`#[toolkit::provides]`, REST and gRPC, and any `<Contract>…ResolvingClient` built by hand.
`ClientConfig::new()` has none; `ClientConfig::with_circuit_breaker` adds one.

The split between contract and config follows Resilience4j; per-endpoint state follows Envoy
outlier detection. The comparison with other libraries is in toolkit ADR-0005.

### Consequences

* **Behaviour change for existing REST consumers and providers.** Every client built through
  `#[toolkit::consumes]` or `#[toolkit::provides]` now has a breaker with no config change. During a
  mass provider failure, calls fail fast with `ServiceUnavailable` (503 with `Retry-After` when
  propagated) instead of waiting out their timeouts and retries, and a caller that matched on a
  timeout sees `ServiceUnavailable` while the breaker is open. `{ enabled: false }` restores the old
  behaviour per dependency.
* Long polls and other calls that are expected to time out must be marked `#[circuit_breaker(skip)]`;
  otherwise their timeouts open the breaker.
* Counting is per attempt: a `#[retryable]` call that makes three attempts against a dead endpoint
  records three failures.
* Hand-written clients do not get a breaker. cluster-sdk's gRPC client (one channel for four
  services) can add `CircuitBreakerLayer` to its channel later.
* A gRPC status sent in trailers after the response headers (a server-streaming error, or a server
  that sends headers first) is not classified. tonic servers send unary errors trailers-only, which
  the classifier sees. Likewise a REST or gRPC stream that fails after its response head is not
  counted.

### Confirmation

* REST classification and placement: unit tests in `libs/toolkit-contract/src/runtime/breaker.rs`
  and `libs/toolkit-contract/tests/circuit_breaker_rest.rs` (gateway errors open the breaker and it
  then refuses without sending; a Problem 503 is not counted; a refused connection counts and runs
  the failure hook; a timed-out attempt reaches the breaker; a marked request bypasses it).
* gRPC classification: unit tests in `libs/toolkit-contract/src/grpc/mod.rs`, and
  `examples/toolkit/api-contracts/api-contracts/tests/grpc_breaker.rs` (a bare `UNAVAILABLE` opens
  the breaker, one carrying a Problem does not, a `skip` method is neither counted nor refused).
* gRPC recovery: `a_dead_provider_opens_the_breaker_and_it_closes_after_recovery` in
  `examples/toolkit/api-contracts/api-contracts/tests/grpc_resolving.rs`.
* Macro: `#[circuit_breaker(skip)]` marks only the annotated method, and any other argument is
  rejected (`grpc_contract.rs` unit tests; REST shares `support::parse_circuit_breaker_skip`).

## Pros and Cons of the Options

### Option A: Thresholds on the contract

* Good, because the protection is visible in the contract source.
* Bad, because the author picks numbers for traffic and recovery times they do not see; each
  deployment would override them anyway.
* Bad, because a threshold change becomes an SDK release.

### Option B: `skip` in the contract, numbers in config (chosen)

* Good, because the contract carries only what its author knows, and the operator tunes the rest
  per dependency without a rebuild.
* Good, because every wired client is protected with no code and no config.
* Bad, because the default is a behaviour change for existing consumers (see Consequences).

### Option C: No breaker in generated clients

* Good, because nothing changes for existing consumers.
* Bad, because each gear has to wire the layer itself, and most will not until after an incident.
* Bad, because a hand-written wrapper cannot see attempts inside the generated retry loop.

## More Information

* Breaker mechanism: [toolkit ADR-0005](../../../adrs/toolkit/0005-toolkit-client-circuit-breaker.md)
  (`cpt-toolkit-adr-client-circuit-breaker`).
* DESIGN §1.3 *Operational Semantics* and §7 *Remote Backend Unavailability*:
  [`../DESIGN.md`](../DESIGN.md).
* `cpt-cf-binding-adr-consumer-wiring` —
  [`0004-cpt-cf-binding-adr-consumer-wiring.md`](./0004-cpt-cf-binding-adr-consumer-wiring.md):
  the directory-resolving clients and the `clients` config section.
* `cpt-cf-binding-adr-grpc-projection` —
  [`0008-cpt-cf-binding-adr-grpc-projection.md`](./0008-cpt-cf-binding-adr-grpc-projection.md):
  the generated gRPC client and its channel.
* Integration: `libs/toolkit-contract/src/runtime/breaker.rs`, `src/grpc/mod.rs`
  (`GrpcChannel`, `guard_channel`, `GrpcEndpointFailure`), `src/runtime/client.rs`
  (`send_unary`, `map_send_error`), `src/runtime/resolving.rs`, `src/wiring.rs`
  (`ClientTuning::circuit_breaker`).
