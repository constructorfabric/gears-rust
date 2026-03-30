---
status: proposed
date: 2026-03-23
owner: SDK architecture team
scope: modules/serverless-sdk
priority: p2
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 RULES:
  - ADRs represent actual decision dilemma and decision state
  - DESIGN is the primary artifact ("what"); ADRs annotate DESIGN with rationale ("why")
  - Use single ADR per decision
 ==============================================================================
 -->

# ADR — Synchronous Environment Interface (Adapter Pre-Fetch Model)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Synchronous Environment](#option-a-synchronous-environment)
  - [Option B: Async Environment](#option-b-async-environment)
  - [Option C: Sync with Optional Async Reload](#option-c-sync-with-optional-async-reload)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-sync-environment`
## Context and Problem Statement

`Handler::call` receives an `&dyn Environment` that provides access to configuration
values and secrets. The platform's secret resolution mechanism is inherently async —
obtaining a secret requires an I/O call to a remote credential store. The `Environment`
trait must decide whether `get_secret` (and `get_config`) are synchronous or asynchronous.

If sync: the adapter must pre-fetch all required values before calling the handler;
`Environment` is a read-only snapshot at call time.
If async: handlers fetch on demand, but `get_secret` must be declared `async` — which
for `dyn Environment` requires `#[async_trait]` or equivalent machinery, and the
`Environment` implementation must hold a live client reference across the entire
handler call.

Which interface should the `Environment` trait expose?

## Decision Drivers

* **[P1]** The `Environment` trait must satisfy `cpt-cf-serverless-sdk-core-principle-impl-agnostic`:
  it cannot expose credstore SDK types or any engine-specific interface — hard architectural
  constraint; violations break the SDK/adapter boundary.
* **[P1]** The `Environment` trait is a testability boundary — unit tests must supply a
  simple `HashMap`-backed mock without any platform infrastructure — non-negotiable for
  developer experience and CI correctness.
* **[P2]** Handler implementations must remain free of async fetching boilerplate inside
  `call` (`cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics`) — ergonomics requirement;
  reduces implementation errors and cognitive burden on function authors.
* **[P3]** The platform design assumes that secret requirements are declared in the function
  definition's deployment configuration, making the full set of required secrets known
  before the handler is called; eager loading is therefore feasible — validates Option A's
  pre-fetch model. (Platform design assumption; see Serverless Runtime DESIGN.md §3.1.)
* **[P3]** Pre-fetching a small, bounded set of secrets before the handler call is cheap
  relative to total invocation cost — supporting evidence for Option A performance
  acceptability.

## Considered Options

* **Option A**: Synchronous `Environment` — adapter resolves all required values before
  calling the handler and provides them via a `HashMap`-backed implementation.
* **Option B**: Async `Environment` — `get_secret` is declared `async`; the implementing
  struct holds a live client reference for on-demand resolution. Two sub-variants:
  (B1) using `async-trait` — preserves `dyn Environment` object safety;
  (B2) RPITIT `impl Future` return type — loses `dyn Environment` object safety,
  the same constraint identified in `cpt-cf-serverless-sdk-core-adr-async-trait` Option B.
* **Option C**: Sync `Environment` with async `reload` — `get_secret` and `get_config`
  are sync; a separate `async fn reload(&mut self, keys: &[&str])` method on an optional
  supertrait allows mid-invocation secret refresh for long-running workflows. Requires
  `&mut dyn Environment` or a separate `Refreshable` supertrait with downcasting —
  changes the handler signature from Option A's `&dyn Environment` baseline.

## Decision Outcome

Chosen option: **Option A: Synchronous `Environment`**, because it satisfies all three
P1 and P2 decision drivers simultaneously: `get_secret` and `get_config` are simple
synchronous calls with no `await` or error propagation in the handler body
(`cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics`); the trait exposes no async
infrastructure, satisfying `cpt-cf-serverless-sdk-core-principle-impl-agnostic`; and a
`HashMap`-backed test double requires no async executor, keeping unit tests simple
(P1 testability driver). The pre-fetch model is feasible given the platform design
assumption that required secrets are declared in deployment configuration (P3).

Option B was rejected because async methods in the `Environment` trait would require the
implementing struct to hold a live client reference, violating the impl-agnostic principle
and destroying the `HashMap`-backed testability boundary.

Option C was rejected because `reload(&mut self)` would require changing the handler
signature to `&mut dyn Environment` or introducing a `Refreshable` supertrait with
downcasting — a structural cost with no current requirement to justify it.

### Consequences

* Adapters are responsible for resolving all configuration values and secrets declared by
  the function definition before calling `Handler::call`. The expected caching granularity
  is **cold-start**: secrets are pre-fetched once at function cold-start and reused for
  the lifetime of that invocation; adapters MUST NOT re-fetch secrets on every invocation
  unless the platform's credential caching layer already guarantees in-process caching.
* If a secret is unavailable at pre-fetch time, the adapter must fail the invocation before
  the handler is called, with an appropriate retryable or non-retryable error indicator.
* Long-running workflows that need secrets to remain valid across a multi-day suspension
  must be handled by the adapter (e.g., refreshing secrets on resume). The `Environment`
  contract does not address this; it is an adapter concern.
* If a concrete need for mid-invocation async secret refresh arises in future, a separate
  async `SecretProvider` trait can be introduced in the adapter crate without modifying
  `Environment`. This keeps the breaking-change surface small.

### Confirmation

* `Environment` trait definition has no `async fn`, no `impl Future`, and no `async-trait`
  attribute — verified by code inspection.
* A `HashMap<String, String>`-backed `Environment` implementation compiles and satisfies
  the trait in unit tests without any platform infrastructure.

## Pros and Cons of the Options

### Option A: Synchronous Environment

The adapter resolves config and secrets eagerly and provides a snapshot `Environment`.

* Good, because handler code is the simplest possible: `env.get_secret("API_KEY")` with
  no `.await`, no error propagation, no async context required.
* Good, because the trait is trivially testable — any `HashMap` satisfies it.
* Good, because the trait definition has no async machinery, no async infrastructure types,
  no lifetime complexity from a held async client.
* Good, because the set of required secrets is known from the function definition, making
  eager loading deterministic.
* Neutral, because adapters incur a round-trip to the credential store at cold-start before
  the first invocation. Acceptable: the cost is bounded and amortised across all invocations
  for the lifetime of the function instance.
* Bad, because secrets fetched at invocation start may expire mid-execution for very
  long-running workflows. This edge case requires adapter-level handling.

### Option B: Async Environment

`get_secret` returns a `Future`; the implementing struct holds a live credential store client.

* Good, because secrets can be fetched lazily — unused secrets are never resolved.
* Good, because secrets can be refreshed mid-invocation if they expire.
* Bad, because `get_secret` must be declared `async`, adding `async-trait` complexity or
  RPITIT signature noise to a trait that is primarily used for simple key lookups.
* Bad, because `env.get_secret("KEY").await?` in handler code requires handlers to import
  `async` plumbing — violates the goal of keeping handlers free of platform boilerplate.
* Bad, because the implementing struct must hold a live client reference, coupling a platform
  infrastructure type into the implementation.
* Bad, because unit testing requires an async mock credential store client, not just a `HashMap`.

### Option C: Sync with Optional Async Reload

`get_secret` is sync; a separate `async fn reload` method refreshes specific keys.

* Good, because normal handler use is sync; the async path is opt-in.
* Good, because it handles the mid-invocation expiry case without forcing async everywhere.
* Bad, because it complicates the trait definition with an orthogonal async extension method.
* Bad, because `reload` requires the implementing struct to hold a mutable async client
  internally, reintroducing credential store coupling in the implementation surface.
* Bad, because the need for mid-invocation secret refresh has no concrete requirement yet —
  adding this complexity is premature.

## More Information

The pre-fetch model is consistent with how AWS Lambda, Google Cloud Functions, and other
FaaS platforms handle secrets — credentials are loaded at cold start or at invocation setup
time and cached for the duration of the invocation. This is a well-established pattern with
understood trade-offs.

## Non-Applicable Domains

The following checklist domains are not applicable to this ADR and are explicitly excluded:

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| SEC | N/A | Decision concerns trait synchrony model; no credential storage in the SDK — credentials are opaque strings passed as `Option<&str>`; security of the underlying credential store is an adapter/platform concern |
| REL | N/A | No stateful data, SLO, or availability commitment at the SDK trait level |
| DATA | N/A | No data schema, persistence, or lifecycle managed by the `Environment` trait |
| OPS | N/A | Pure library; no deployment topology, monitoring infra, or operational concern |
| COMPL | N/A | Internal developer tooling; no regulatory, certification, or legal requirement |
| UX | N/A | No end-user UI; developer ergonomics are the primary driver and are addressed in Decision Outcome |
| BIZ | N/A | Internal Rust library crate; no business stakeholder buy-in, cost analysis, or time-to-market consideration applicable to a trait synchrony decision |

## Review Conditions

This ADR should be revisited when any of the following conditions is met:

| Trigger | Action |
|---------|--------|
| A concrete requirement emerges for mid-invocation secret refresh (e.g., a workflow suspended for days resumes and its secrets have expired) | Re-evaluate Option C (sync + async `reload` extension); introduce a separate `AsyncSecretProvider` in the adapter crate without modifying `Environment` |
| The platform's credential resolution gains a synchronous in-process call path (no network round-trip) | Re-evaluate whether lazy async fetch becomes as cheap as eager sync pre-fetch, reducing the advantage of Option A |
| The function definition model changes such that required secrets are no longer known statically at registration time | Re-evaluate the pre-fetch feasibility assumption; eager loading requires a known secret set |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-sdk-core-fr-environment-trait` — `Environment` trait signature
* `cpt-cf-serverless-sdk-core-principle-impl-agnostic` — no platform infrastructure types in the trait
* `cpt-cf-serverless-sdk-core-component-environment` — responsibility boundary definition
