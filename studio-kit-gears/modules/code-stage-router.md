# Gears Code Stage Router

Shared by `cf-gears-implement` (FEATURE-led, `@cpt-*` traceability) and
`cf-gears-coding` (DESIGN-led). A preset binds its source contract and rules
and continues into `GearsCodeStageEntry`; this module supplies the kit-owned
prerequisites, resolves the stage, routes it to the matching Studio coding
skill, and pins the preset again with the next stage. The stage table and the
definition of done live in `{gears_code_phase}`.

Preset inputs (set by the preset before `CONTINUE GearsCodeStageEntry`):
`GEARS_CODE_SKILL`, `GEARS_CODE_MODE` (feature-led | design-led),
`GEARS_CODE_SOURCE_KIND` (FEATURE | DESIGN | ADR | PRD | UPSTREAM_REQS), `GEARS_CODE_RULES`,
`GEARS_CODE_CHECKLIST`.

```pdsl
UNIT GearsCodeStageEntry
PURPOSE: Resolve the stage, gear, and source contract of this gears code run, then route it.
STATE:
  SET GEARS_STAGE: tests | author | validate | review | fix | close | unset (default unset, scope workflow_run)
  SET GEARS_GEAR: string | unset (default unset, scope workflow_run)
  SET GEARS_SOURCE_PATH: path | unset (default unset, scope workflow_run)
  SET GEARS_SLICE: string | unset (default unset, scope workflow_run)
  SET GEARS_SOURCE_CONTEXT: string | unset (default unset, scope workflow_run)
  SET GEARS_FORWARD_PAYLOAD: object | unset (default unset, scope workflow_run)
DO:
  RUN GearsCodeStageResolve
  RUN GearsCodeSourceResolve
  CONTINUE GearsCodeStageRoute
RULES:
  ALWAYS treat the preset's rules, checklist, and source contract as read-only preset data
  NEVER write tests, code, or review verdicts in this module; route every stage to a Studio coding skill or the kit close unit
```

```pdsl
UNIT GearsCodeStageResolve
PURPOSE: Pick the stage from the pinned handoff, the request wording, or the default.
DO:
  SET GEARS_STAGE, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT, and GEARS_SLICE from NEXT_ACTION_PAYLOAD WHEN NEXT_ACTION_PAYLOAD contains GEARS_STAGE
  EMIT "Ignoring the unknown stage '<GEARS_STAGE>' and its handoff payload; resolving the stage from the request instead." and SET GEARS_STAGE = unset and SET NEXT_ACTION_PAYLOAD = unset WHEN GEARS_STAGE is set and is not one of tests, author, validate, review, fix, close
  SET GEARS_FORWARD_PAYLOAD = NEXT_ACTION_PAYLOAD without the GEARS_* fields WHEN NEXT_ACTION_PAYLOAD is set
  SET GEARS_STAGE = author WHEN GEARS_STAGE == unset AND the request states that failing tests for the slice already exist
  EMIT "Review, fix, and close run from the router's own handoff; starting at validate so the gate evidence exists." and SET GEARS_STAGE = validate WHEN GEARS_STAGE == unset AND the request asks to review, fix findings, or close the slice
  SET GEARS_STAGE = validate WHEN GEARS_STAGE == unset AND the request explicitly asks to run the checks
  SET GEARS_STAGE = tests WHEN GEARS_STAGE == unset
RULES:
  ALWAYS drop a handoff payload whose stage is unknown before any field of it is forwarded
  NEVER enter review, fix, or close from request wording alone; those stages require the handoff payload of the stage before them
```

