# Gears Code Stage Router

Shared by `cf-gears-implement` (FEATURE-led, `@cpt-*` traceability) and
`cf-gears-coding` (DESIGN-led). A preset binds its source contract and rules
and continues into `GearsCodeStageEntry`; this module supplies the kit-owned
prerequisites, resolves the stage, routes it to the matching Studio coding
skill, and pins the preset again with the next stage. The stage table and the
definition of done live in `{gears_code_phase}`.

Preset inputs (set by the preset before `CONTINUE GearsCodeStageEntry`):
`GEARS_CODE_SKILL`, `GEARS_CODE_MODE` (feature-led | design-led),
`GEARS_CODE_SOURCE_KIND` (FEATURE | DESIGN), `GEARS_CODE_RULES`,
`GEARS_CODE_CHECKLIST`.

```pdsl
UNIT GearsCodeStageEntry
PURPOSE: Resolve the stage, gear, and source contract of this gears code run, then route it.
STATE:
  SET GEARS_STAGE: tests | author | validate | review | fix | close | unset (default unset, scope workflow_run)
  SET GEARS_GEAR: string | unset (default unset, scope workflow_run)
  SET GEARS_SOURCE_PATH: path | unset (default unset, scope workflow_run)
  SET GEARS_SLICE: string | unset (default unset, scope workflow_run)
  SET GEARS_FORWARD_PAYLOAD: object | unset (default unset, scope workflow_run)
DO:
  RUN GearsCodeStageResolve
  RUN GearsCodeSourceResolve
  CONTINUE GearsCodeStageRoute
RULES:
  ALWAYS treat the preset's rules, checklist, and source contract as read-only preset data
  NEVER write tests, code, or review verdicts in this module; route every stage to a Studio coding skill or the kit close unit
  ALWAYS dispatch a main-context coder (cf-generate-coder-smart, or cf-generate-coder-casual for small slices) instead of the worktree-isolated cf-codegen while GEARS_SOURCE_PATH, its upstream docs, or files from earlier stages are uncommitted, because an isolated worktree only sees committed files
```

```pdsl
UNIT GearsCodeStageResolve
PURPOSE: Pick the stage from the pinned handoff, the request wording, or the default.
DO:
  SET GEARS_STAGE, GEARS_GEAR, GEARS_SOURCE_PATH, and GEARS_SLICE from NEXT_ACTION_PAYLOAD WHEN NEXT_ACTION_PAYLOAD contains GEARS_STAGE
  SET GEARS_FORWARD_PAYLOAD = NEXT_ACTION_PAYLOAD without the GEARS_* fields WHEN NEXT_ACTION_PAYLOAD is set
  SET GEARS_STAGE = author, validate, review, fix, or close WHEN GEARS_STAGE == unset AND the request explicitly asks to implement without new tests, run the checks, review, fix findings, or close the slice
  SET GEARS_STAGE = tests WHEN GEARS_STAGE == unset
```

```pdsl
UNIT GearsCodeSourceResolve
PURPOSE: Resolve the gear and the source contract the code must realize.
DO:
  SET GEARS_GEAR = the gear named in the request or implied by GEARS_SOURCE_PATH WHEN GEARS_GEAR == unset
  SET GEARS_SOURCE_PATH = the GEARS_CODE_SOURCE_KIND document named in the request, or the single one under gears/<GEARS_GEAR>/docs/, WHEN GEARS_SOURCE_PATH == unset
  SET GEARS_SLICE = the source IDs the request scopes, or the first unimplemented slice of GEARS_SOURCE_PATH, WHEN GEARS_SLICE == unset
  EMIT "Which <GEARS_CODE_SOURCE_KIND> should this implement? Reply with its path under gears/<gear>/docs/." WHEN GEARS_SOURCE_PATH == unset
  STOP_TURN WHEN GEARS_SOURCE_PATH == unset
RULES:
  ALWAYS keep one run bound to one slice of one source contract
```

```pdsl
UNIT GearsCodeStageRoute
PURPOSE: Hand the stage to the Studio coding skill that owns it.
DO:
  RUN GearsCodeSupplyPhaseArtifacts WHEN GEARS_STAGE == tests OR GEARS_STAGE == author
  RUN GearsCodeDispatchContext WHEN GEARS_STAGE != close
  LOAD {cf-studio-path}/.core/workflows/coding-tests.md WHEN GEARS_STAGE == tests
  LOAD {cf-studio-path}/.core/workflows/coding-gen.md WHEN GEARS_STAGE == author
  LOAD the Studio workflow coding-ci.md, coding-review.md, or coding-fix.md from {cf-studio-path}/.core/workflows/ WHEN GEARS_STAGE is validate, review, or fix respectively
  CONTINUE the entry unit of the loaded workflow (CodingTestsPreset, CodingGenBootstrap, CodingCiEntry, CodingReviewEntry, or CodingFixBootstrap) WHEN GEARS_STAGE != close
  CONTINUE GearsCodeClose WHEN GEARS_STAGE == close
RULES:
  ALWAYS forward GEARS_FORWARD_PAYLOAD unchanged as NEXT_ACTION_PAYLOAD to the loaded workflow, so review findings and approvals reach cf-coding-fix
  ALWAYS use `make gear-ci GEAR=<GEARS_GEAR>` and `make dylint`, plus `cfs validate` when GEARS_CODE_MODE == feature-led, as the remembered project gate commands for cf-coding-ci
  ALWAYS RUN GearsCodePinNextStage immediately before the loaded workflow's NextActionsOffer, replacing any cf-coding-* pin it set
  ALWAYS end every routed stage through the loaded workflow's completion unit and its NextActionsOffer menu with the pinned GEARS_CODE_SKILL next stage marked (suggested); NEVER end a stage with free prose instead of that menu
```

