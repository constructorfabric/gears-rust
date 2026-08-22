---
status: accepted
date: 2026-05-18
---

# ADR-0002: Synchronous Environment with Pre-Fetched Config and Secrets

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Operating Envelope](#operating-envelope)
  - [Confirmation](#confirmation)
  - [Applicability](#applicability)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Synchronous `Environment`, pre-fetched](#synchronous-environment-pre-fetched)
  - [Asynchronous `Environment`, lazy fetch](#asynchronous-environment-lazy-fetch)
  - [Hybrid: sync hits with async fallback](#hybrid-sync-hits-with-async-fallback)
- [More Information](#more-information)
  - [Related Decisions](#related-decisions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-adr-sync-environment`

## Context and Problem Statement

Handlers need access to per-invocation configuration and secrets resolved from the platform credstore. The credstore is itself an async, network-bound service. The SDK must decide whether handlers see this access as synchronous (with values pre-fetched before the call) or asynchronous (with each lookup awaiting credstore I/O).

## Decision Drivers

**Primary drivers**

- Handler authors should not have to `.await` on every config or secret lookup (`cpt-cf-serverless-runtime-plugin-sdk-nfr-authoring-ergonomics`).
- Credstore I/O on the hot path would convert handler invocation into a sequence of network round-trips, breaking `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`.

**Supporting facts**

- The set of config/secret keys a handler needs is statically known from the function definition; resolving lazily buys nothing.
- The platform credstore SDK (`cf-credstore-sdk`) is the only sanctioned source; the SDK must integrate with it without leaking its types into the handler-author surface (`cpt-cf-serverless-runtime-plugin-sdk-principle-impl-agnostic`).

## Considered Options

- Synchronous `Environment` trait, pre-fetched per invocation.
- Asynchronous `Environment` trait, lazy fetch on each call.
- Hybrid: sync hits with async fallback on cache miss.

## Decision Outcome

Chosen: **synchronous `Environment` trait, pre-fetched per invocation**, because the set of required keys is known up-front and synchronous access removes credstore I/O from the handler hot path entirely.

### Consequences

- The adapter is responsible for populating an `Environment` instance from the credstore before invoking `FunctionHandler::call`; the SDK ships `CredStoreEnvironment` as the standard implementation.
- `get_config` and `get_secret` return `Option<&str>` borrowed from `&self`; the implementation must own the resolved string data. Implications for binary secrets and dynamic keys are spelled out in [Operating Envelope](#operating-envelope).
- Values rotated mid-execution are not visible to the handler; handlers must not assume freshness within a single invocation.
- Handler-author code is engine-agnostic and does not import any credstore types.

### Operating Envelope

This decision is sound only inside the following envelope. Workloads outside it require a separate API or a new ADR.

- **Statically declared keys.** A handler's config and secret keys are declared up-front in its function definition. Keys computed at runtime (e.g., `format!("tenant-{tenant_id}/api-key")`) are out of scope; handlers needing dynamic key resolution must use a separate API and are not covered here.
- **Bounded invocation duration.** A handler invocation must complete before any pre-fetched credential expires. For sub-second function handlers this is trivially satisfied; for workflow handlers (ADR-0004), each step's pre-fetch is treated as a fresh invocation by the adapter, and step-spanning credentials must either be re-fetched at step boundaries or refreshed before they age out.
- **UTF-8 secret content.** `get_secret` returning `Option<&str>` commits the SDK to UTF-8 secrets at the handler-author surface. `cf-credstore-sdk`'s underlying `SecretValue` is `Vec<u8>`, so binary secrets (DER certificates, raw cryptographic keys) must be base64/hex-encoded at the credstore layer or routed through a separate API.
- **No mid-invocation rotation visibility.** Values rotated after pre-fetch are invisible to the in-flight invocation; handlers must not assume freshness within a single call.

### Confirmation

- `CredStoreEnvironment` carries an owned `HashMap<String, String>` populated at construction time — covered by a unit test in `environment.rs::tests`.
- The conformance suite includes a fixture that verifies pre-fetch ordering: any credstore I/O happens strictly before `FunctionHandler::call` is invoked.
- The workspace clippy lint `await_holding_lock = "deny"` (root `Cargo.toml`) reinforces this design boundary: any future regression that holds a lock across `.await` is a build break.

### Applicability

| Domain | Status | Notes |
|---|---|---|
| ARCH | Addressed | Sync trait shape; pre-fetch responsibility on the adapter. |
| INT | Addressed | Sync surface to handler authors; integration with `cf-credstore-sdk` localized to `CredStoreEnvironment`. |
| MAINT | Addressed | One implementation in the SDK (`CredStoreEnvironment`); engine-agnostic handler code. |
| PERF | Addressed | Credstore I/O off the per-call hot path (bounded by `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`). |
| SEC | Addressed (with caveat) | Mid-execution secret rotation is not visible to in-flight invocations — by design; freshness bounded to invocation start. |
| TEST | Addressed | Conformance fixture exercises pre-fetch ordering. |
| REL | N/A | No runtime state owned by the trait. |
| DATA | N/A | No persistence. |
| OPS | N/A | No deployable surface introduced. |
| COMPL / UX / BIZ | N/A | Internal SDK; no end-user or regulated surface. |

## Pros and Cons of the Options

### Synchronous `Environment`, pre-fetched

- Good, because handler code stays free of `.await` for config/secret lookups.
- Good, because credstore I/O is paid once per invocation, not once per lookup.
- Good, because the integration boundary with `cf-credstore-sdk` is localised to the adapter and `CredStoreEnvironment`.
- Bad, because mid-execution rotation is invisible until the next invocation.
- Bad, because the adapter must know which keys to pre-fetch (driven by function definition).

### Asynchronous `Environment`, lazy fetch

- Good, because mid-execution rotation is visible if the handler re-fetches.
- Bad, because every lookup adds an `.await` and a potential network round-trip on the hot path.
- Bad, because handler code becomes async even for trivial config reads.

### Hybrid: sync hits with async fallback

- Good, because hot lookups stay sync for the common case.
- Bad, because the handler author must know per-key which access path applies — the cognitive cost lands on every handler implementation, not just on the SDK.
- Bad, because the boundary between pre-fetched and fallback keys is brittle; small changes to the function definition silently move keys between the two modes.
- Bad, because two access methods coexist, doubling the conformance contract and the documentation surface.

## More Information

Within the SDK, `Environment` is the read-only, handler-side projection of credstore-resolved values. The parent thin-host ADR (`cpt-cf-serverless-runtime-adr-thin-host`) refers to the same data as "adapter-supplied invocation context"; the two terms describe the same flow at different layers.

### Related Decisions

- **ADR-0001** (async-trait everywhere) — `Environment` is the documented exception to that mandate. The exception is justified because the set of required keys is statically known and the credstore round-trips can be batched once per invocation.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-serverless-runtime-plugin-sdk-fr-environment-trait` — synchronous trait shape.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-context` — Context companion that travels alongside Environment.
- `cpt-cf-serverless-runtime-plugin-sdk-nfr-authoring-ergonomics` — handler code remains free of `.await` on lookups.
- `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead` — credstore I/O kept off the per-call hot path.
- `cpt-cf-serverless-runtime-plugin-sdk-component-environment` — component that owns the trait and the CredStoreEnvironment implementation.
- `cpt-cf-serverless-runtime-plugin-sdk-principle-impl-agnostic` — credstore types do not leak into the handler-author surface.
