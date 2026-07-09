---
cf-studio: true
type: workflow
name: cf-gears-decompose
description: Invoke when the user asks to decompose, break down, or author/revise a Gears DECOMPOSITION - e.g. "decompose", "break into features", "create the feature list / plan", "order features and dependencies with coverage back to PRD/DESIGN". Thin preset binding the DECOMPOSITION artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the DECOMPOSITION artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-decompose - DECOMPOSITION authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the DECOMPOSITION artifact KIND and template, injects embedded
DECOMPOSITION-specific generation rules, and delegates the full author ->
deterministic-gate -> semantic-review loop to `cf-write-docs`. It authors no
content itself.

```pdsl
UNIT DecomposePreset
PURPOSE: Bind the DECOMPOSITION artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: DECOMPOSITION (default DECOMPOSITION, scope workflow_run)
DO:
  SET ARTIFACT_KIND = DECOMPOSITION
  SET artifact_template = {decomposition_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = DECOMPOSITION and the gears DECOMPOSITION template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsDecompositionGenerationRules unit below as additional gears DECOMPOSITION authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the DECOMPOSITION file
  ALWAYS keep {decomposition_checklist} and {decomposition_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author DECOMPOSITION content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears DECOMPOSITION KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsDecompositionGenerationRules
PURPOSE: Generate or revise a Gears feature decomposition with coverage back to requirements and design.
WHEN:
  REQUIRE authoring or revising a DECOMPOSITION artifact
DO:
  LOAD {decomposition_template}
  RUN follow {decomposition_template} structure and section order
  RUN split work into cohesive FEATURE candidates with explicit dependencies
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {decomposition_checklist} review-only; NEVER load it during generation
  ALWAYS keep {decomposition_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs using `cpt-{system}-status-{slug}` and `cpt-{system}-feature-{slug}` for decomposition status and feature entries
  ALWAYS use valid Gears feature IDs, statuses, and priority markers from the template
  ALWAYS trace every feature candidate to PRD, DESIGN, ADR, or UPSTREAM_REQS IDs when those sources exist
  ALWAYS provide 100 percent explicit coverage for required design elements and requirements passthrough
  ALWAYS list Requirements Covered, Design Components, Sequences, and Data for every feature, using `None` only when explicitly true
  ALWAYS make dependencies, ordering constraints, and parallelization opportunities explicit
  ALWAYS keep each feature independently implementable and testable where practical
  ALWAYS keep checkbox/status consistency: parent checked only when all nested referenced blocks are checked, and no duplicate checkbox IDs exist within a feature block
  ALWAYS preserve existing stable IDs; add new IDs only for new feature candidates
  NEVER create orphan features with no upstream coverage or no clear definition of done
  NEVER include implementation details, new requirement definitions, or architecture decisions
  NEVER leave placeholders, TODOs, TBDs, dangling references, or unprioritized features
```
