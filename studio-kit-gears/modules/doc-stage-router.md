# Gears Document Stage Router

Shared by every `cf-gears-doc-*` preset. A preset binds its KIND resources and
continues into `GearsDocStageEntry`; this module supplies the kit-owned
prerequisites, resolves the stage, routes the stage to the matching Studio thin
skill, and pins the preset again with the next stage. The stage table and the
definition of done live in `{gears_doc_phase}`.

Preset inputs (all set by the preset before `CONTINUE GearsDocStageEntry`):
`GEARS_DOC_KIND`, `GEARS_DOC_SKILL`, `GEARS_DOC_UPSTREAM` (entries tagged
required or optional), `GEARS_DOC_NEXT_SKILL`, `GEARS_DOC_PATH_RULE`,
`GEARS_DOC_TEMPLATE`, `GEARS_DOC_RULES`, `GEARS_DOC_CHECKLIST`,
`GEARS_DOC_EXAMPLE`, and optionally `GEARS_DOC_COMPANION` (a section of one
other artifact the author stage must update in the same change).

```pdsl
UNIT GearsDocStageEntry
PURPOSE: Resolve the stage and target of this gears document run, then route it.
STATE:
  SET GEARS_STAGE: author | validate | review | fix | close | unset (default unset, scope workflow_run)
  SET GEARS_TARGET_PATH: path | unset (default unset, scope workflow_run)
  SET GEARS_FORWARD_PAYLOAD: object | unset (default unset, scope workflow_run)
DO:
  RUN GearsDocStageResolve
  RUN GearsDocTargetResolve
  RUN GearsDocBindResources
  CONTINUE GearsDocUpstreamCheck WHEN GEARS_STAGE == author
  CONTINUE GearsDocStageRoute WHEN GEARS_STAGE != author
RULES:
  ALWAYS treat the preset's bound KIND resources as read-only preset data
  NEVER author, validate, or review content in this module; route every stage to a Studio thin skill or the kit close unit
```

```pdsl
UNIT GearsDocStageResolve
PURPOSE: Pick the stage from the pinned handoff, the request wording, or the default.
DO:
  SET GEARS_STAGE = NEXT_ACTION_PAYLOAD.GEARS_STAGE WHEN NEXT_ACTION_PAYLOAD contains GEARS_STAGE
  SET GEARS_TARGET_PATH = NEXT_ACTION_PAYLOAD.GEARS_TARGET_PATH WHEN NEXT_ACTION_PAYLOAD contains GEARS_TARGET_PATH
  SET GEARS_FORWARD_PAYLOAD = NEXT_ACTION_PAYLOAD without GEARS_STAGE and GEARS_TARGET_PATH WHEN NEXT_ACTION_PAYLOAD is set
  EMIT "Ignoring unknown stage '<GEARS_STAGE>' from the handoff; resolving the stage from the request instead." and SET GEARS_STAGE = unset WHEN GEARS_STAGE is set and is not one of author, validate, review, fix, close
  SET GEARS_STAGE = validate, review, fix, or close WHEN GEARS_STAGE == unset AND the request explicitly asks to validate or check, review, fix findings, or close the artifact
  SET GEARS_STAGE = author WHEN GEARS_STAGE == unset
```

```pdsl
UNIT GearsDocTargetResolve
PURPOSE: Resolve the single artifact path this run works on.
DO:
  SET GEARS_TARGET_PATH = the explicit document path in the request WHEN GEARS_TARGET_PATH == unset AND the request names one
  SET GEARS_TARGET_PATH = the path GEARS_DOC_PATH_RULE derives for the gear named in the request WHEN GEARS_TARGET_PATH == unset AND the request names a gear
  EMIT "Which gear is this <GEARS_DOC_KIND> for? Reply with the gear name (the directory under gears/) or the document path." WHEN GEARS_TARGET_PATH == unset
  STOP_TURN WHEN GEARS_TARGET_PATH == unset
RULES:
  ALWAYS keep one run bound to one artifact path; multi-artifact work belongs in cf-documenting-planning
```

