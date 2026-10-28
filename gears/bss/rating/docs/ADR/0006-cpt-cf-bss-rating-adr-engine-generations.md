---
status: proposed
date: 2026-10-12
decision-makers: "BSS Rating team"
---

Created:  2026-10-12 by Virtuozzo International GmbH
Updated:  2026-10-12 by Virtuozzo International GmbH

# ADR-0006: Retain Frozen Engine Generations

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Retained frozen generations (chosen)](#retained-frozen-generations-chosen)
  - [Semantic version only](#semantic-version-only)
  - [Always re-evaluate with the latest engine](#always-re-evaluate-with-the-latest-engine)
  - [Pin external crates by version or checksum](#pin-external-crates-by-version-or-checksum)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-engine-generations`

## Context and Problem Statement

Rating copies everything it rates (T-D-41) so that a final result re-executes byte-identically
from its own store years later, and only an administrative re-rate may re-price final usage
under the same commercial input (T-D-21, T-D-24; a new fact version is a new input, T-D-73). T-D-46 recorded an `engine_version` string on every result. A version string names old
code but cannot run it: after an upgrade, a correction or a replay would run the new code.

Evaluation also runs code Rating does not own. The Products derived-usage evaluator
(`bss_products_sdk::derived::evaluate`) applies formulas and output rounding to each granule, and
the shared money crate (`bss_money`) performs the exact arithmetic (ADR-0005). A dependency bump of
either crate can change an amount without any change in Rating's source.

How does Rating keep old results executable and stop semantics from changing silently?

## Decision Drivers

* Replay and corrections must run the semantics a result was produced with (T-D-41, T-D-24).
* Financial results are retained for at least 7 years (DESIGN §4.12).
* A change in an external crate that runs inside evaluation is a change of semantics.
* Missing code must fail by name; it must never fall back to other code.

## Considered Options

1. Retained frozen engine generations (chosen).
2. A semantic version recorded on each result, with no retained code.
3. Always re-evaluate with the latest engine.
4. Retained generations that pin external crates by version or `Cargo.lock` checksum only.

## Decision Outcome

Chosen option: **retained frozen engine generations** (DECISIONS T-D-65, refining T-D-46).

- **Generation.** Every change of evaluation semantics is a new module
  `rating_core::engine::gen_<n>`, frozen once released. Semantics include formulas, step order,
  exact-arithmetic rules, the arithmetic budget, numeric bounds and error classification. A refactor
  that keeps semantics stays in its generation and must pass that generation's golden vectors
  bit for bit.
- **External code is vendored.** Every external crate whose code runs inside evaluation is vendored
  into the generation's frozen tree when the generation is released, as a renamed path crate: the
  `bss_products_sdk::derived` evaluator, `bss_money` (its exact and money modules), and the registry
  crates they compute with (`rust_decimal`, `bigdecimal`, `num-bigint`, `num-integer`, `num-traits`,
  `arrayvec`, `libm`, as resolved). A registry copy is taken from the release
  whose `Cargo.lock` checksum the workspace recorded. Pinning by version or by `Cargo.lock` does not
  work: Cargo records no checksum for path crates such as `cf-gears-bss-products-sdk` and
  `cf-gears-bss-money`, so a change to `derived.rs` never shows in the lockfile; and Cargo refuses
  two semver-compatible versions of `rust_decimal` as renamed dependencies, so an old version could
  not link beside a new one. Renamed path crates are distinct crates and do link side by side.
- **Isolation.** Each generation is its own crate (`cf-gears-bss-rating-engine-gen-<n>`). A CI check
  fails it if it depends on anything but its vendored copies and allowlisted non-numeric crates
  (`serde`, `uuid`, `time`, `thiserror`); the vendored set is the full transitive closure of the
  numeric code (including `bigdecimal`, `num-bigint`, `num-integer`, `num-traits`, `arrayvec` and `libm`). The generation
  crate defines its own frozen input and output types; `rating-core` depends on it, never the
  reverse, and maps values into those types, numbers as canonical text.
- **Digest.** `engine_digest` is SHA-256 over the generation's frozen source tree, including the
  vendored copies, and its golden-vector outputs. The releasing migration
  records it in `bss_rating__engine_generation`. At start-up each instance verifies that every
  generation referenced by a retained result is compiled in with a matching digest; otherwise it
  does not start the rater.
- **Dispatch.** `EngineRegistry` dispatches by the recorded `engine_generation` and never
  substitutes another one. A generation that is missing, or whose digest differs, fails
  `engine_unavailable`.
- **Engine-of-record.** New work uses the current generation. The generation in force when a child
  first becomes final is its engine-of-record. Input corrections use it; only an administrative
  re-rate (a `RerateService` run with a frozen `target_engine_generation`) moves it. Results record
  `(engine_generation, engine_digest)`, and each delivery lists its `engine_generations`.
- **Retention.** A generation is kept while any retained result references it. Removing one needs an
  ADR and proof that no retained result references it, usually by re-rating those results first.

### Consequences

* The binary grows with every generation, and old generations must keep compiling with the current
  toolchain.
* An upgrade of `bss-products-sdk`, `bss-money` or `rust_decimal` in the workspace changes no
  released generation. Adopting it in evaluation is a Rating release with a new generation and new
  vendored copies.
* Each generation carries copies of third-party code with their licences, and a Products fix must
  be ported by vendoring it into a new generation.
* A Products evaluator fix does not reach finalized results until an administrative re-rate.
* The re-execution promise of T-D-41 holds. Weakening it to an audit-only guarantee would be a
  product decision.

### Confirmation

* CI runs the golden corpus of every retained generation.
* A cross-upgrade replay test: results produced by generation `n` re-execute identically in a build
  whose current generation is `n + 1`.
* A dependency check (`cargo tree`) fails CI when a generation crate depends on anything outside its
  vendored copies and the allowlist.
* A test edits a vendored file and shows that the generation's digest no longer matches.
* Start-up tests: a missing generation or a wrong digest keeps the rater stopped; a replay of a
  result whose generation is absent returns `engine_unavailable`.

## Pros and Cons of the Options

### Retained frozen generations (chosen)

* Good: old results execute exactly; corrections never upgrade semantics silently; an upstream
  change cannot reach a released generation.
* Bad: retained code, vendored third-party copies and golden corpora for many years.

### Semantic version only

* Good: no retained code.
* Bad: the version is a label. A replay after an upgrade runs different code, so the
  determinism promise becomes an audit claim that cannot be checked.

### Always re-evaluate with the latest engine

* Good: one code path; fixes apply everywhere at once.
* Bad: an ordinary correction can change the amount of every line of a fact, not just the corrected
  input; a replay cannot reproduce what was invoiced.

### Pin external crates by version or checksum

* Good: no copied code.
* Bad: does not work. Path crates have no `Cargo.lock` checksum, so a change to the Products
  evaluator is not detected, and Cargo cannot link two semver-compatible versions of one registry
  crate, so an old generation could not keep its version.

## More Information

- Engine generations and replay: [DESIGN §4.1](../DESIGN.md#41-versioning-and-historical-correctness);
  data lifecycle: [DESIGN §4.12](../DESIGN.md#412-data-lifecycle).
- The Products evaluator:
  [`gears/bss/products/products-sdk/src/derived.rs`](../../../products/products-sdk/src/derived.rs);
  the money crate: [`gears/bss/libs/money`](../../../libs/money/src/exact.rs).
- Re-rate behaviour: [`../features/08-corrections-rerate.md`](../features/08-corrections-rerate.md).

## Traceability

- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) T-D-65 (this decision), T-D-46 (refined),
  T-D-41, T-D-21, T-D-24, R-33 (a bound change is a new generation).
- **Design**: [DESIGN §3.2](../DESIGN.md#32-component-model) (`EngineRegistry`),
  [DESIGN §4.1](../DESIGN.md#41-versioning-and-historical-correctness),
  [DESIGN §4.12](../DESIGN.md#412-data-lifecycle).
- **Related ADRs**: [ADR-0002](./0002-cpt-cf-bss-rating-adr-rating-gear-consolidation.md)
  (`rating-core` crate boundary), [ADR-0005](./0005-cpt-cf-bss-rating-adr-exact-money.md).
