---
cf-studio: true
type: workflow
name: cf-gears-doc-prd
description: Invoke when the user asks to author, write, revise, validate, review, or fix a Gears PRD - e.g. "generate PRD", "write the PRD", "create product requirements", "capture actors, FR/NFR, use cases, public interfaces, or success criteria". Kit preset that binds the PRD resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears PRD kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-doc-prd - PRD preset

Binds the gears PRD template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT PrdPreset
PURPOSE: Bind the gears PRD kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = PRD, GEARS_DOC_SKILL = cf-gears-doc-prd, GEARS_DOC_NEXT_SKILL = cf-gears-doc-design
  SET GEARS_DOC_UPSTREAM = gears/<gear>/docs/UPSTREAM_REQS.md, GEARS_DOC_UPSTREAM_REQUIRED = false
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/PRD.md
  SET GEARS_DOC_TEMPLATE = {prd_template}, GEARS_DOC_RULES = {prd_rules}, GEARS_DOC_CHECKLIST = {prd_checklist}, GEARS_DOC_EXAMPLE = {prd_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every PRD resource before continuing into the router
  NEVER author, validate, or review PRD content in this preset
```