```pdsl
UNIT GearsCodeSourceResolve
PURPOSE: Resolve the gear and the source contract the code must realize.
DO:
  SET GEARS_GEAR = NEXT_ACTION_PAYLOAD.GEARS_GEAR and GEARS_SOURCE_PATH = NEXT_ACTION_PAYLOAD.GEARS_UPSTREAM_PATH WHEN GEARS_SOURCE_PATH == unset AND NEXT_ACTION_PAYLOAD contains GEARS_UPSTREAM_PATH, the document a gears document preset just closed
  SET GEARS_GEAR = the gear named in the request or implied by GEARS_SOURCE_PATH WHEN GEARS_GEAR == unset
  SET GEARS_SOURCE_PATH = the GEARS_CODE_SOURCE_KIND document named in the request, or the only such document under gears/<GEARS_GEAR>/docs/ when exactly one exists, WHEN GEARS_SOURCE_PATH == unset
  SET GEARS_SOURCE_PATH = inline and GEARS_SOURCE_CONTEXT = the design context quoted from the request WHEN GEARS_SOURCE_PATH == unset AND GEARS_CODE_MODE == design-led AND the request names no document but states the design context itself
  SET GEARS_SLICE = the source IDs the request scopes, or the first unimplemented slice of GEARS_SOURCE_PATH (the whole inline context when GEARS_SOURCE_PATH == inline), WHEN GEARS_SLICE == unset
  EMIT "Which <GEARS_CODE_SOURCE_KIND> should this implement? Reply with its path under gears/<gear>/docs/ (candidates: <every matching document>)." WHEN GEARS_SOURCE_PATH == unset
  STOP_TURN WHEN GEARS_SOURCE_PATH == unset
RULES:
  ALWAYS keep one run bound to one slice of one source contract
  NEVER pick a source document when more than one of GEARS_CODE_SOURCE_KIND matches and the request names none; ask instead
  NEVER accept inline design context in feature-led mode; a FEATURE document is the contract there
```

```pdsl
UNIT GearsCodeStageRoute
PURPOSE: Hand the stage to the Studio coding skill that owns it.
DO:
  RUN GearsCodeSupplyPhaseArtifacts WHEN GEARS_STAGE == tests OR GEARS_STAGE == author
  RUN GearsCodeDispatchContext WHEN GEARS_STAGE != close
  LOAD the Studio workflow coding-tests.md, coding-gen.md, coding-ci.md, coding-review.md, or coding-fix.md from {cf-studio-path}/.core/workflows/ WHEN GEARS_STAGE is tests, author, validate, review, or fix respectively
  RUN GearsCodePinNextStage WHEN GEARS_STAGE != close
  CONTINUE the entry unit of the loaded workflow (CodingTestsPreset, CodingGenBootstrap, CodingCiEntry, CodingReviewEntry, or CodingFixBootstrap) WHEN GEARS_STAGE != close
  CONTINUE GearsCodeClose WHEN GEARS_STAGE == close
RULES:
  ALWAYS forward GEARS_FORWARD_PAYLOAD unchanged as NEXT_ACTION_PAYLOAD to the loaded workflow, so review findings and approvals reach cf-coding-fix
  ALWAYS use `make gear-ci GEAR=<GEARS_GEAR>` and `make dylint`, plus `cfs validate` when GEARS_CODE_MODE == feature-led, as the remembered project gate commands for cf-coding-ci
  ALWAYS RUN GearsCodePinNextStage again immediately before the loaded workflow's NextActionsOffer, so the pin reflects the stage outcome and replaces any cf-coding-* pin it set
  ALWAYS end every routed stage through the loaded workflow's completion unit and its NextActionsOffer menu with the pinned GEARS_CODE_SKILL next stage marked (suggested); NEVER end a stage with free prose instead of that menu
```

```pdsl
UNIT GearsCodeDispatchContext
PURPOSE: Build the kit context block every sub-agent dispatch of this stage must carry.
DO:
  SET GEARS_DISPATCH_CONTEXT = "Gears kit context (read-only): implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH> (the quoted GEARS_SOURCE_CONTEXT when inline); slice <GEARS_SLICE>; gear gears/<GEARS_GEAR>/" WHEN GEARS_STAGE != review
  SET GEARS_DISPATCH_CONTEXT = "Gears kit review methodology (apply in addition to the Studio methodologies, and cite its item IDs in findings): <GEARS_CODE_CHECKLIST>; implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH> (the quoted GEARS_SOURCE_CONTEXT when inline); slice <GEARS_SLICE>; gear gears/<GEARS_GEAR>/; mode <GEARS_CODE_MODE>" WHEN GEARS_STAGE == review
  RUN `git status --porcelain -- <GEARS_SOURCE_PATH> gears/<GEARS_GEAR>/ Cargo.toml Cargo.lock apps/cf-gears-example-server/` (the slice, the gear, and the registration surface named in {gears_code_phase}) and SET GEARS_WORKTREE_DIRTY = true when it lists any path, else false
RULES:
  ALWAYS paste GEARS_DISPATCH_CONTEXT verbatim into the prompt of every sub-agent this stage dispatches (author, coder, test writer, reviewer, bug finder, fixer)
  NEVER dispatch a sub-agent in this stage without GEARS_DISPATCH_CONTEXT
  ALWAYS dispatch a main-context coder (cf-generate-coder-smart, or cf-generate-coder-casual for small slices) instead of the worktree-isolated cf-codegen WHEN GEARS_WORKTREE_DIRTY == true, because an isolated worktree only sees committed files
```

