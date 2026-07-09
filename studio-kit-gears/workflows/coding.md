---
cf-studio: true
type: workflow
name: cf-gears-coding
description: Invoke when the user asks to code, implement, build, or revise Gears code directly from DESIGN, ADR, PRD, or upstream design context without a FEATURE artifact or @cpt-* implementation traceability.
version: 1.0
purpose: Thin preset that binds non-FEATURE Gears code work to the core cf-coding workflow while reusing Gears implementation guardrails without FEATURE traceability.
---

# cf-gears-coding - DESIGN-led CODE preset

This workflow is a thin preset over the core `cf-coding` authoring engine. It
is for Gears code changes where the implementation contract is a DESIGN, ADR,
PRD, UPSTREAM_REQS, or explicit user-supplied design context rather than a
FEATURE artifact. Use `cf-gears-implement` instead when a FEATURE artifact is
the source of truth or when `@cpt-*` implementation traceability is required.

```pdsl
UNIT CodingPreset
PURPOSE: Bind DESIGN-led Gears code work and delegate implementation/review to the core cf-coding workflow.
STATE:
  SET ARTIFACT_KIND: CODE (default CODE, scope workflow_run)
DO:
  SET ARTIFACT_KIND = CODE
  SET source_design_context = the DESIGN, ADR, PRD, UPSTREAM_REQS, or explicit design context the implementation realizes
  LOAD {cf-studio-path}/.core/workflows/coding.md as the controlling implementation workflow
  CONTINUE CodingBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = CODE before delegating to cf-coding
  ALWAYS inject the embedded GearsDesignLedCodingRules unit below as additional Gears CODE implementation rules into every coder dispatch
  ALWAYS keep {codebase_checklist} review-only; semantic review and PR review MUST load it before code review dispatch, and generation MUST NOT load it
  ALWAYS carry ARTIFACT_KIND and the bound source_design_context as read-only preset data, never overriding cf-coding gates or verdicts
  NEVER require a FEATURE artifact, FEATURE checkbox/status update, or `@cpt-*` implementation marker in this preset
  NEVER author code in this preset; delegate all implementation and review to cf-coding
NOTES:
  cf-coding drives the coder -> deterministic gate -> semantic review loop. This preset only supplies Gears CODE rules for code work that starts from design context instead of FEATURE traceability.
```

```pdsl
UNIT GearsDesignLedCodingRules
PURPOSE: Implement Gears code from design context without FEATURE traceability.
WHEN:
  REQUIRE implementing or revising Gears code from DESIGN, ADR, PRD, UPSTREAM_REQS, or explicit design context without a FEATURE artifact
DO:
  LOAD the source design context
  RUN derive implementation slices from the source design's boundaries, interfaces, sequences, data contracts, security requirements, and acceptance constraints
  RUN order slices by dependency and user-observable behavior, keeping each slice independently testable
  RUN identify risky slices touching registry/autodetect/ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior, or security-boundary logic
  RUN implement one slice at a time with TDD: write or update the failing test first, implement the smallest passing code, then refactor
  RUN after each slice, run deterministic validation with project tests, lint/typecheck/build when available
  RUN fix every deterministic finding and repeat validation until zero errors before starting the next slice
  RUN after each slice is deterministic-clean, run the semantic review loop for that slice and fix findings before starting the next slice
  RUN preserve existing behavior outside the current slice and requested design scope
RULES:
  ALWAYS keep {codebase_checklist} review-only; NEVER load it during generation
  ALWAYS treat the source DESIGN, ADR, PRD, UPSTREAM_REQS, or explicit design context as the implementation contract
  ALWAYS resolve the source design context before implementation; if it cannot be resolved from user input or repository context, stop and ask for it
  ALWAYS create a slice plan from the source design context before editing code; each slice must name the design element, ADR, requirement, or user-supplied constraint it implements
  ALWAYS finish the current slice's TDD, deterministic validation, and semantic review before moving to the next slice
  ALWAYS preserve SDK-first public contracts, domain/API/infrastructure separation, runtime-owned privileged access, canonical OperationBuilder/operation-registration behavior, canonical API/error behavior, and safe wire errors unless the source design context documents an approved deviation
  ALWAYS record explicit review evidence for slices touching registry/autodetect/ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior, or security-boundary logic in the slice plan, implementation summary, or review notes; the evidence must name the touched guardrail or deviation, rationale, owner, and validation performed
  ALWAYS add compile-fail tests when the implementation exposes compile-time guarantees such as macro diagnostics, generated code contracts, type-state APIs, or security/type-system invariants and the repository has an available compile-fail harness; otherwise document why the gate is not applicable and run the closest available compile/typecheck validation
  ALWAYS preserve PRD outcomes, DESIGN principles, constraints, components, sequences, data contracts, and security requirements in code behavior when those sources exist
  ALWAYS preserve existing stable IDs and markers; move markers only with the code they describe
  NEVER add, require, remove, or rewrite `@cpt-*` markers solely for this DESIGN-led preset
  NEVER update FEATURE implementation checkboxes or statuses in this preset
  NEVER broaden scope beyond the source design context without an explicit upstream artifact or user-approved scope change
  NEVER leave deterministic validation, tests, lint, typecheck, or build failures unresolved when the commands are available
```
