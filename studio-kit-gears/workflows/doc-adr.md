---
cf-studio: true
type: workflow
name: cf-gears-doc-adr
description: Invoke when the user asks to author, write, revise, validate, review, fix, or record a Gears ADR or architecture decision - e.g. "generate ADR", "record a decision", "document why we chose X", "capture context / options / decision / consequences". Kit preset that binds the ADR resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears ADR kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-doc-adr - ADR preset

Binds the gears ADR template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT AdrPreset
PURPOSE: Bind the gears ADR kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = ADR, GEARS_DOC_SKILL = cf-gears-doc-adr, GEARS_DOC_NEXT_SKILL = cf-gears-doc-design
  SET GEARS_DOC_UPSTREAM = gears/<gear>/docs/PRD.md, GEARS_DOC_UPSTREAM_REQUIRED = true
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/ADR/<NNNN>-<slug>.md, with NNNN the next free four-digit number in that directory
  SET GEARS_DOC_TEMPLATE = {adr_template}, GEARS_DOC_RULES = {adr_rules}, GEARS_DOC_CHECKLIST = {adr_checklist}, GEARS_DOC_EXAMPLE = {adr_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every ADR resource before continuing into the router
  NEVER author, validate, or review ADR content in this preset
```
