---
cf-studio: true
type: workflow
name: cf-gears-doc-upstream-reqs
description: Invoke when the user asks to author, write, revise, validate, review, or fix Gears UPSTREAM_REQS - e.g. "generate upstream requirements", "capture upstream requirements", "write UPSTREAM_REQS", "document requirements from existing modules toward a future module". Kit preset that binds the UPSTREAM_REQS resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears UPSTREAM_REQS kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-doc-upstream-reqs - UPSTREAM_REQS preset

Binds the gears UPSTREAM_REQS template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT UpstreamReqsPreset
PURPOSE: Bind the gears UPSTREAM_REQS kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = UPSTREAM_REQS, GEARS_DOC_SKILL = cf-gears-doc-upstream-reqs, GEARS_DOC_NEXT_SKILL = cf-gears-doc-prd
  SET GEARS_DOC_UPSTREAM = the code or docs of each requesting gear named in the request, resolved under that requesting gear's own directory rather than the target gear (required, at least one)
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/UPSTREAM_REQS.md
  SET GEARS_DOC_TEMPLATE = {upstream_reqs_template}, GEARS_DOC_RULES = {upstream_reqs_rules}, GEARS_DOC_CHECKLIST = {upstream_reqs_checklist}, GEARS_DOC_EXAMPLE = {upstream_reqs_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every UPSTREAM_REQS resource before continuing into the router
  NEVER author, validate, or review UPSTREAM_REQS content in this preset
```
