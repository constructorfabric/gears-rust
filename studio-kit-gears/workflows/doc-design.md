---
cf-studio: true
type: workflow
name: cf-gears-doc-design
description: Invoke when the user asks to author, write, revise, validate, review, or fix a Gears DESIGN or system/technical design - e.g. "generate DESIGN", "design the gear", "define components / interfaces / architecture / boundaries". Kit preset that binds the DESIGN resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears DESIGN kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-doc-design - DESIGN preset

Binds the gears DESIGN template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT DesignPreset
PURPOSE: Bind the gears DESIGN kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = DESIGN, GEARS_DOC_SKILL = cf-gears-doc-design, GEARS_DOC_NEXT_SKILL = cf-gears-decompose
  SET GEARS_DOC_UPSTREAM = gears/<gear>/docs/PRD.md (required) and accepted gears/<gear>/docs/ADR/*.md (optional)
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/DESIGN.md, or DESIGN-<aspect>.md when the request names a companion design aspect
  SET GEARS_DOC_TEMPLATE = {design_template}, GEARS_DOC_RULES = {design_rules}, GEARS_DOC_CHECKLIST = {design_checklist}, GEARS_DOC_EXAMPLE = {design_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every DESIGN resource before continuing into the router
  NEVER author, validate, or review DESIGN content in this preset
```
