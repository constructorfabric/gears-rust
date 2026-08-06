---
status: accepted
date: 2026-05-18
---

# ADR-0001: Use `async-trait` for SDK Async Trait Declarations

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Applicability](#applicability)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [`async-trait` macro](#async-trait-macro)
  - [Native RPITIT with hand-written `+ Send` plumbing](#native-rpitit-with-hand-written--send-plumbing)
  - [Manual `Pin<Box<dyn Future + Send>>` returns](#manual-pinboxdyn-future--send-returns)
- [More Information](#more-information)
  - [Related Decisions](#related-decisions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-adr-async-trait`

## Context and Problem Statement

Every async trait the SDK declares (`RuntimeAdapter`, `FunctionHandler`, `WorkflowHandler`, `ServerlessRuntimeClient`) is invoked from multi-threaded async runtimes; the returned futures must carry a `Send` bound or callers cannot spawn them. Stable Rust's native `async fn` in traits (return-position `impl Trait` in traits, RPITIT) does not bound the returned future with `Send`, and there is no ergonomic way to add it on stable today.

## Decision Drivers

- Adapters and the host run on multi-threaded `tokio` (or equivalent); spawned futures require `Send`.
- Adapter authors should write plain `async fn …` in trait impls — no manual `Pin<Box<…>>` plumbing.
- Public API documentation must remain readable (NFR `nfr-api-docs`).
- SDK is constrained to stable Rust. This is a workspace-wide policy — [`rust-toolchain.toml`](../../../../../rust-toolchain.toml) pins the channel to `1.97.0` and the root [`Cargo.toml`](../../../../../Cargo.toml) sets the MSRV `rust-version = "1.95.0"`. The SDK's local restatement is [`cpt-cf-serverless-runtime-plugin-sdk-constraint-stable-rust`](../DESIGN.md#stable-rust--no-nightly-features); any RPITIT + `Send` workaround that requires nightly is out-of-bounds.

## Considered Options

- `async-trait` macro (`Pin<Box<dyn Future + Send>>` under the hood).
- Native RPITIT with hand-written `+ Send` plumbing per method.
- Manual `Pin<Box<dyn Future + Send>>` returns on every method.

## Decision Outcome

Chosen: **`async-trait` macro**, because it is the only option that combines stable-Rust eligibility, `Send` bounds on every returned future, and unmodified `async fn` syntax at the impl site.

### Consequences

- Every public async trait method in the crate compiles to one boxed, heap-allocated future per call.
- The low-overhead NFR ([`cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`](../DESIGN.md#nfr-allocation)) is preserved: cost is bounded at one `Box<dyn Future>` allocation per dispatch (plus the one `tracing` span already accounted for in the NFR allocation map), not per inner `.await`.
- The macro is added as a stable, single-purpose dependency; no other macro crate is permitted to shadow it. `async-trait` is already a workspace dependency at the root `Cargo.toml` and is pulled in by 30+ crates across CF/Gears — this ADR ratifies the existing convention rather than introducing it.
- The choice is a deliberate, scoped use of trait-object type erasure (`Box<dyn Future + Send>`); the general Rust guidance to prefer `impl Trait` over `Box<dyn Trait>` is consciously overridden here by the stable-Rust + `Send`-bound requirements.
- Migration path is explicit: revisit this ADR when stable Rust offers an ergonomic way to attach `Send` to RPITIT-returned futures.

### Confirmation

- `#![deny(missing_docs)]` plus the workspace-level `clippy::missing_errors_doc` / `clippy::missing_panics_doc` denies ensure `cargo doc --no-deps` builds without warnings; this realizes `cpt-cf-serverless-runtime-plugin-sdk-nfr-api-docs`.
- The adapter conformance suite spawns handler futures on a multi-threaded executor; any non-`Send` future surfaces as a compile error at the spawn site.

### Applicability

| Domain | Status | Notes |
|---|---|---|
| ARCH | Addressed | Async trait shape across SDK and adapters. |
| INT | Addressed | Public trait surface (`RuntimeAdapter`, `FunctionHandler`, `WorkflowHandler`, `ServerlessRuntimeClient`). |
| MAINT | Addressed | Single macro dependency, already shared workspace-wide; migration path documented (revisit when stable RPITIT carries `Send`). |
| PERF | Addressed | One `Box<dyn Future>` per dispatch, bounded by `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`. |
| TEST | Addressed | Conformance suite enforces `Send` at compile time. |
| SEC | N/A | Library crate; no runtime trust boundary introduced by this choice. |
| REL | N/A | No runtime state; trait declaration only. |
| DATA | N/A | No persistence. |
| OPS | N/A | No deployable surface. |
| COMPL / UX / BIZ | N/A | Internal SDK; no end-user or regulated surface. |

## Pros and Cons of the Options

### `async-trait` macro

- Good, because adapter authors write idiomatic `async fn` in impls with no extra annotations.
- Good, because returned futures are `Send` by construction.
- Good, because it is the de-facto stable-Rust pattern for `Send`-bounded async traits today.
- Bad, because each call introduces one `Box<dyn Future>` allocation.
- Neutral, because the macro adds one dependency the crate already accepts.

### Native RPITIT with hand-written `+ Send` plumbing

- Good, because no macro and no heap allocation on the future itself.
- Bad, because attaching `Send` to RPITIT-returned futures is awkward and verbose on stable Rust today.
- Bad, because adapter authors must learn and repeat the pattern at every impl site.

### Manual `Pin<Box<dyn Future + Send>>` returns

- Good, because explicit; no macro magic.
- Bad, because every method body becomes a hand-rolled `Box::pin(async move { … })`, reducing adapter ergonomics.
- Bad, because trait declarations stop looking like `async fn`, which hurts readability and discoverability.

## More Information

This decision is library-wide and applies to every public async trait the SDK exports. It is expected to be revisited when native `async fn` in traits gains an ergonomic `Send` bound on stable Rust.

### Related Decisions

- **ADR-0002** (sync `Environment`) is the declared *exception* to this async-trait-everywhere stance: credstore I/O is pre-fetched so that `Environment` can expose a synchronous, allocation-free getter on the hot path.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-trait` — async trait shape for FunctionHandler.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-send-sync` — `Send + Sync + 'static` bound made possible by the macro.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-workflow-handler-trait` — same async-trait expansion for compensation.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-invoke` — async dispatch by host.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-control` — async cancel/suspend/resume.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-schedule` — async schedule bind/update/revoke.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-event-trigger` — async trigger bind/update/revoke.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-client-index-events` — async index-event emission.
- `cpt-cf-serverless-runtime-plugin-sdk-nfr-authoring-ergonomics` — plain `async fn` syntax in adapter impls.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-handler-trait` — interface contract.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-workflow-trait` — interface contract.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-adapter` — interface contract.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-client` — interface contract.
