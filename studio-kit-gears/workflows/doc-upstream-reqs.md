---
cf-studio: true
type: workflow
name: cf-gears-doc-upstream-reqs
description: Invoke when the user asks to author, write, revise, or generate Gears UPSTREAM_REQS - e.g. "generate upstream requirements", "capture upstream requirements", "write UPSTREAM_REQS", "document requirements from existing modules toward a future module". Thin preset binding the UPSTREAM_REQS artifact KIND, delegating authoring and review to the core cf-write-docs engine with gears kit resources.
version: 1.0
purpose: Thin preset that binds the UPSTREAM_REQS artifact KIND and gears kit references, then delegates authoring and review to the core cf-write-docs workflow.
---

# cf-gears-doc-upstream-reqs - UPSTREAM_REQS authoring preset

This workflow is a thin preset over the core `cf-write-docs` authoring engine.
It binds the UPSTREAM_REQS artifact KIND and template, injects embedded
UPSTREAM_REQS-specific generation rules, and delegates the full author ->
deterministic-gate -> semantic-review loop to `cf-write-docs`. It authors no
content itself.

```pdsl
UNIT DocUpstreamReqsPreset
PURPOSE: Bind the UPSTREAM_REQS artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: UPSTREAM_REQS (default UPSTREAM_REQS, scope workflow_run)
DO:
  SET ARTIFACT_KIND = UPSTREAM_REQS
  SET artifact_template = {upstream_reqs_template}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = UPSTREAM_REQS and the gears UPSTREAM_REQS template before delegating to cf-write-docs
  ALWAYS inject the embedded GearsUpstreamReqsGenerationRules unit below as additional gears UPSTREAM_REQS authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the UPSTREAM_REQS file
  ALWAYS keep {upstream_reqs_checklist} and {upstream_reqs_example} review-only; semantic review MUST load both before cf-semantic-reviewer-artifact dispatch, and generation MUST NOT load them
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author UPSTREAM_REQS content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review loop; this preset only supplies the gears UPSTREAM_REQS KIND binding and embedded generation rules.
```

```pdsl
UNIT GearsUpstreamReqsGenerationRules
PURPOSE: Generate or revise upstream requirements from existing modules toward a future module.
WHEN:
  REQUIRE authoring or revising an UPSTREAM_REQS artifact
DO:
  LOAD {upstream_reqs_template}
  RUN follow {upstream_reqs_template} structure and section order
  RUN capture source module needs as WHAT and WHY, not implementation HOW
  RUN generate or update the Table of Contents with `cfs toc <path>`
  RUN validate the Table of Contents with `cfs validate-toc <path>`
  RUN deterministic validation with `cfs validate --artifact <path>`
  RUN fix every deterministic finding and repeat validation until zero errors
RULES:
  ALWAYS keep {upstream_reqs_checklist} review-only; NEVER load it during generation
  ALWAYS keep {upstream_reqs_example} review-only; semantic review MUST load it when checking depth and example conformance
  ALWAYS generate and maintain an accurate Table of Contents matching the final headings
  ALWAYS generate canonical CPT IDs using `cpt-{system}-upreq-{slug}` and keep them unique within the artifact
  ALWAYS use valid upstream requirement IDs and priority markers from the template
  ALWAYS name the requesting module, future module boundary, and reason for every requirement
  ALWAYS state observable acceptance signals for each upstream requirement
  ALWAYS make every requirement traceable back to concrete requesting gear code or documentation
  ALWAYS include a traceability section linking to future PRD and DESIGN, even when those artifacts are not created yet
  ALWAYS preserve existing stable IDs; add new IDs only for new upstream requirements
  ALWAYS keep downstream PRD/DESIGN/FEATURE coverage references explicit when they exist
  NEVER include product vision, roadmap goals, implementation choices, internal algorithms, or crate-level design decisions
  NEVER leave placeholders, TODOs, TBDs, dangling references, or unprioritized requirements
```
