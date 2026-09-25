---
cf-studio: true
type: workflow
name: cf-gears-decompose
description: Invoke when the user asks to decompose, break down, or author, revise, validate, review, or fix a Gears DECOMPOSITION - e.g. "decompose", "break into features", "create the feature list / plan", "order features and dependencies with coverage back to PRD/DESIGN". Kit preset that binds the DECOMPOSITION resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears DECOMPOSITION kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-decompose - DECOMPOSITION preset

Binds the gears DECOMPOSITION template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT DecompositionPreset
PURPOSE: Bind the gears DECOMPOSITION kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = DECOMPOSITION, GEARS_DOC_SKILL = cf-gears-decompose, GEARS_DOC_NEXT_SKILL = cf-gears-doc-feature
  SET GEARS_DOC_UPSTREAM = gears/<gear>/docs/DESIGN*.md (required)
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/DECOMPOSITION.md
  SET GEARS_DOC_TEMPLATE = {decomposition_template}, GEARS_DOC_RULES = {decomposition_rules}, GEARS_DOC_CHECKLIST = {decomposition_checklist}, GEARS_DOC_EXAMPLE = {decomposition_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every DECOMPOSITION resource before continuing into the router
  NEVER author, validate, or review DECOMPOSITION content in this preset
```
