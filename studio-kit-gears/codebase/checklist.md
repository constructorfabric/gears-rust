# Constructor Studio SDLC Code Checklist (Kit-Specific)

ALWAYS open and follow `{cf-studio-path}/.core/requirements/code-checklist.md` FIRST

**Artifact**: Code Implementation (Constructor Studio SDLC)
**Version**: 1.0
**Purpose**: Kit-specific checks that require Constructor Studio SDLC artifacts (PRD/DESIGN/DECOMPOSITION/FEATURE/ADR) and/or Constructor Studio traceability.

---

## Table of Contents

1. [Traceability Preconditions](#traceability-preconditions)
2. [Semantic Alignment (SEM)](#semantic-alignment-sem)

---

## Traceability Preconditions

Before running the SDLC-specific checks:

- [ ] Determine traceability mode from `artifacts.toml` for the relevant system/artifact: `FULL` vs `DOCS-ONLY`
- [ ] If `FULL`: identify the design source(s) to trace (Feature design is preferred)
- [ ] If `DOCS-ONLY`: skip traceability requirements and validate semantics against provided design sources

---

## Semantic Alignment (SEM)

These checks are **Constructor Studio SDLC-specific** because they require Constructor Studio artifacts (Feature design, Overall Design, ADRs, PRD/DESIGN coverage) and/or Constructor Studio markers.

### SEM-CODE-001: Resolve Design Sources
**Severity**: HIGH

- [ ] Resolve Feature design via `@cpt-*` markers using the `cfs where-defined` or `cfs where-used` skill
- [ ] If no `@cpt-*` markers exist, ask the user to provide the Feature design location before proceeding
- [ ] If the user is unsure, search the repository for candidate feature designs and present options for user selection
- [ ] Resolve Overall Design by following references from the Feature design (or ask the user for the design path)

### SEM-CODE-002: FEATURE Context Semantics
**Severity**: HIGH

- [ ] Confirm code behavior aligns with the Feature Overview, Purpose, and key assumptions
- [ ] Verify all referenced actors are represented by actual interfaces, entrypoints, or roles in code
- [ ] Ensure referenced ADRs and related specs do not conflict with current implementation choices

### SEM-CODE-003: FEATURE Flows Semantics
**Severity**: HIGH

- [ ] Verify each implemented flow follows the ordered steps, triggers, and outcomes in Actor Flows
- [ ] Confirm conditionals, branching, and return paths match the flow logic
- [ ] Validate all flow steps marked with IDs are implemented and traceable

### SEM-CODE-004: FEATURE Algorithms Semantics
**Severity**: HIGH

- [ ] Validate algorithm steps match the Feature design algorithms (inputs, rules, outputs)
- [ ] Ensure data transformations and calculations match the described business rules
- [ ] Confirm loop/iteration behavior and validation rules align with algorithm steps

### SEM-CODE-005: FEATURE State Semantics
**Severity**: HIGH

- [ ] Confirm state transitions match the Feature design state machine
- [ ] Verify triggers and guards for transitions match defined conditions
- [ ] Ensure invalid transitions are prevented or handled explicitly

### SEM-CODE-006: FEATURE Definition of Done Semantics
**Severity**: HIGH

- [ ] Verify each requirement in Definition of Done is implemented and testable
- [ ] Confirm implementation details (API, DB, domain entities) match the requirement section
- [ ] If the implementation introduces or changes GTS identifiers, type schemas, well-known instances, discriminator/const-enum-like values, `x-gts-traits` / `x-gts-traits-schema`, or type-driven authorization/extension behavior, review against `{guidelines_gts}`
- [ ] Validate requirement mappings to flows and algorithms are satisfied
- [ ] Ensure PRD coverage (FR/NFR) is preserved in implementation outcomes
- [ ] Ensure Design coverage (principles, constraints, components, sequences, db tables) is satisfied

### SEM-CODE-006b: Idiomatic Rust / ToolKit Framework Conformance
**Severity**: HIGH

- [ ] Apply `{toolkit_pr_review_docs}/RULES.md` and `{toolkit_pr_review_docs}/rules/*.md` for idiomatic-Rust and ToolKit-framework conformance (architecture, async/concurrency, design/API, errors/panics, security, tests, framework compliance) — neither the generic Studio code checklist nor the SDLC semantic checks above cover this
- [ ] When reviewing a full PR diff, prefer running `cf-gears-toolkit-review` (local or PR mode) over re-deriving these findings ad hoc — it owns invoking the underlying `toolkit-pr-review` skill and its rule-module fallback, so this capability has one owner instead of being re-derived per caller

### SEM-CODE-007: Overall Design Consistency
**Severity**: HIGH

- [ ] Confirm architecture vision and system boundaries are respected
- [ ] Validate architecture drivers (FR/NFR) are still satisfied by implementation
- [ ] Verify ADR decisions are reflected in code choices or explicitly overridden
- [ ] Confirm principles and constraints are enforced in implementation
- [ ] Validate domain model entities and invariants are respected by code
- [ ] Confirm component responsibilities, boundaries, and dependencies match the component model
- [ ] Validate API contracts and integration boundaries are honored
- [ ] Confirm public contract changes preserve SDK-first boundaries when the source design requires reusable client behavior
- [ ] Confirm domain, API, and infrastructure responsibilities remain separated or the approved design deviation is cited
- [ ] Confirm privileged access stays behind the runtime or explicitly trusted boundary defined by the design
- [ ] Confirm HTTP routes use the canonical OperationBuilder/operation-registration path, not ad hoc route wiring
- [ ] Confirm HTTP-facing errors use the canonical Problem/RFC-9457 shape and do not expose stack traces, secrets, internal diagnostics, or implementation-only identifiers
- [ ] Confirm tenant, identity, authorization, and security context boundaries are preserved for security-sensitive code paths
- [ ] Confirm registry/autodetect/ignore, secrets, FIPS (see `{fips_requirements}`), privilege-boundary, and security-boundary changes have a review record naming the guardrail or deviation, rationale, owner, and validation performed
- [ ] If the design relies on GTS-based modeling, ensure the code follows `{guidelines_gts}` for identifier structure, traits usage, well-known instances, and extensibility patterns
- [ ] Verify interactions and sequences are implemented as described
- [ ] Ensure database schemas, constraints, and access patterns align with design
- [ ] Confirm topology and tech stack choices are not contradicted
- [ ] Document any deviation with a rationale and approval
- [ ] Verify test layering: unit and integration tests cover deterministic logic, domain rules, and integration boundaries at the right layer
- [ ] Verify E2E tests cover externally observable integration flows without replacing lower-level deterministic tests, and that the suite follows `{toolkit_unified_system}/13_e2e_testing.md`'s one-call-per-API-method coverage goal rather than adding redundant calls to an already-exercised method — but never reject a call that exercises a distinct observable outcome (an error or authorization denial) an existing happy-path call can't; that's the Coverage Goal's own rare-exception carve-out, not a redundant call
- [ ] Verify new test-support/fixture code extends the gear's existing shared helpers (`tests/common/mod.rs` or equivalent) instead of duplicating a parallel module — including that the gear's *first* slice scaffolded one deliberately shared module rather than a slice-scoped one that later slices then duplicated — and that test files are organized by production module/concern rather than one file per handler or edge-case cluster, per `{toolkit_unified_system}/12_unit_testing.md` (gears-rust#5322, gears-rust#5323)
- [ ] Verify compile-fail tests exist when the implementation exposes compile-time guarantees such as macro diagnostics, generated code contracts, type-state APIs, or security/type-system invariants
- [ ] For changes touching transactions, retries, migrations, pagination, or concurrent DB access, cross-check `{toolkit_unified_system}/14_db_behavior_testing.md`'s rules (retry safety, lock ordering, bind-limit safety, total order under pagination, migration up/down correctness) and confirm the corresponding test exists
- [ ] For security/authorization-sensitive or tenant-scoped changes, cross-check `{toolkit_unified_system}/16_defect_class_to_control_map.md` to confirm the control for the relevant defect class (tenant isolation bypass, missing authz gate, globally scoped tables, wire/storage mismatch, information disclosure, etc.) is actually present on this gear, not assumed
- [ ] Confirm coverage thresholds are taken from project-local policy, not assumed from the shared kit

---

Use `{cf-studio-path}/.core/requirements/code-checklist.md` for all generic code quality checks.
