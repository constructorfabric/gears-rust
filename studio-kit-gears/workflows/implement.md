---
cf-studio: true
type: workflow
name: cf-gears-implement
description: Invoke when the user asks to implement, build, or write the code for a Gears FEATURE - e.g. "implement", "write the code", "build this feature", "implement FEATURE with @cpt-* traceability". Thin preset binding the CODE artifact KIND, delegating implementation and review to the core cf-coding engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the CODE artifact KIND and gears kit references, then delegates implementation and review to the core cf-coding workflow.
---

# cf-gears-implement - CODE implementation preset

This workflow is a thin preset over the core `cf-coding` authoring engine. It
binds the CODE artifact KIND, injects embedded CODE-specific generation rules,
points the implementation at a source FEATURE with `@cpt-*` traceability
markers, and delegates the full coder -> deterministic-gate -> semantic-review
loop to `cf-coding`. It authors no code itself.

```pdsl
UNIT ImplementPreset
PURPOSE: Bind the CODE artifact KIND and gears kit references, then delegate implementation and review to the core cf-coding workflow.
STATE:
  SET ARTIFACT_KIND: CODE (default CODE, scope workflow_run)
DO:
  SET ARTIFACT_KIND = CODE
  SET source_feature = the FEATURE artifact the implementation realizes
  LOAD {cf-studio-path}/.core/workflows/coding.md as the controlling implementation workflow
  CONTINUE CodingBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = CODE before delegating to cf-coding
  ALWAYS inject the embedded GearsCodeGenerationRules unit below as additional gears CODE traceability and implementation rules into every coder dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <code-path>` for code traceability in addition to the project's test, lint, typecheck, and build commands
  ALWAYS keep {codebase_checklist} review-only; semantic review and PR review MUST load it before code review dispatch, and generation MUST NOT load it
  ALWAYS require `@cpt-*` markers that trace implemented code back to the source FEATURE IDs when traceability mode is FULL
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-coding gates or verdicts
  NEVER author code in this preset; delegate all implementation and review to cf-coding
NOTES:
  cf-coding already drives the coder -> deterministic gate (tests/lint/typecheck/build plus cfs validate) -> semantic review loop; this preset only supplies the gears CODE KIND binding, embedded code generation rules, and source FEATURE contract.
```

```pdsl
UNIT GearsCodeGenerationRules
PURPOSE: Implement a Gears FEATURE with deterministic validation and traceability.
WHEN:
  REQUIRE implementing or revising code for a Gears FEATURE
DO:
  LOAD the source FEATURE artifact
  RUN determine traceability mode from the FEATURE or project configuration
  RUN split the FEATURE design into implementation slices from its flow, algorithm, state, and definition-of-done IDs
  RUN order slices by dependency and user-observable behavior, keeping each slice independently testable
  RUN identify risky slices touching registry/autodetect/ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior, or security-boundary logic
  RUN implement one slice at a time with TDD: write or update the failing test first, implement the smallest passing code, then refactor
  RUN after each slice, run deterministic validation with project tests, lint/typecheck/build when available, and `cfs validate --artifact <code-path>`
  RUN fix every deterministic finding and repeat validation until zero errors before starting the next slice
  RUN after each slice is deterministic-clean, run the semantic review loop for that slice and fix findings before starting the next slice
  RUN preserve existing behavior outside the current slice and requested FEATURE scope
RULES:
  ALWAYS keep {codebase_checklist} review-only; NEVER load it during generation
  ALWAYS treat the source FEATURE and linked DESIGN/ADR/PRD/UPSTREAM_REQS IDs as the implementation contract
  ALWAYS resolve the source FEATURE location before implementation; if it cannot be resolved from `@cpt-*` markers or user input, stop and ask for it
  ALWAYS create a slice plan from the FEATURE design before editing code; each slice must name the FEATURE IDs it implements
  ALWAYS finish the current slice's TDD, deterministic validation, and semantic review before moving to the next slice
  ALWAYS keep slice scope small enough that tests, code, CPT markers, and FEATURE checkbox/status updates can be reviewed together
  ALWAYS add `@cpt-begin` and `@cpt-end` markers for implemented CDSL IDs when traceability mode is FULL
  ALWAYS generate CPT marker IDs from existing FEATURE CDSL IDs; never invent implementation-only CPT IDs outside the source artifact contract
  ALWAYS keep CPT markers minimal, correctly nested, and attached to the code that realizes the referenced behavior
  ALWAYS update FEATURE implementation checkboxes/status only when the corresponding code and validation are complete
  ALWAYS implement every referenced flow step, algorithm requirement, state transition, and definition-of-done item that is in scope
  ALWAYS preserve PRD coverage outcomes and DESIGN principles, constraints, components, sequences, data contracts, and security requirements in code behavior
  ALWAYS preserve SDK-first public contracts, domain/API/infrastructure separation, runtime-owned privileged access, canonical OperationBuilder/operation-registration behavior, canonical API/error behavior, and safe wire errors unless the source FEATURE or DESIGN documents an approved deviation
  ALWAYS record explicit review evidence for slices touching registry/autodetect/ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior, or security-boundary logic in the slice plan, implementation summary, or review notes; the evidence must name the touched guardrail or deviation, rationale, owner, and validation performed
  ALWAYS add compile-fail tests when the implementation exposes compile-time guarantees such as macro diagnostics, generated code contracts, type-state APIs, or security/type-system invariants and the repository has an available compile-fail harness; otherwise document why the gate is not applicable and run the closest available compile/typecheck validation
  ALWAYS preserve existing stable IDs and markers; move markers only with the code they describe
  NEVER introduce orphan, duplicate, stale, or speculative `@cpt-*` markers
  NEVER broaden scope beyond the source FEATURE without an explicit upstream artifact change
  NEVER leave deterministic validation, tests, lint, typecheck, or build failures unresolved when the commands are available
```
