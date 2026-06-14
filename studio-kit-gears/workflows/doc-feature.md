---
cf-studio: true
type: workflow
name: cf-gears-doc-feature
description: Invoke when the user asks to author, write, revise, generate, or spec a Gears FEATURE - e.g. "generate FEATURE", "spec the feature", "define flows / algorithms / states / definition of done (CDSL)", "write test scenarios for a feature". Thin preset binding the FEATURE artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the FEATURE artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-doc-feature - FEATURE authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the FEATURE artifact KIND and template, injects embedded
FEATURE-specific generation rules, and delegates the full author ->
deterministic-gate -> semantic-review loop to `cf-write-docs`. It authors no
content itself.

```pdsl
UNIT DocFeaturePreset
PURPOSE: Bind the FEATURE artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: FEATURE (default FEATURE, scope workflow_run)
DO:
  SET ARTIFACT_KIND = FEATURE
  SET artifact_template = {feature_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = FEATURE and the gears FEATURE template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsFeatureGenerationRules unit below as additional gears FEATURE authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the FEATURE file
  ALWAYS keep {feature_checklist} and {feature_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author FEATURE content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears FEATURE KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsFeatureGenerationRules
PURPOSE: Generate or revise a Gears FEATURE as an implementation-ready contract.
WHEN:
  REQUIRE authoring or revising a FEATURE artifact
DO:
  LOAD {feature_template}
  RUN follow {feature_template} structure and section order
  RUN define flows, algorithms, states, data contracts, and definition of done in CDSL-ready form
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {feature_checklist} review-only; NEVER load it during generation
  ALWAYS keep {feature_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs from the template patterns, including featstatus, feature, flow, algo, state, and dod IDs
  ALWAYS use valid Gears FEATURE IDs, CDSL IDs, statuses, and priority markers from the template
  ALWAYS include the `featstatus` checkbox and the DECOMPOSITION `feature` backreference directly under the H1 title
  ALWAYS trace the FEATURE to DECOMPOSITION, DESIGN, PRD, ADR, or UPSTREAM_REQS IDs when those sources exist
  ALWAYS preserve PRD coverage integrity and DESIGN principles, constraints, components, sequences, and data references
  ALWAYS document feature-local deviations from shared platform, security, API, testing, or architecture baselines with the deviation, rationale, and review owner
  ALWAYS preserve SDK-first public contracts, domain/API/infrastructure separation, runtime-owned privileged access, and canonical API/error behavior when those apply to the FEATURE
  ALWAYS keep `featstatus` checkbox state consistent with flow, algorithm, state, and definition-of-done checkbox states
  ALWAYS define testable acceptance criteria and deterministic completion signals
  ALWAYS define security, reliability, data integrity, observability, rollback, test-layering, and compile-time-gate behavior when applicable; otherwise state why not applicable
  ALWAYS include implementation constraints that code must satisfy, without prescribing incidental code structure
  ALWAYS preserve existing stable IDs; add new IDs only for new feature/CDSL elements
  NEVER introduce new system-level type definitions, API endpoints, architecture decisions, product requirements, sprint tasks, code snippets, test implementation, infrastructure code, or secrets
  NEVER create CDSL steps that cannot be implemented, tested, or traced
  NEVER leave placeholders, TODOs, TBDs, dangling references, or unprioritized CDSL elements
```
