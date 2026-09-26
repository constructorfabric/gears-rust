---
cf-studio: true
type: workflow
name: cf-gears-doc-feature
description: Invoke when the user asks to author, write, revise, validate, review, fix, or spec a Gears FEATURE - e.g. "generate FEATURE", "spec the feature", "define flows / algorithms / states / definition of done (CDSL)", "write test scenarios for a feature". Kit preset that binds the FEATURE resources and drives the author, validate, review, fix, and close stages through the Studio document skills.
version: 2.0
purpose: Bind the gears FEATURE kit resources and route the requested stage through the gears document stage router.
---

# cf-gears-doc-feature - FEATURE preset

Binds the gears FEATURE template, rules, checklist, and example, then hands
control to the shared gears document stage router (`{gears_doc_stage_router}`).
The router supplies the phase prerequisites from `{gears_doc_phase}`, routes
each stage to `cf-documenting-gen`, `-ci`, `-review`, or `-fix`, and pins this
preset again with the next stage until the definition of done holds.

```pdsl
UNIT FeaturePreset
PURPOSE: Bind the gears FEATURE kit resources and route the requested stage through the gears document stage router.
DO:
  SET GEARS_DOC_KIND = FEATURE, GEARS_DOC_SKILL = cf-gears-doc-feature, GEARS_DOC_NEXT_SKILL = cf-gears-implement
  SET GEARS_DOC_UPSTREAM = gears/<gear>/docs/DECOMPOSITION.md (required; the feature's entry in it is checked by the FEATURE rules)
  SET GEARS_DOC_PATH_RULE = gears/<gear>/docs/features/<file>.md, following the naming already used in that directory and defaulting to <NNNN>-<slug>.md
  SET GEARS_DOC_TEMPLATE = {feature_template}, GEARS_DOC_RULES = {feature_rules}, GEARS_DOC_CHECKLIST = {feature_checklist}, GEARS_DOC_EXAMPLE = {feature_example}
  LOAD {gears_doc_stage_router}
  CONTINUE GearsDocStageEntry
RULES:
  ALWAYS bind every FEATURE resource before continuing into the router
  NEVER author, validate, or review FEATURE content in this preset
```
