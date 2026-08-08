<!--
Created:  2026-05-14 by Constructor Tech
Updated:  2026-05-20 by Constructor Tech
-->
# Feature: Gear Scaffold


<!-- toc -->

- [Feature: Gear Scaffold](#feature-gear-scaffold)
  - [Feature Status](#feature-status)
  - [1. Feature Context](#1-feature-context)
    - [1.1 Overview](#11-overview)
    - [1.2 Purpose](#12-purpose)
    - [1.3 Actors](#13-actors)
    - [1.4 References](#14-references)
  - [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
    - [ToolKit Gear Registration](#toolkit-gear-registration)
    - [Smoke Test — Gear Loads](#smoke-test--gear-loads)
  - [4. States (CDSL)](#4-states-cdsl)
  - [5. Definitions of Done](#5-definitions-of-done)
    - [Cargo Crate Wired Into Workspace](#cargo-crate-wired-into-workspace)
    - [ToolKit Gear Declaration](#toolkit-gear-declaration)
    - [Layer Skeleton Directories](#layer-skeleton-directories)
    - [Baseline DomainError Enum](#baseline-domainerror-enum)
    - [Gear Registers In Example Server](#gear-registers-in-example-server)
    - [Smoke Test](#smoke-test)
  - [6. Acceptance Criteria](#6-acceptance-criteria)
  - [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

## Feature Status

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-featstatus-gear-scaffold`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-serverless-runtime-feature-gear-scaffold`

## 1. Feature Context

### 1.1 Overview

Bootstrap the `serverless-runtime` host crate as a ToolKit gear: Cargo.toml + `src/lib.rs` with `#[toolkit::gear]`, root-workspace wiring, skeleton layer directories (`api/`, `domain/`, `infra/`), a baseline `DomainError` enum, and a smoke test that confirms the gear registers in `cf-gears-example-server`. This feature is the **foundation** for every other host feature — no upstream feature dependencies, no SDK or plugin dependencies at compile time.

### 1.2 Purpose

Provide the minimum host-crate surface that ToolKit recognizes, so subsequent features (Function Registry, REST surface, etc.) have a place to live and a known gear-lifecycle entry point. Establishes the layer convention (`api/`, `domain/`, `infra/`) used by every later feature.

**Requirements**: `cpt-cf-serverless-runtime-nfr-composition-deps`

**Principles**: `cpt-cf-serverless-runtime-principle-pluggable-adapters`

### 1.3 Actors

Not applicable — this feature has no user-facing actor flows. It is infrastructure: a Cargo crate, a `#[toolkit::gear]` registration, and a smoke test. Subsequent features (F-02 Function Registry onward) introduce the Application Developer / Tenant Admin / Platform Operator interactions described in PRD §2.

### 1.4 References

- **PRD**: [PRD.md](../../../docs/PRD.md)
- **Design**: [DESIGN.md](../../../docs/DESIGN.md)
- **Decomposition**: [DECOMPOSITION.md §2.1](../DECOMPOSITION.md) — `cpt-cf-serverless-runtime-feature-gear-scaffold`
- **ADR**: [ADR-0005 Thin Host Gear, Fat Runtime Plugins](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
- **ToolKit reference**: `docs/toolkit_unified_system/01_overview.md`, `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md`, `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md`
- **Dependencies**: None (foundation feature)

## 2. Actor Flows (CDSL)

Not applicable — see §1.3. No actor-triggered flows belong in a pure scaffold feature. End-to-end user flows begin in F-02 Function Registry and later features.

## 3. Processes / Business Logic (CDSL)

### ToolKit Gear Registration

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-algo-gear-scaffold-toolkit-registration`

**Input**: ToolKit gear-orchestrator startup (host-process boot).

**Output**: `serverless-runtime` gear registered with ToolKit, lifecycle hooks wired, ClientHub slot reserved.

**Steps**:

1. [ ] - `p1` - Declare `#[toolkit::gear]` on the host crate's gear struct in `src/lib.rs` - `inst-declare-toolkit-gear`
2. [ ] - `p1` - Implement the ToolKit `Gear` trait stub (no clients registered yet in this feature) - `inst-impl-gear-trait`
3. [ ] - `p1` - Register the gear in the root `cf-gears-example-server` composition root - `inst-register-in-example-server`
4. [ ] - `p1` - **RETURN** gear-registration result via ToolKit lifecycle - `inst-return-registration`

### Smoke Test — Gear Loads

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-algo-gear-scaffold-smoke-test`

**Input**: Test binary spawning `cf-gears-example-server` with the new gear included.

**Output**: Assertion that the gear registers without panic and exposes a non-empty gear identity.

**Steps**:

1. [ ] - `p1` - Boot `cf-gears-example-server` test harness with `serverless-runtime` gear included - `inst-boot-test-harness`
2. [ ] - `p1` - Query ToolKit's directory/registry for the `serverless-runtime` gear identity - `inst-query-registry`
3. [ ] - `p1` - **IF** gear is absent OR registration emitted an error - `inst-check-presence`
   1. [ ] - `p1` - **RETURN** test FAIL with the diagnostic message - `inst-fail`
4. [ ] - `p1` - **RETURN** test PASS - `inst-pass`

## 4. States (CDSL)

Not applicable — the scaffold itself has no entity lifecycle. Gear lifecycle is fully owned by ToolKit's `Gear` trait and is not redefined here.

## 5. Definitions of Done

### Cargo Crate Wired Into Workspace

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-cargo-wired`

The system **MUST** add `gears/serverless-runtime/serverless-runtime/Cargo.toml` as a member of the root Cargo workspace, with the crate name `serverless-runtime` and dependencies limited to ToolKit core, the two contract crates (`serverless-runtime-sdk`, whose consumer trait the gear implements, and `serverless-runtime-plugin-sdk`, whose plugin trait the gear dispatches through), and any other crates already required by the ToolKit `#[toolkit::gear]` macro. **MUST NOT** depend on any plugin crate at compile time (per `cpt-cf-serverless-runtime-adr-thin-host`).

**Implements**:
- `cpt-cf-serverless-runtime-algo-gear-scaffold-toolkit-registration`

**Touches**:
- Files: `gears/serverless-runtime/serverless-runtime/Cargo.toml`, root `Cargo.toml`

### ToolKit Gear Declaration

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-toolkit-declaration`

The system **MUST** declare the gear struct in `src/lib.rs` annotated with `#[toolkit::gear]` and implementing the ToolKit `Gear` trait at the level required for registration. The implementation **MUST** be minimal — no client registrations, no lifecycle background tasks — those belong to later features.

**Implements**:
- `cpt-cf-serverless-runtime-algo-gear-scaffold-toolkit-registration`

**Touches**:
- Files: `gears/serverless-runtime/serverless-runtime/src/lib.rs`

### Layer Skeleton Directories

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-layer-dirs`

The system **MUST** create `src/api/`, `src/domain/`, and `src/infra/` directories with at least an empty `mod.rs` (or equivalent) each, so subsequent features have a known layer to place code in. Layer split follows ToolKit unified system docs (`02_gear_layout_and_sdk_pattern.md`).

**Touches**:
- Files: `gears/serverless-runtime/serverless-runtime/src/api/mod.rs`, `src/domain/mod.rs`, `src/infra/mod.rs`

### Baseline DomainError Enum

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-domain-error`

The system **MUST** define a gear-level `DomainError` enum stub in `src/domain/mod.rs` (or `src/domain/error.rs`) with at minimum a `Internal(String)` variant, deriving `Debug` + `thiserror::Error`. This is the placeholder for subsequent feature-specific error variants. RFC-9457 Problem mapping is **out of scope** of this DoD — that belongs to F-08.

**Touches**:
- Files: `gears/serverless-runtime/serverless-runtime/src/domain/error.rs`

### Gear Registers In Example Server

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-register-in-example`

The system **MUST** register the `serverless-runtime` gear in `cf-gears-example-server`'s composition root so it boots as part of the platform. The registration **MUST** not require any plugin or SDK-trait implementation to be present.

**Implements**:
- `cpt-cf-serverless-runtime-algo-gear-scaffold-toolkit-registration`

**Touches**:
- Files: `cf-gears-example-server` composition root (path per existing ToolKit gears' precedent)

### Smoke Test

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-dod-gear-scaffold-smoke-test`

The system **MUST** include an integration smoke test that boots a test instance of `cf-gears-example-server` and asserts the `serverless-runtime` gear is registered in ToolKit's directory without errors. The test **MUST** run under `cargo test -p serverless-runtime` (or the equivalent workspace test target) and gate the MVP PR.

**Implements**:
- `cpt-cf-serverless-runtime-algo-gear-scaffold-smoke-test`

**Touches**:
- Files: `gears/serverless-runtime/serverless-runtime/tests/smoke.rs` (or equivalent)

## 6. Acceptance Criteria

- [ ] `cargo build -p serverless-runtime` succeeds from a clean workspace state.
- [ ] `cargo clippy -p serverless-runtime -- -D warnings` succeeds.
- [ ] `cargo fmt --check` succeeds.
- [ ] The host crate has zero compile-time dependency on any plugin crate (verifiable via `cargo tree -p serverless-runtime --no-default-features`).
- [ ] `cf-gears-example-server` boots with the `serverless-runtime` gear included and the gear appears in ToolKit's runtime directory.
- [ ] The smoke test (`cpt-cf-serverless-runtime-dod-gear-scaffold-smoke-test`) passes locally and in CI.
- [ ] Layer directories (`api/`, `domain/`, `infra/`) exist with at minimum empty `mod.rs` placeholders.
- [ ] `DomainError` enum stub is present in `src/domain/` and used as the host crate's `Result<_, DomainError>` return alias.
- [ ] No `TODO`, `TBD`, or `FIXME` markers in the scaffold code (acceptable in commit body or PR description but not in committed source).

## 7. Non-Functional Considerations

- **KISS** (per project `CLAUDE.md`): keep the scaffold minimal — no premature abstractions, no anticipated extension points beyond the three layer directories. Subsequent features add what they need when they need it.
- **`cpt-cf-serverless-runtime-nfr-composition-deps`**: This feature is the foundation for the NFR — wiring the host crate into the workspace without coupling it to plugins or downstream gears establishes the composition pattern that later features must preserve.
- **Cross-crate boundary**: The two contract crates (`serverless-runtime-sdk`, docs at `gears/serverless-runtime/serverless-sdk/`, and `serverless-runtime-plugin-sdk`, not yet designed) are the only external dependencies the scaffold pulls in at the gear level. Plugin *implementation* crates are forbidden at compile time per `cpt-cf-serverless-runtime-adr-thin-host` — the plugin SDK is the contract, not an implementation.
- **Not in scope here** (covered by later features): REST endpoints (F-03), SeaORM entities (F-02/F-03), plugin dispatch (F-04), audit / error mapping (F-08), tenant policy (F-07). The scaffold deliberately ships with none of those.