```pdsl
UNIT GearsCodeSupplyPhaseArtifacts
PURPOSE: Supply the kit-owned phase prerequisites that cf-coding-gen checks.
DO:
  SET AVAILABLE_ARTIFACTS = phase-plan (ref "{gears_code_phase}#phase-plan", shape phase-plan-doc), phase-dod (ref "{gears_code_phase}#definition-of-done", shape phase-dod-doc), acceptance-criteria (shape doc-bundle: the GEARS_SLICE IDs in GEARS_SOURCE_PATH and their definition-of-done items, or GEARS_SOURCE_CONTEXT when the source is inline), and relevant-files-map (ref "{gears_code_phase}#relevant-files" for gear GEARS_GEAR, shape path-map)
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
  SET GEARS_NEXT_STAGE = review WHEN GEARS_STAGE == validate AND GATE_STATUS == pass
  SET GEARS_NEXT_STAGE = author when GATE_STATUS == fail, else validate again (an unset or unknown gate status never advances) WHEN GEARS_STAGE == validate AND GATE_STATUS != pass
  SET GEARS_NEXT_STAGE = close WHEN GEARS_STAGE == review AND ReviewFindingsReport is set AND it has no CRITICAL or MAJOR finding (remaining MINOR findings go to the close report)
  SET GEARS_NEXT_STAGE = fix when ReviewFindingsReport has a CRITICAL or MAJOR finding, else review again (an unset report never closes) WHEN GEARS_STAGE == review AND (ReviewFindingsReport is unset OR it has a CRITICAL or MAJOR finding)
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE GEARS_NEXT_STAGE, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT when set, GEARS_SLICE, plus the loaded workflow's own handoff fields (FINDINGS, ReviewFindingsReport, APPROVED_REVIEW_FINDING_IDS, REVIEW_FIX_SCOPE, REVIEW_FIX_APPROVED, REVIEW_TARGET_PATHS, REVIEW_TARGET_SLICES, GATE_STATUS) when set
RULES:
  ALWAYS name the pinned action "<GEARS_CODE_SKILL> — <GEARS_NEXT_STAGE> <GEARS_GEAR> <GEARS_SLICE>" so the user sees the stage
  NEVER edit files in the validate stage; a failing gate pins the author stage with the CI findings instead of fixing them in place
```

```pdsl
UNIT GearsCodeClose
PURPOSE: Check the definition of done for the slice, report, and offer the next slice.
DO:
  RUN `make gear-ci GEAR=<GEARS_GEAR>` and `make dylint` (plus `cfs validate` when GEARS_CODE_MODE == feature-led), then check every item of "{gears_code_phase}#definition-of-done" against those results and the review findings in GEARS_FORWARD_PAYLOAD
  EMIT a SKILL_RESULT envelope with skill = GEARS_CODE_SKILL, status = completed when every definition-of-done item holds else failed, produced_artifacts = code-changes and unit-tests for GEARS_SLICE plus phase-status, report_outputs = the gate result and the security-boundary evidence recorded for the slice (or the statement that no guardrail was touched), missing_artifacts = every definition-of-done item that fails (failing gate commands, unresolved CRITICAL or MAJOR finding IDs, unmet traceability, missing security-boundary evidence) or [] when all hold, assumptions = any recorded overrides, and suggested_next_skills = [GEARS_CODE_SKILL, cf-git-commit]
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT when set, GEARS_SLICE, GATE_STATUS, plus the failing definition-of-done items WHEN a definition-of-done item fails, so the gate runs again and routes to author or fix from fresh evidence
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE tests, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT when set WHEN every definition-of-done item holds AND GEARS_SOURCE_PATH still has unimplemented slices
  SET NEXT_ACTION_PINNED_SKILL = cf-git-commit WHEN every definition-of-done item holds AND every slice of GEARS_SOURCE_PATH is implemented
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  ALWAYS list remaining MINOR review findings in the close report
  NEVER mark the slice done while a gate command fails or a CRITICAL or MAJOR finding is unresolved
```
