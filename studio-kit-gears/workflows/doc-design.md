---
cf-studio: true
type: workflow
name: cf-gears-doc-design
description: Invoke when the user asks to author, write, revise, generate, or produce a Gears DESIGN or system/technical design - e.g. "generate DESIGN", "design the gear", "define components / interfaces / architecture / boundaries". Thin preset binding the DESIGN artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the DESIGN artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-doc-design - DESIGN authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the DESIGN artifact KIND and template, injects embedded DESIGN-specific
generation rules, and delegates the full author -> deterministic-gate ->
semantic-review loop to `cf-write-docs`. It authors no content itself.

```pdsl
UNIT DocDesignPreset
PURPOSE: Bind the DESIGN artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: DESIGN (default DESIGN, scope workflow_run)
DO:
  SET ARTIFACT_KIND = DESIGN
  SET artifact_template = {design_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = DESIGN and the gears DESIGN template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsDesignGenerationRules unit below as additional gears DESIGN authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the DESIGN file
  ALWAYS keep {design_checklist} and {design_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author DESIGN content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears DESIGN KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsDesignGenerationRules
PURPOSE: Generate or revise a Gears DESIGN from PRD, ADR, and upstream context.
WHEN:
  REQUIRE authoring or revising a DESIGN artifact
DO:
  LOAD {design_template}
  RUN follow {design_template} structure and section order
  RUN model the gear architecture, boundaries, interfaces, data/control flow, and operational behavior
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {design_checklist} review-only; NEVER load it during generation
  ALWAYS keep {design_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs from the template patterns, including design, tech, principle, constraint, entity, component, interface, seq, db, dbtable, and topology IDs
  ALWAYS use valid Gears DESIGN IDs and priority markers from the template
  ALWAYS trace design elements to PRD, ADR, and UPSTREAM_REQS IDs when those sources exist
  ALWAYS preserve PRD intent and scope; document any deviation as explicit approved scope change
  ALWAYS cover referenced ADR decisions and keep ADR/PRD links valid
  ALWAYS define ownership boundaries, public interfaces, lifecycle/state behavior, and error surfaces when relevant
  ALWAYS document deviations from shared platform, security, API, testing, and architecture baselines with the deviation, rationale, and review owner
  ALWAYS preserve SDK-first public contracts, domain/API/infrastructure separation, and runtime-owned privileged access unless an approved design deviation says otherwise
  ALWAYS define REST contract metadata, canonical OperationBuilder/operation-registration behavior, canonical Problem/RFC-9457 error envelopes, and safe wire-error behavior when exposing HTTP APIs
  ALWAYS document security boundaries, data protection, fault tolerance, observability, testability, and compliance posture when applicable; otherwise state why not applicable
  ALWAYS call out assumptions, constraints, dependencies, and migration impacts
  ALWAYS preserve existing stable IDs; add new IDs only for new design elements
  NEVER include spec-level details, decision debates, product requirements, implementation tasks, code snippets, infrastructure code, test code, schema implementation, API specs, or secrets
  NEVER leave placeholders, TODOs, TBDs in critical sections, dangling references, or unprioritized design elements
```
