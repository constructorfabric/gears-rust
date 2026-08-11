<!--
Created: 2026-08-11 by Constructor Tech
Updated: 2026-08-11 by Constructor Tech
-->

# Decomposition: CF/Gears Serverless Runtime SDK


<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Domain Model Foundation - HIGH](#21-domain-model-foundation---high)
  - [2.2 Published Consumer Client Contract - HIGH](#22-published-consumer-client-contract---high)
  - [2.3 Test Support and Contract Verification - HIGH](#23-test-support-and-contract-verification---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-sdk-status-overall`

## 1. Overview

The consumer SDK is decomposed into three cohesive features: the transport-agnostic domain
model, the published client contract, and opt-in test support. The order follows the crate's
dependency direction: operations depend on stable value types, and the test double depends on
the completed trait surface.

This split gives explicit coverage to every PRD FR/NFR and every identified DESIGN principle,
constraint, entity, component, interface, and sequence. No feature owns persistence, transport,
runtime-plugin behavior, or executable business logic.

The PRD's consumer-developer and runtime-gear actors are intentionally not allocated to features:
they identify participants in the contract, not deliverables to implement.

`cpt-cf-serverless-runtime-sdk-nfr-api-docs` intentionally appears in all three features because
each introduces public API that it must document. The test-support feature additionally owns the
CI enforcement that keeps the complete public surface documented.

The contract remains draft while host gaps G-01, G-02, G-05, and G-06 are open. Those decisions
stay in the host documentation and are not duplicated as SDK features here.

FEATURE artifacts are intentionally deferred until this decomposition is accepted. When authored,
they will use the feature IDs and slugs defined below; the headings remain unlinked until then.

## 2. Entries

### 2.1 Domain Model Foundation - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-sdk-feature-domain-model-foundation`

- **Purpose**: Establish the engine-neutral value types and crate boundaries used by the
  published client and test support.

- **Depends On**: None

- **Scope**:
  - Invocation request, outcome, summary, status, control, identifier, and paging value types
  - Opaque callable identifiers and opaque JSON input/output payloads
  - Redacted failure summaries with no backend-authored message or arbitrary details
  - Crate dependency, unsafe-code, and public-module boundaries
  - Documentation for every public model and identifier type

- **Out of scope**:
  - Client trait operations and refusal mappings
  - Runtime or plugin implementation behavior
  - Test-double behavior

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-payload-opacity`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-payload-exposure`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-nfr-engine-neutrality`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-nfr-no-unsafe`
  - [ ] `p2` - `cpt-cf-serverless-runtime-sdk-nfr-api-docs`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-principle-opaque-ids`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-constraint-no-engine-deps`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-constraint-no-unsafe`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-constraint-dep-direction`

- **Domain Model Entities**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-entity-invocation`

- **Design Components**: None

- **API**: Public value types in `src/models.rs`; no callable operations.

- **Sequences**: None

- **Data**: None — the SDK owns no persistence.

- **Phases**: Single-phase implementation.

---

### 2.2 Published Consumer Client Contract - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-sdk-feature-consumer-client-contract`

- **Purpose**: Publish the in-process trait that consuming gears use to invoke, observe, and
  control automation with typed refusal semantics.

- **Depends On**: `cpt-cf-serverless-runtime-sdk-feature-domain-model-foundation`

- **Scope**:
  - Invoke, synchronous-result, dry-run, and idempotency contract operations
  - Invocation read, query, status, control, and replay operations
  - Refusal taxonomy, refusal-versus-failure distinction, and HTTP problem-type parity
  - ClientHub-facing trait surface and read-locality contract
  - Documentation for the public client and error contract

- **Out of scope**:
  - HTTP, JSON-RPC, MCP, or other transport handlers
  - Callable, schedule, trigger, or tenant-policy administration
  - Runtime-plugin traits, event ports, and conformance behavior
  - Test-double implementation

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-invoke`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-sync-result`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-dry-run`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-idempotency`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-read-run`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-query-runs`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-run-states`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-control`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-replay`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-refusal-reasons`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-failure-vs-refusal`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-refusal-parity`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-nfr-read-locality`
  - [ ] `p2` - `cpt-cf-serverless-runtime-sdk-nfr-api-docs`

- **PRD Interfaces**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-interface-client`

- **Integration Contracts**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-contract-refusal-parity`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-contract-run-states`

- **Use Cases**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-usecase-start-async`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-usecase-check-outcome`
  - [ ] `p2` - `cpt-cf-serverless-runtime-sdk-usecase-cancel`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-principle-refusal-vs-failure`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-principle-minimal-surface`

- **Design Constraints Covered**: None — inherited from the domain foundation.

- **Domain Model Entities**: Uses the invocation entities owned by the domain foundation.

- **Design Components**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-component-api`

- **API**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-interface-client-v1`
  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-interface-error`

- **Sequences**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-seq-invoke`

- **Data**: None — reads return host-owned invocation-index projections through the trait.

- **Phases**: Single-phase implementation after the inherited host gaps are resolved.

---

### 2.3 Test Support and Contract Verification - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-sdk-feature-test-support`

- **Purpose**: Let consuming gears verify success and failure handling without running the
  Serverless Runtime while enforcing public-surface documentation quality.

- **Depends On**: `cpt-cf-serverless-runtime-sdk-feature-consumer-client-contract`

- **Scope**:
  - Opt-in configurable implementation of `ServerlessRuntimeClientV1`
  - Consumer-controlled outcomes for success, refusal, and callable failure paths
  - Contract-level tests for operation results and refusal distinctions
  - Public API documentation enforcement

- **Out of scope**:
  - Runtime, backend, database, or transport integration
  - Runtime-plugin conformance testing
  - Production behavior or production dependencies when `test-util` is disabled

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-fr-test-double`
  - [ ] `p2` - `cpt-cf-serverless-runtime-sdk-nfr-api-docs`

- **PRD Interfaces**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-sdk-interface-test-double`

- **Design Principles Covered**: None

- **Design Constraints Covered**: None — inherited through the published contract.

- **Domain Model Entities**: None — reuses the published contract types.

- **Design Components**: None — the test double implements the existing consumer component.

- **API**: `test-util` feature exposing the configurable test double.

- **Sequences**: None

- **Data**: None — configured outcomes are test-process state only.

- **Phases**: Single-phase implementation.

## 3. Feature Dependencies

```text
cpt-cf-serverless-runtime-sdk-feature-domain-model-foundation
                         ↓
cpt-cf-serverless-runtime-sdk-feature-consumer-client-contract ← blocked by external host gaps G-01, G-02, G-05, and G-06
                         ↓
cpt-cf-serverless-runtime-sdk-feature-test-support
```

**Dependency Rationale**:

- The consumer client contract requires the domain model because every operation exchanges its
  value types.
- The consumer client contract is additionally blocked by host gaps G-01, G-02, G-05, and G-06.
  These are external contract prerequisites rather than SDK features, so they are not listed in
  its `Depends On` field.
- Test support requires the finalized client contract because it implements that trait and
  configures its outcomes.
- The dependency chain is intentionally linear; no later feature can be implemented completely
  before the public types and trait it consumes are defined.
