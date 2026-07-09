---
cf-studio: true
type: workflow
name: cf-gears-doc-adr
description: Invoke when the user asks to author, write, revise, generate, or record a Gears ADR or architecture decision - e.g. "generate ADR", "record a decision", "document why we chose X", "capture context / options / decision / consequences". Thin preset binding the ADR artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the ADR artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-doc-adr - ADR authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the ADR artifact KIND and template, injects embedded ADR-specific
generation rules, and delegates the full author -> deterministic-gate ->
semantic-review loop to `cf-write-docs`. It authors no content itself.

```pdsl
UNIT DocAdrPreset
PURPOSE: Bind the ADR artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: ADR (default ADR, scope workflow_run)
DO:
  SET ARTIFACT_KIND = ADR
  SET artifact_template = {adr_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = ADR and the gears ADR template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsAdrGenerationRules unit below as additional gears ADR authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the ADR file
  ALWAYS keep {adr_checklist} and {adr_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author ADR content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears ADR KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsAdrGenerationRules
PURPOSE: Generate or revise a Gears ADR with stable decision traceability.
WHEN:
  REQUIRE authoring or revising an ADR artifact
DO:
  LOAD {adr_template}
  RUN follow {adr_template} structure and section order
  RUN capture context, options, decision, and consequences
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {adr_checklist} review-only; NEVER load it during generation
  ALWAYS keep {adr_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs using `cpt-{system}-adr-{slug}` and keep them stable
  ALWAYS use valid Gears ADR IDs and status values from the template
  ALWAYS compare at least two viable options unless the user explicitly records a constrained decision
  ALWAYS record why the chosen option wins and why rejected options lose
  ALWAYS state decision drivers, decision scope, and review/supersession expectations
  ALWAYS link affected PRD, DESIGN, FEATURE, or UPSTREAM_REQS IDs when known
  ALWAYS preserve accepted ADR history; supersede with a new ADR instead of silently rewriting a final decision
  ALWAYS document performance, security, reliability, data, integration, operations, testing, compliance, UX, and business impact when applicable; otherwise state why not applicable
  NEVER include complete architecture descriptions, product requirements, implementation tasks, schema definitions, code, secrets, test implementation, or operational runbooks
  NEVER hide tradeoffs, consequences, migration obligations, or reversibility notes
  NEVER leave placeholders, TODOs, TBDs in critical sections, dangling references, or missing status fields
```