```pdsl
UNIT GearsDocBindResources
PURPOSE: Normalize the gears KIND bindings into the fields the Studio document skills read.
DO:
  SET ARTIFACT_KIND = GEARS_DOC_KIND
  SET artifact_template = GEARS_DOC_TEMPLATE
  SET artifact_rules = GEARS_DOC_RULES
  SET artifact_checklist = GEARS_DOC_CHECKLIST
  SET artifact_example = GEARS_DOC_EXAMPLE WHEN GEARS_STAGE == review OR GEARS_STAGE == fix
  SET AUTHOR_TARGET_PATHS = [GEARS_TARGET_PATH] plus the file of GEARS_DOC_COMPANION when it is set and exists, and REVIEW_TARGET_PATHS = [GEARS_TARGET_PATH]
RULES:
  ALWAYS keep GEARS_DOC_CHECKLIST review-only for authoring; GEARS_DOC_RULES carries no pre-write checklist directive, so the author does not load it
  ALWAYS keep GEARS_DOC_EXAMPLE out of the author stage so generation follows the template, not the example
  ALWAYS limit edits to the GEARS_DOC_COMPANION file to the named section; every other part of that file stays untouched
```

```pdsl
UNIT GearsDocUpstreamCheck
PURPOSE: Require the upstream artifact of the gears chain before authoring.
DO:
  LOAD {cf-studio-path}/.core/skills/studio/modules/runtime/skill-io-contract-load.md
  RUN SkillIoContractLoad
  SET AVAILABLE_ARTIFACTS = one gears-upstream-doc descriptor per GEARS_DOC_UPSTREAM entry that resolves to existing files, resolved under the gear the entry names (the target gear unless the entry names another gear)
  SET REQUIRED_ARTIFACT_SPECS = one gears-upstream-doc spec per GEARS_DOC_UPSTREAM entry tagged required, with why_needed "The <GEARS_DOC_KIND> must trace to <entry>", accepted_shapes doc-ref, suggested_producers the kit preset that authors that entry's KIND, override_allowed true, override_summary "Proceed without the upstream artifact; traceability to it stays open"; [] when no entry is tagged required
  RUN PrerequisiteCheckContract
  CONTINUE GearsDocStageRoute WHEN PREREQUISITE_STATUS == ready OR OVERRIDE_REQUESTED == explicit-user-approval
RULES:
  ALWAYS declare gears-upstream-doc as a kit extension artifact type, not a canonical Studio artifact
  ALWAYS record an overridden upstream as an assumption with artifact_or_gate gears-upstream-doc
```

```pdsl
UNIT GearsDocStageRoute
PURPOSE: Hand the stage to the Studio thin skill that owns it.
DO:
  RUN GearsDocSupplyPhaseArtifacts WHEN GEARS_STAGE == author
  LOAD the Studio workflow documenting-gen.md, documenting-ci.md, documenting-review.md, or documenting-fix.md from {cf-studio-path}/.core/workflows/ WHEN GEARS_STAGE is author, validate, review, or fix respectively
  RUN GearsDocPinNextStage WHEN GEARS_STAGE != close
  RUN GearsDocAnnounceStage WHEN GEARS_STAGE != close
  CONTINUE the entry unit of the loaded workflow (DocumentingGenBootstrap, DocumentingCiPreset, DocumentingReviewPreset, or DocumentingFixBootstrap) WHEN GEARS_STAGE != close
  CONTINUE GearsDocClose WHEN GEARS_STAGE == close
RULES:
  ALWAYS forward GEARS_FORWARD_PAYLOAD unchanged as NEXT_ACTION_PAYLOAD to the loaded workflow, so review findings and approvals reach cf-documenting-fix
  ALWAYS RUN GearsDocPinNextStage again immediately before the loaded workflow's NextActionsOffer, so the pin reflects the stage outcome and replaces any cf-documenting-* pin it set
  NEVER override the loaded workflow's gates, verdicts, or STOP_TURN points
  ALWAYS end every routed stage through the loaded workflow's completion unit and its NextActionsOffer menu with the pinned GEARS_DOC_SKILL next stage marked (suggested); NEVER end a stage with free prose instead of that menu
```

