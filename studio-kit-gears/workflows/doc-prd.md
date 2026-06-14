---
cf-studio: true
type: workflow
name: cf-gears-doc-prd
description: Invoke when the user asks to author, write, revise, or generate a Gears PRD - e.g. "generate PRD", "write the PRD", "create product requirements", "capture actors, FR/NFR, use cases, public interfaces, or success criteria". Thin preset binding the PRD artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the PRD artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-doc-prd - PRD authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the PRD artifact KIND and template, injects embedded PRD-specific
generation rules, and delegates the full author -> deterministic-gate ->
semantic-review loop to `cf-write-docs`. It authors no content itself.

```pdsl
UNIT DocPrdPreset
PURPOSE: Bind the PRD artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: PRD (default PRD, scope workflow_run)
DO:
  SET ARTIFACT_KIND = PRD
  SET artifact_template = {prd_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = PRD and the gears PRD template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsPrdGenerationRules unit below as additional gears PRD authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the PRD file
  ALWAYS keep {prd_checklist} and {prd_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author PRD content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears PRD KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsPrdGenerationRules
PURPOSE: Generate or revise a Gears PRD from the template with deterministic validation.
WHEN:
  REQUIRE authoring or revising a PRD artifact
DO:
  LOAD {prd_template}
  RUN follow {prd_template} structure and section order
  RUN author requirements as WHAT and WHY, not implementation HOW
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {prd_checklist} review-only; NEVER load it during generation
  ALWAYS keep {prd_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs from the template patterns, including actor, fr, nfr, interface, contract, and usecase IDs
  ALWAYS use valid Gears PRD IDs and priority markers from the template; every requirement-like item needs a stable ID
  ALWAYS link covered UPSTREAM_REQS IDs when upstream requirements exist
  ALWAYS cover every UPSTREAM_REQS ID with at least one FR or NFR via a `Covers` field when an UPSTREAM_REQS document exists
  ALWAYS make functional and non-functional requirements observable or measurable
  ALWAYS define actors, user journeys, public interfaces, and success criteria when relevant
  ALWAYS explicitly state non-applicability with a reason for omitted critical domains
  ALWAYS preserve existing stable IDs; add new IDs only for new requirements
  NEVER duplicate constraints.toml; follow it only through deterministic validation and template rules
  NEVER include implementation tasks, architecture decisions, schema definitions, API specs, test cases, infrastructure specs, security implementation details, or code-level documentation
  NEVER leave placeholders, TODOs, TBDs in critical sections, dangling references, or unprioritized requirements
```
