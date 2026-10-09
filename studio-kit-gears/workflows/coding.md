---
cf-studio: true
type: workflow
name: cf-gears-coding
description: Invoke when the user asks to implement Gears code directly from DESIGN, ADR, PRD, or upstream design context without a FEATURE artifact or `@cpt-*` implementation traceability - e.g. "implement from the design", "code this without a FEATURE". Kit preset that binds the gears code rules and drives the tests, author, validate, review, fix, and close stages through the Studio coding skills.
version: 2.0
purpose: Bind the gears code rules for design-led work and route the requested stage through the gears code stage router.
---

# cf-gears-coding - DESIGN-led implementation without FEATURE traceability

Binds the DESIGN source contract, the gears code rules, and the code review
checklist, then hands control to the shared gears code stage router
(`{gears_code_stage_router}`). The router supplies the phase prerequisites
from `{gears_code_phase}`, routes each stage to `cf-coding-tests`, `-gen`,
`-ci`, `-review`, or `-fix`, and pins this preset again with the next stage
until the slice's definition of done holds.

```pdsl
UNIT CodingPreset
PURPOSE: Bind the gears code rules for design-led work and route the requested stage through the gears code stage router.
DO:
  SET GEARS_CODE_SKILL = cf-gears-coding, GEARS_CODE_MODE = design-led, GEARS_CODE_SOURCE_KIND = DESIGN, or ADR, PRD, or UPSTREAM_REQS when the request names that document as the design context
  SET GEARS_CODE_RULES = {codebase_design_led_rules}, GEARS_CODE_CHECKLIST = {codebase_checklist}
  LOAD {gears_code_stage_router}
  CONTINUE GearsCodeStageEntry
RULES:
  ALWAYS bind the rules and checklist before continuing into the router
  NEVER write tests or code in this preset
```
