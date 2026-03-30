<!--
Created: 2026-03-30 by Constructor Tech
Updated: 2026-03-30 by Constructor Tech
-->
---
status: proposed
date: 2026-03-23
---
<!--
=============================================================================
ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
=============================================================================
RULES:
 - ADRs represent actual decision dilemma and decision state
 - DESIGN is the primary artifact ("what"); ADRs annotate DESIGN with rationale ("why")
 - Use single ADR per decision

STANDARDS ALIGNMENT:
 - MADR (Markdown Any Decision Records)
 - IEEE 42010 (architecture decisions as first-class elements)
 - ISO/IEC 15288 / 12207 (decision analysis process)
==============================================================================
-->

# ADR — Use `async-trait` for Handler and WorkflowHandler Traits


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: `async-trait`](#option-a-async-trait)
  - [Option B: RPITIT with explicit `Send` bound](#option-b-rpitit-with-explicit-send-bound)
  - [Option C: `dynosaur` crate](#option-c-dynosaur-crate)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-async-trait`
## Context and Problem Statement

`FunctionHandler<I, O>` and `WorkflowHandler<I, O>` define async methods (`call`, `compensate`)
so handler implementations can perform async work in their bodies. Adapters dispatch
handler calls on multi-threaded tokio runtimes, which requires the `Future` returned
from `call` and `compensate` to be `+ Send`. Rust stable does not provide automatic
`+ Send` bounds on `Future`s returned from `async fn` in traits.

Which approach should be used to define async trait methods with `+ Send` Future
guarantees, while keeping the implementation ergonomic for function authors and
compatible with stable Rust?

## Decision Drivers

* **[P1]** The crate must compile on stable Rust 1.92.0 with no nightly features
  (`cpt-cf-serverless-sdk-core-constraint-stable-rust`) — hard constraint; no option
  that requires nightly is acceptable.
* **[P1]** Handler trait implementations must produce `Future` values that are `+ Send`,
  because adapters dispatch handler calls across multi-threaded tokio runtimes — correctness
  requirement; a solution that does not satisfy `Send` is invalid.
* **[P2]** Function authors should write `async fn call(...)` with no lifetime annotations
  or boilerplate (`cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics`) — the simpler
  the authoring surface, the lower the barrier to implementing the trait correctly.

## Considered Options

* **Option A**: `async-trait` crate — `#[async_trait]` attribute on trait and impls,
  ergonomic `async fn` syntax, `Box<dyn Future + Send>` expansion under the hood;
  `dyn FunctionHandler<I, O>` trait objects remain object-safe.
* **Option B**: RPITIT with explicit `Send` bound — `fn call<'a>(&'a self, ...) -> impl Future<Output = ...> + Send + 'a`
  method signatures; no extra dependency; no boxing; trait objects (`dyn FunctionHandler<I, O>`)
  are **not** object-safe with RPITIT methods.
* **Option C**: `dynosaur` crate — proc-macro generates a vtable-based `DynHandler`
  wrapper that restores `dyn Trait` object safety for async traits; boxing of the
  returned `Future` is shifted into the generated vtable rather than the call site,
  but a heap allocation still occurs per call.

> Note: plain `async fn` in traits without a `+ Send` bound was considered and
> immediately eliminated — without a named associated type for the returned `Future`,
> callers cannot write a stable `where`-clause to constrain it to `Send`, which
> violates the [P1] correctness requirement.

## Decision Outcome

Chosen option: **Option A: `async-trait`**, because it satisfies all three decision
drivers simultaneously: it compiles on stable Rust 1.92.0
(`cpt-cf-serverless-sdk-core-constraint-stable-rust`), the expanded
`Pin<Box<dyn Future + Send>>` return type guarantees `+ Send` on every handler
future without any per-impl annotation (`cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics`),
and `dyn FunctionHandler<I, O>` trait objects remain object-safe — which Option B (RPITIT)
cannot provide. The one heap allocation per invocation is acceptable: a handler
invocation is a coarse-grained unit of work, not a hot inner loop, and the cost is
bounded by `cpt-cf-serverless-sdk-core-nfr-low-overhead`.

### Consequences

* All `FunctionHandler<I, O>` and `WorkflowHandler<I, O>` implementors must annotate their `impl`
  blocks with `#[async_trait]`.
* The returned `Future` from `call` and `compensate` is heap-allocated (`Box<dyn Future>`),
  adding one allocation per invocation. Acceptable because each invocation is a coarse
  unit of work, not a hot inner loop (`cpt-cf-serverless-sdk-core-nfr-low-overhead`).
* Trait object dispatch (`dyn FunctionHandler<I, O>`) is possible without special ergonomics,
  which simplifies adapter type-erased handler registries.

### Confirmation

* `cargo check` on stable 1.92.0 must succeed with `#[async_trait]` in place —
  the primary acceptance criterion.
* `cargo check` on stable 1.92.0 must fail when `#[async_trait]` is removed from
  `handler.rs` and `workflow.rs` — confirming the attribute is load-bearing, not
  accidental.
* All handler impls in tests and examples must use `#[async_trait]`.
* Code review: no `impl Future` / RPITIT syntax in `handler.rs` or `workflow.rs`.

## Pros and Cons of the Options

### Option A: `async-trait`

Crate attribute expands `async fn` to `fn ... -> Pin<Box<dyn Future + Send>>`.

* Good, because implementors write natural `async fn call(...)` — no lifetime annotations.
* Good, because `Future + Send` is guaranteed automatically without per-impl constraints.
* Good, because trait objects `dyn FunctionHandler<I, O>` work out of the box.
* Neutral, because one heap allocation per invocation (acceptable for coarse-grained unit of work).
* Bad, because implementors must remember `#[async_trait]` on every `impl` block.

### Option B: RPITIT with explicit `Send` bound

```rust
fn call<'a>(
    &'a self,
    ctx: &'a Context,
    env: &'a dyn Environment,
    input: I,
) -> impl Future<Output = Result<O, ServerlessSdkError>> + Send + 'a;
```

* Good, because zero runtime overhead — no boxing.
* Good, because no additional dependency.
* Bad, because each method needs explicit lifetime and `+ Send + 'a` annotation — verbose.
* Bad, because trait objects `dyn FunctionHandler<I, O>` are **not** object-safe with RPITIT methods,
  preventing adapters from storing `Box<dyn FunctionHandler<I, O>>`.
* Bad, because the explicit lifetime on the return position is non-obvious and error-prone
  for function authors unfamiliar with RPITIT semantics.

### Option C: `dynosaur` crate

Proc-macro generates a vtable-based `DynHandler<dyn FunctionHandler<I, O>>` wrapper, restoring
`dyn Trait` object safety for async traits. Boxing of the returned `Future` is shifted
into the generated vtable rather than exposed at the call site, but a heap allocation
still occurs per call.

* Good, because `dyn FunctionHandler<I, O>` object safety is preserved (the exact weakness of
  Option B), so adapters can store `Box<dyn FunctionHandler<I, O>>` without changes.
* Good, because handler authors still write `async fn call(...)` — ergonomics on par
  with Option A.
* Bad, because a heap allocation still occurs per call — the boxing is internal to the
  generated vtable, not eliminated.
* Bad, because `dynosaur` is a young crate (first stable release ~2024) with a smaller
  adoption base than `async-trait`; API stability is not yet guaranteed.
* Bad, because the generated `DynHandler` wrapper type leaks into adapter code that
  stores type-erased handlers, making the indirection non-obvious to readers.
* Bad, because `dynosaur`'s proc-macro output is harder to audit than `async-trait`'s
  well-known `Box<dyn Future + Send>` expansion.

## More Information

- [`async-trait` crate](https://crates.io/crates/async-trait) — crate documentation and changelog
- Rust tracking issue for `async fn` in traits with object safety and `Send` bound support:
  [rust-lang/rust#91611](https://github.com/rust-lang/rust/issues/91611)

## Non-Applicable Domains

The following checklist domains are not applicable to this ADR and are explicitly excluded:

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| SEC | N/A | Decision concerns trait ergonomics and Rust async machinery; no credential management, no authentication, no user data |
| REL | N/A | No stateful data or availability SLO; library crate with no deployment artifact |
| DATA | N/A | No data storage, schema, or persistence involved in this decision |
| OPS | N/A | Pure library; no deployment topology, monitoring, or infrastructure concern |
| COMPL | N/A | Internal developer tooling; no regulatory, certification, or legal requirement |
| UX | N/A | No end-user UI or user-facing strings; developer ergonomics addressed in Decision Outcome |
| BIZ | N/A | Internal Rust library crate; no business stakeholder buy-in, cost analysis, or time-to-market consideration applicable to a trait mechanism decision |

## Review Conditions

This ADR should be revisited when any of the following conditions is met:

| Trigger | Action |
|---------|--------|
| `dyn Trait` object safety for `async fn` methods stabilises on stable Rust | Re-evaluate Option B; RPITIT itself is stable since Rust 1.75 (within our 1.92 baseline), but Option B is still blocked because RPITIT methods are not object-safe — `Box<dyn FunctionHandler<I, O>>` is not possible without `async-trait` or equivalent. Track the Rust `dyn async fn` object safety initiative; migration removes the `async-trait` dependency but requires updating all `impl` blocks and is a breaking change for downstream implementors |
| `async-trait` crate is deprecated or unmaintained | Migrate to RPITIT or `dynosaur` (Option C) depending on object-safety requirements |
| `dynosaur` reaches a stable 1.0 API and gains broad ecosystem adoption | Re-evaluate Option C as an alternative that preserves `dyn FunctionHandler<I, O>` object safety; assess whether its per-call allocation profile is meaningfully different from Option A in practice |
| A handler use case emerges that cannot tolerate per-invocation heap allocation (hot inner loop) | Evaluate Option C (`dynosaur`) for that specific handler category; note that per-call allocation is not eliminated but may be restructured |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-sdk-core-fr-handler-trait` — async `call` method signature
* `cpt-cf-serverless-sdk-core-fr-handler-send-sync` — `Future + Send` guarantee on handler futures
* `cpt-cf-serverless-sdk-core-fr-workflow-handler-trait` — async `compensate` method signature
* `cpt-cf-serverless-sdk-core-constraint-stable-rust` — no nightly features required
* `cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics` — plain `async fn` syntax, no lifetime annotations on `impl` blocks
* `cpt-cf-serverless-sdk-core-nfr-low-overhead` — one allocation per invocation; boxing cost bounded by this NFR
* `cpt-cf-serverless-sdk-core-component-handler` — async trait implementation
* `cpt-cf-serverless-sdk-core-component-workflow` — async supertrait implementation