```pdsl
UNIT GearsCodeDispatchContext
PURPOSE: Build the kit context block every sub-agent dispatch of this stage must carry.
DO:
  SET GEARS_DISPATCH_CONTEXT = "Gears kit context (read-only): implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH>; slice <GEARS_SLICE>; gear gears/<GEARS_GEAR>/" WHEN GEARS_STAGE != review
  SET GEARS_DISPATCH_CONTEXT = "Gears kit review methodology (apply in addition to the Studio methodologies, and cite its item IDs in findings): <GEARS_CODE_CHECKLIST>; implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH>" WHEN GEARS_STAGE == review
RULES:
  ALWAYS paste GEARS_DISPATCH_CONTEXT verbatim into the prompt of every sub-agent this stage dispatches (author, coder, test writer, reviewer, bug finder, fixer)
  NEVER dispatch a sub-agent in this stage without GEARS_DISPATCH_CONTEXT
```

```pdsl
UNIT GearsCodeSupplyPhaseArtifacts
PURPOSE: Supply the kit-owned phase prerequisites that cf-coding-gen checks.
DO:
  SET AVAILABLE_ARTIFACTS = phase-plan (ref "{gears_code_phase}#phase-plan", shape phase-plan-doc), phase-dod (ref "{gears_code_phase}#definition-of-done", shape phase-dod-doc), acceptance-criteria (shape doc-bundle: the GEARS_SLICE IDs in GEARS_SOURCE_PATH and their definition-of-done items), and relevant-files-map (ref "{gears_code_phase}#relevant-files" for gear GEARS_GEAR, shape path-map)
  SET ORIGINAL_INTENT = the user's request plus "gear: <GEARS_GEAR>; source: <GEARS_SOURCE_PATH>; slice: <GEARS_SLICE>" plus the CI findings in GEARS_FORWARD_PAYLOAD when this run revises after a failed validate stage
RULES:
  ALWAYS present these as caller-supplied artifact descriptors so the gen prerequisite check reports ready without an override
  ALWAYS treat the tests written in the tests stage as the slice's test artifacts in the author stage
```

```pdsl
UNIT GearsCodePinNextStage
PURPOSE: Pin the preset itself as the suggested next action with the next stage.
DO:
  SET GEARS_NEXT_STAGE = author WHEN GEARS_STAGE == tests
  SET GEARS_NEXT_STAGE = validate WHEN GEARS_STAGE == author OR GEARS_STAGE == fix
  SET GEARS_NEXT_STAGE = review WHEN GEARS_STAGE == validate AND GATE_STATUS != fail
  SET GEARS_NEXT_STAGE = author WHEN GEARS_STAGE == validate AND GATE_STATUS == fail
  SET GEARS_NEXT_STAGE = close WHEN GEARS_STAGE == review AND REVIEW_FINDINGS_REMAINING == 0
  SET GEARS_NEXT_STAGE = fix WHEN GEARS_STAGE == review AND REVIEW_FINDINGS_REMAINING != 0
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE GEARS_NEXT_STAGE, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SLICE, plus the loaded workflow's own handoff fields (FINDINGS, ReviewFindingsReport, APPROVED_REVIEW_FINDING_IDS, REVIEW_FIX_SCOPE, REVIEW_FIX_APPROVED, REVIEW_TARGET_PATHS, REVIEW_TARGET_SLICES, GATE_STATUS) when set
RULES:
  ALWAYS name the pinned action "<GEARS_CODE_SKILL> — <GEARS_NEXT_STAGE> <GEARS_GEAR> <GEARS_SLICE>" so the user sees the stage
  NEVER edit files in the validate stage; a failing gate pins the author stage with the CI findings instead of fixing them in place
```

```pdsl
UNIT GearsCodeClose
PURPOSE: Check the definition of done for the slice, report, and offer the next slice.
DO:
  RUN `make gear-ci GEAR=<GEARS_GEAR>` and `make dylint`, plus `cfs validate` WHEN GEARS_CODE_MODE == feature-led
  RUN check every item of "{gears_code_phase}#definition-of-done" against those results and the review findings in GEARS_FORWARD_PAYLOAD
  EMIT a SKILL_RESULT envelope with skill = GEARS_CODE_SKILL, status = completed when every definition-of-done item holds else failed, produced_artifacts = code-changes and unit-tests for GEARS_SLICE plus phase-status, report_outputs = the gate result, missing_artifacts = [], assumptions = any recorded overrides, and suggested_next_skills = [GEARS_CODE_SKILL, cf-git-commit]
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE tests, GEARS_GEAR, GEARS_SOURCE_PATH WHEN GEARS_SOURCE_PATH still has unimplemented slices
  SET NEXT_ACTION_PINNED_SKILL = cf-git-commit WHEN every slice of GEARS_SOURCE_PATH is implemented
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  ALWAYS list remaining MINOR review findings in the close report
  NEVER mark the slice done while a gate command fails or a CRITICAL or MAJOR finding is unresolved
```
