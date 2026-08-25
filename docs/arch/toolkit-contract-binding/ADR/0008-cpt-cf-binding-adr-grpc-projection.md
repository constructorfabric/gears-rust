---
status: accepted
date: 2026-08-11
---

# gRPC Transport Projection via `#[toolkit::grpc_contract]`

**ID**: `cpt-cf-binding-adr-grpc-projection`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [More Information](#more-information)

## Context and Problem Statement

ADR-0001 establishes the base trait as the contract's source of truth and REST as
its first projection. DESIGN §7 listed gRPC as an open question with four parts:
proto-first vs. code-first generation, how it interacts with `tonic`, which
streaming shapes to support, and whether a gRPC projection can coexist with a
REST projection on the same base trait.

This ADR is written **after the fact**: `#[toolkit::grpc_contract]` shipped on
`feature/toolkit_contracts` (`grpc_contract.rs`, `proto_bridge.rs`,
`toolkit-contract-protogen`, a committed `proto.lock.toml`, and an integration
test), while PRD §4.2 and DESIGN §7 still described it as future work. Recording
the decisions here closes that gap and gives the shipped behaviour — including
two deliberate asymmetries with REST — a normative home.

## Decision Drivers

* **One base trait, many projections** — a gear that already exposes REST must be
  able to add gRPC without touching its domain contract or its consumers.
* **The `.proto` is a published artifact** — external teams generate clients from
  it, so it must be reviewable in diffs and stable across unrelated refactors,
  not regenerated implicitly at build time.
* **No panic on peer-controlled data** — a malformed field from a remote must not
  take the process down.
* **Server generation is out of scope** (ADR-0002) — service authors write the
  `tonic` server by hand; only the client is generated.

## Considered Options

* **Option A**: Proto-first — hand-author `.proto`, generate the Rust trait from it.
* **Option B**: Code-first with a committed `.proto` — the Rust contract is the
  source of truth; the `.proto` is generated from the contract IR, committed, and
  checked in CI.
* **Option C**: Code-first with no `.proto` artifact — derive `prost` messages
  directly from the DTOs and never materialize a schema file.

## Decision Outcome

Chosen option: **Option B — code-first with a committed `.proto`.**

Option A would put the schema and the Rust contract in two places with nothing
reconciling them — the exact drift ADR-0001 exists to prevent. Option C makes the
wire schema invisible: a reviewer cannot see that a field number changed, and
external consumers have nothing to generate against.

### Macro shape

```rust
#[toolkit::grpc_contract(
    package = "api_contracts.payment.v1",
    service = "PaymentApi",
    stubs_module = "crate::grpc::stubs"
)]
pub trait PaymentApiGrpc: PaymentApi {
    #[rpc(name = "Charge")]
    #[idempotency_level(NotIdempotent)]
    async fn charge(&self, ctx: SecurityContext, req: ChargeRequest)
        -> Result<ChargeResponse, CanonicalError>;

    #[rpc(name = "ListPayments")]
    #[idempotency_level(NoSideEffects)]
    #[streaming]
    fn list_payments(&self, ctx: SecurityContext, filter: ListPaymentsFilter)
        -> Result<PaymentSummary, CanonicalError>;
}
```

The macro emits four artifacts, **all client-side**: the cleaned trait, a
`<trait>_grpc_binding()` IR function, a `{Trait}Client` (gated on `grpc-client`),
and `impl BaseTrait for {Trait}Client` so the consumer keeps seeing
`Arc<dyn PaymentApi>`.

### `.proto` generation and the lock file

`toolkit-contract-protogen` renders a `.proto` from the contract IR. The result
is **committed**, and `proto.lock.toml` pins it so an unintended schema change
shows up as a lock-file diff rather than silently re-rendering during a build.
Regeneration is an explicit step (the SDK's `gen_grpc_proto` example).

### DTO ↔ proto bridging, and the panic rule

`#[derive(ProtoBridge)]` maps a Rust DTO onto its prost stub. Fields that have no
proto primitive (`Uuid`, `Decimal`) are marked `#[proto_bridge(via_string)]` and
cross the wire as strings.

That direction is fallible, and the distinction is load-bearing:

* `From<Proto> for Dto` is **infallible and panics** on an unparseable
  `via_string` field. It is for data this process produced.
* `TryFromProto` / the inherent `try_from_proto` is the fallible counterpart, and
  is **mandatory on every inbound path** — the generated client uses it for
  responses and stream items, and hand-written `tonic` servers must use it for
  requests.

The generated client originally used the panicking conversion on all three
response paths (unary, retryable unary, server-streaming), which made one
malformed field from a peer a remote crash. `TryFromProto` is a trait rather than
just an inherent method so codegen can convert any response type uniformly,
including `ProtoBridge` enums; a hand-written bridge used as a response type gets
a compile error rather than falling back to the panicking path.

Unknown enum discriminants are **not** an error: they log at `warn` and fall back
to `Default`, so a peer adding a variant does not break older clients. Callers
that need to detect it use `try_from_i32`.

### Idempotency and retry

`#[idempotency_level(NoSideEffects | Idempotent | NotIdempotent)]` is emitted into
the `.proto` as the standard `idempotency_level` option. `#[retryable]` on a
`NotIdempotent` method is a **compile error**: the client retries transient
transport failures, and a 502/504 can arrive after the server committed, so the
retry would duplicate the write.

### Streaming scope

**Server-streaming only.** Client-streaming and bidirectional RPCs are not
supported; there is no `Stream`-typed *parameter* form in the projection
vocabulary. A method needing them writes a `tonic` client by hand against the
same base trait.

### Coexistence with REST

A base trait may carry both projections. They generate independent clients
(`PaymentApiRestClient`, `PaymentApiGrpcClient`), both implement the base trait,
and `#[toolkit::provides(transports = [local, rest, grpc])]` selects one at
runtime from `client_wiring`. The `api-contracts` example ships all three.

### Known asymmetries with the REST projection

Recorded deliberately rather than left to be rediscovered:

* **No reconnect or idle timeout on gRPC server-streaming.** The REST/SSE path
  reconnects with `Last-Event-ID` and enforces an idle timeout; the gRPC path
  does neither — a dropped stream surfaces as an error to the caller.
* **`SecurityContext` is not compile-time mandatory.** A REST projection method
  without a plane context is rejected at macro-expansion time; a gRPC method is
  not, and simply sends no `authorization` metadata.
* **The policy stack does not run.** As with REST, `policies = [...]` on
  `#[toolkit::provides]` applies to the local transport only; the wiring logs a
  `warn!` when a non-local transport is selected with policies declared.

### Consequences

* The `.proto` and `proto.lock.toml` are review artifacts. A contract change that
  alters the wire schema will not merge without them being regenerated.
* `TryFromProto` is public API. A hand-written proto bridge used as an RPC
  response type must implement it.
* `MAX_PROBLEM_TRAILER_BYTES` bounds the RFC 9457 envelope carried in gRPC
  trailers at 4 KiB of pre-base64 JSON; larger problems are reduced to a minimal
  envelope and flagged with `x-toolkit-problem-truncated`.

### Confirmation

* Integration test: unary, retryable unary, error mapping, and server-streaming
  round-trips against an in-process `tonic` server (`grpc_integration.rs`).
* Hostile-peer test: a server returning malformed `via_string` values on all
  three response paths makes the client return `Err` without panicking
  (`grpc_hostile_peer.rs`).
* Negative compile test: `#[retryable]` on `NotIdempotent`
  (`grpc_retryable_non_idempotent` trybuild fixture).
* `ClientHub` test: a `PaymentApiGrpcClient` resolves as `Arc<dyn PaymentApi>`.

## Pros and Cons of the Options

### Option A: Proto-First

* Good, because the wire schema is explicit and idiomatic for gRPC teams.
* Bad, because the Rust contract and the `.proto` become two sources of truth
  with no compiler check between them.
* Bad, because it inverts ADR-0001 for one transport only.

### Option B: Code-First with a Committed `.proto` (chosen)

* Good, because the base trait stays the single source of truth.
* Good, because the schema is a reviewable artifact and external consumers have
  something to generate from.
* Good, because the lock file turns an accidental schema change into a visible
  diff.
* Neutral, because regeneration is a manual step authors must remember; CI
  checking the lock file is what makes that safe.

### Option C: Code-First with No `.proto`

* Good, because there is no artifact to keep in sync.
* Bad, because field-number changes become invisible in review — the classic way
  to break every deployed client at once.
* Bad, because external (non-Rust) consumers have nothing to generate against.

## More Information

* ADR-0001 — contract source of truth:
  [`0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
* ADR-0002 — spec limits and the manual-implementation escape hatch:
  [`0002-cpt-cf-binding-adr-openapi-spec-limits.md`](./0002-cpt-cf-binding-adr-openapi-spec-limits.md)
  — server generation is out of scope for both transports.
* ADR-0007 — contract versioning:
  [`0007-cpt-cf-binding-adr-contract-versioning.md`](./0007-cpt-cf-binding-adr-contract-versioning.md)
  — a `V<N>` marker is stripped before the projection-suffix check, so
  `PaymentApiV2Grpc` classifies like `PaymentApiGrpc`.
* Codegen: `libs/toolkit-contract-macros/src/grpc_contract.rs`,
  `proto_bridge.rs`; proto rendering: `libs/toolkit-contract-protogen`.
* Runtime helpers: `libs/toolkit-contract/src/grpc.rs` (status mapping, bearer
  metadata), `libs/toolkit-transport-grpc` (problem trailers, retry).
* Reference implementation: `examples/toolkit/api-contracts`.