```pdsl
UNIT GearsDocAnnounceStage
PURPOSE: Make the pinned next stage visible before the Studio skill takes over.
DO:
  EMIT "Stage <GEARS_STAGE> of <GEARS_DOC_SKILL> for <GEARS_TARGET_PATH>; this stage ends with a next-actions menu whose suggested action is <GEARS_DOC_SKILL> — <GEARS_NEXT_STAGE>."
```

```pdsl
UNIT GearsDocSupplyPhaseArtifacts
PURPOSE: Supply the kit-owned phase prerequisites that cf-documenting-gen checks.
DO:
  SET AVAILABLE_ARTIFACTS = phase-plan (ref "{gears_doc_phase}#phase-plan", shape phase-plan-doc), phase-dod (ref "{gears_doc_phase}#definition-of-done", shape phase-dod-doc), and acceptance-criteria (shape doc-bundle: the user's request, the GEARS_DOC_UPSTREAM files, and the Requirements section of GEARS_DOC_RULES)
  SET ORIGINAL_INTENT = the user's request plus "target: <GEARS_TARGET_PATH>" plus the CI findings in GEARS_FORWARD_PAYLOAD when this author run revises after a failed validate stage
RULES:
  ALWAYS present these as caller-supplied artifact descriptors so the gen prerequisite check reports ready without an override
  NEVER claim a phase-plan or phase-dod other than {gears_doc_phase}
```

```pdsl
UNIT GearsDocPinNextStage
PURPOSE: Pin the preset itself as the suggested next action with the next stage.
DO:
  SET GEARS_NEXT_STAGE = validate WHEN GEARS_STAGE == author
  SET GEARS_NEXT_STAGE = review WHEN GEARS_STAGE == validate AND GATE_STATUS == pass
  SET GEARS_NEXT_STAGE = author when GATE_STATUS == fail, else validate again (an unset or unknown gate status never advances) WHEN GEARS_STAGE == validate AND GATE_STATUS != pass
  SET GEARS_NEXT_STAGE = close WHEN GEARS_STAGE == review AND REVIEW_FINDINGS_REMAINING == 0
  SET GEARS_NEXT_STAGE = fix WHEN GEARS_STAGE == review AND REVIEW_FINDINGS_REMAINING != 0
  SET GEARS_NEXT_STAGE = review WHEN GEARS_STAGE == fix
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE GEARS_NEXT_STAGE, GEARS_TARGET_PATH, plus the loaded workflow's own handoff fields (FINDINGS, ReviewFindingsReport, APPROVED_REVIEW_FINDING_IDS, REVIEW_FIX_SCOPE, REVIEW_FIX_APPROVED, REVIEW_TARGET_PATHS, REVIEW_TARGET_SLICES, GATE_STATUS) when set
RULES:
  ALWAYS name the pinned action "<GEARS_DOC_SKILL> — <GEARS_NEXT_STAGE> <GEARS_TARGET_PATH>" so the user sees the stage
  NEVER edit files in the validate stage; a failing gate pins the author stage with the CI findings instead of fixing them in place
```

```pdsl
UNIT GearsDocClose
PURPOSE: Check the definition of done, report, and offer the next artifact of the chain.
DO:
  RUN `cfs validate-toc <GEARS_TARGET_PATH>` and `cfs validate --artifact <GEARS_TARGET_PATH>`
  RUN check every item of "{gears_doc_phase}#definition-of-done" against those results and the review findings in GEARS_FORWARD_PAYLOAD
  EMIT a SKILL_RESULT envelope with skill = GEARS_DOC_SKILL, status = completed when every definition-of-done item holds else failed, produced_artifacts = doc-changes for GEARS_TARGET_PATH plus phase-status, report_outputs = the validation result, missing_artifacts = every definition-of-done item that fails (validation errors, unresolved CRITICAL or MAJOR finding IDs, uncovered upstream IDs) or [] when all hold, assumptions = any recorded overrides, and suggested_next_skills = [GEARS_DOC_NEXT_SKILL]
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_NEXT_SKILL WHEN every definition-of-done item holds
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE author, GEARS_TARGET_PATH WHEN a definition-of-done item fails
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  ALWAYS list remaining MINOR review findings in the close report
  NEVER mark the phase done while `cfs validate --artifact` reports errors beyond the exception the phase definition of done allows, or while a CRITICAL or MAJOR finding is unresolved
```
