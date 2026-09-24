---
cf-studio: true
type: workflow
name: cf-gears-implement
description: Invoke when the user asks to implement, build, or write the code for a Gears FEATURE - e.g. "implement", "write the code", "build this feature", "implement FEATURE with @cpt-* traceability". Kit preset that binds the gears code rules and drives the tests, author, validate, review, fix, and close stages through the Studio coding skills.
version: 2.0
purpose: Bind the gears code rules for feature-led work and route the requested stage through the gears code stage router.
---

# cf-gears-implement - FEATURE-led implementation with `@cpt-*` traceability

Binds the FEATURE source contract, the gears code rules, and the code review
checklist, then hands control to the shared gears code stage router
(`{gears_code_stage_router}`). The router supplies the phase prerequisites
from `{gears_code_phase}`, routes each stage to `cf-coding-tests`, `-gen`,
`-ci`, `-review`, or `-fix`, and pins this preset again with the next stage
until the slice's definition of done holds.

```pdsl
UNIT ImplementPreset
PURPOSE: Bind the gears code rules for feature-led work and route the requested stage through the gears code stage router.
DO:
  SET GEARS_CODE_SKILL = cf-gears-implement, GEARS_CODE_MODE = feature-led, GEARS_CODE_SOURCE_KIND = FEATURE
  SET GEARS_CODE_RULES = {codebase_rules}, GEARS_CODE_CHECKLIST = {codebase_checklist}
  LOAD {gears_code_stage_router}
  CONTINUE GearsCodeStageEntry
RULES:
  ALWAYS bind the rules and checklist before continuing into the router
  NEVER write tests or code in this preset
```
