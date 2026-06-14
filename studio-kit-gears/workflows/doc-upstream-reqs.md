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
It binds the UPSTREAM_REQS artifact KIND and the gears kit resources (template,
rules, checklist, example), injects UPSTREAM_REQS-specific authoring rules, and
delegates the full author -> deterministic-gate -> semantic-review loop to
`cf-write-docs`. It authors no content itself.

```pdsl
UNIT DocUpstreamReqsPreset
PURPOSE: Bind the UPSTREAM_REQS artifact KIND and gears kit references, then delegate authoring and review to the core cf-write-docs workflow.
STATE:
  SET ARTIFACT_KIND: UPSTREAM_REQS (default UPSTREAM_REQS, scope workflow_run)
DO:
  SET ARTIFACT_KIND = UPSTREAM_REQS
  SET artifact_template = {upstream_reqs_template}
  SET artifact_rules = {upstream_reqs_rules}
  SET artifact_checklist = {upstream_reqs_checklist}
  SET artifact_example = {upstream_reqs_example}
  LOAD {cf-studio-path}/.core/workflows/write-docs.md as the controlling authoring workflow
  CONTINUE WriteDocsBootstrap
RULES:
  ALWAYS bind ARTIFACT_KIND = UPSTREAM_REQS and the four gears UPSTREAM_REQS references (template, rules, checklist, example) before delegating to cf-write-docs
  ALWAYS inject {upstream_reqs_rules} as additional gears UPSTREAM_REQS authoring rules into every author dispatch
  ALWAYS set the deterministic gate target to `cfs validate --artifact <path>` for the UPSTREAM_REQS file
  ALWAYS pass {upstream_reqs_checklist} as the artifact checklist to cf-semantic-reviewer-artifact and {upstream_reqs_example} as the content-depth reference
  ALWAYS carry ARTIFACT_KIND and the bound references as read-only preset data, never overriding cf-write-docs gates or verdicts
  NEVER author UPSTREAM_REQS content in this preset; delegate all authoring and review to cf-write-docs
NOTES:
  cf-write-docs already drives the author -> deterministic gate (cfs validate --artifact) -> semantic review (cf-semantic-reviewer-artifact) loop; this preset only supplies the gears UPSTREAM_REQS KIND binding and UPSTREAM_REQS-specific rules.
```
