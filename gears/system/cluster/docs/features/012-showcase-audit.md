# Feature: Showcase Examples & Traceability Audit

- [x] `p1` - **ID**: `cpt-cf-clst-featstatus-showcase-audit-implemented`

- [x] `p2` - `cpt-cf-clst-feature-showcase-audit`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Consumer Follows the Showcase Examples](#consumer-follows-the-showcase-examples)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Traceability Audit](#traceability-audit)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Showcase Example Crates](#showcase-example-crates)
  - [Pre-Archive Traceability Audit](#pre-archive-traceability-audit)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Demonstrates the canonical consumer patterns (single-primitive, multi-primitive, multi-profile) and the plugin-author builder/handle shape, and closes the change with the pre-archive documentation and traceability audit that verifies every requirement maps to a realizing design section or ADR and that code markers are wired.

### 1.2 Purpose

Until showcase examples and the audit land, consumers lack a canonical usage reference and the change lacks a final traceability gate. This feature provides both, demonstrating the capability-declaration and startup-failure behavior delivered by the contract and verifying end-to-end coverage.

**Requirements**: `cpt-cf-clst-fr-validation-capability-declarations`, `cpt-cf-clst-fr-validation-startup-fail`

This feature introduces no net-new contract identifiers; its purpose is demonstration and audit of the behavior delivered by the preceding features. The capability-validation requirements above are referenced as the behavior the examples demonstrate, not as net-new coverage (their coverage is assigned to the cache primitive feature in `DECOMPOSITION.md`).

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-clst-actor-platform-gear` | Follows the consumer examples to adopt the SDK |
| `cpt-cf-clst-actor-plugin-author` | Follows the plugin-author builder/handle example shape |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §9 (acceptance criteria), §13 (open questions)
- **Design**: [DESIGN.md](../DESIGN.md) §5 (traceability), §6 (showcase trade-off), §7 (open questions)
- **Dependencies**:
  - [x] `p2` - `cpt-cf-clst-feature-sdk-default-backends`
  - [x] `p2` - `cpt-cf-clst-feature-scoping-polyfill`
  - [x] `p2` - `cpt-cf-clst-feature-watch-auto-restart`
  - [x] `p2` - `cpt-cf-clst-feature-registration-observability`
  - [x] `p2` - `cpt-cf-clst-feature-smoke-tests`

**Review domains**:
- Security — not applicable: the SDK contract exposes no authentication or authorization surface; transport authentication, credential wiring, and tenant isolation are backend/plugin concerns deferred to the OOP deployment design (PRD §4.2).
- Performance — not applicable: this feature delivers example crates and a traceability audit with no runtime performance dimension.
- Reliability — not applicable: the examples and audit introduce no runtime behavior; they demonstrate and verify reliability properties delivered by the preceding features.

## 2. Actor Flows (CDSL)

### Consumer Follows the Showcase Examples

- [x] `p1` - **ID**: `cpt-cf-clst-flow-showcase-audit-consumer-examples`

**Actor**: `cpt-cf-clst-actor-platform-gear`

**Success Scenarios**:
- A consumer follows the single-primitive example, then the multi-primitive and multi-profile examples, and adopts the SDK without further guidance.

**Error Scenarios**:
- A consumer declares a capability the bound backend cannot meet — the example shows the startup failure and how to resolve it.

**Steps**:
1. [x] - `p1` - Consumer follows the single-primitive example to resolve and use one primitive - `inst-ex-single`
2. [x] - `p1` - Consumer follows the multi-primitive and multi-profile examples - `inst-ex-multi`
3. [x] - `p1` - **IF** a declared capability is unmet - `inst-ex-unmet`
   1. [x] - `p1` - The example shows the startup failure naming primitive, requirement, and provider, and how to fix the binding - `inst-ex-fix`
4. [x] - `p1` - Plugin author follows the builder/handle example to shape a plugin - `inst-ex-plugin`

## 3. Processes / Business Logic (CDSL)

### Traceability Audit

- [x] `p1` - **ID**: `cpt-cf-clst-algo-showcase-audit-traceability`

**Input**: The PRD, DESIGN, ADRs, DECOMPOSITION, FEATUREs, and code markers

**Output**: A verified bidirectional traceability result and resolved open questions

**Steps**:
1. [x] - `p1` - **FOR EACH** requirement in the PRD - `inst-ta-foreach`
   1. [x] - `p1` - Verify it maps to a realizing design section or ADR and to a feature - `inst-ta-map`
2. [x] - `p1` - Verify code traceability markers are wired for the relevant identifiers - `inst-ta-markers`
3. [x] - `p1` - Resolve the two open questions (the watch-lifecycle generalization of the relevant ADR) - `inst-ta-openq`
4. [x] - `p1` - **RETURN** the audit result with any gaps recorded - `inst-ta-return`

## 4. States (CDSL)

Not applicable — example crates and the audit introduce no entity lifecycle.

## 5. Definitions of Done

### Showcase Example Crates

- [x] `p1` - **ID**: `cpt-cf-clst-dod-showcase-audit-examples`

The system **MUST** provide showcase example crates demonstrating single-primitive usage, multi-primitive usage, multi-profile usage, and the plugin-author builder/handle shape.

**Implements**:
- `cpt-cf-clst-flow-showcase-audit-consumer-examples`

**Touches**:
- Entities: showcase example crates

### Pre-Archive Traceability Audit

- [x] `p1` - **ID**: `cpt-cf-clst-dod-showcase-audit-traceability`

The system **MUST** complete the pre-archive traceability audit verifying every requirement maps to a realizing design section or ADR and to a feature, confirm code markers are wired, and record resolution of the two open questions.

**Implements**:
- `cpt-cf-clst-algo-showcase-audit-traceability`

**Touches**:
- Entities: traceability audit record

## 6. Acceptance Criteria

- [x] Showcase examples cover single-primitive, multi-primitive, multi-profile, and plugin-author builder/handle usage.
- [x] An example demonstrates a capability-mismatch startup failure and its resolution.
- [x] The traceability audit confirms every requirement maps to a realizing design section or ADR and to a feature.
- [x] Code traceability markers are verified as wired for the relevant identifiers.
- [x] The two open questions are resolved and the resolution recorded.
