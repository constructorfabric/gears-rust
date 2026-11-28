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

One DESIGN-only addition to the otherwise kind-agnostic close stage:
`GearsDocDesignAmendImpactCheck` runs `cf-gears-change-impact-analysis`
automatically whenever a DESIGN with an already-decomposed/already-implemented
downstream (a DECOMPOSITION plus at least one closed FEATURE) closes again, so
a mid-chain DESIGN amendment surfaces its downstream impact right away — non-
blockingly, same as every other use of that workflow — instead of only at
`GearsCodeChainSweep`'s end-of-chain re-check (`modules/code-stage-router.md`),
which remains the actual blocking backstop for whatever this earlier, advisory
signal was missed or ignored.

```pdsl
UNIT GearsDocStageEntry
PURPOSE: Resolve the stage and target of this gears document run, then route it.
STATE:
  SET GEARS_STAGE: author | validate | review | fix | close | unset (default unset, scope workflow_run)
  SET GEARS_TARGET_PATH: path | unset (default unset, scope workflow_run)
  SET GEARS_GEAR: string | unset (default unset, scope workflow_run)
  SET GEARS_UPSTREAM_FILES: path-list | unset (default unset, scope workflow_run)
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
  SET GEARS_TARGET_PATH and GEARS_GEAR from NEXT_ACTION_PAYLOAD, each WHEN NEXT_ACTION_PAYLOAD contains it
  EMIT "Ignoring the unknown stage '<GEARS_STAGE>' and its handoff payload; resolving the stage from the request instead." and SET GEARS_STAGE = unset and SET NEXT_ACTION_PAYLOAD = unset WHEN GEARS_STAGE is set and is not one of author, validate, review, fix, close
  SET GEARS_FORWARD_PAYLOAD = NEXT_ACTION_PAYLOAD without the GEARS_* fields WHEN NEXT_ACTION_PAYLOAD is set
  EMIT "Review, fix, and close run from the router's own handoff; starting at validate so the gate evidence exists." and SET GEARS_STAGE = validate WHEN GEARS_STAGE == unset AND the request asks to review, fix findings, or close the artifact
  SET GEARS_STAGE = validate WHEN GEARS_STAGE == unset AND the request explicitly asks to validate or check the artifact
  SET GEARS_STAGE = author WHEN GEARS_STAGE == unset
RULES:
  ALWAYS drop a handoff payload whose stage is unknown before any field of it is forwarded
  NEVER enter review, fix, or close from request wording alone; those stages require the handoff payload of the stage before them
```

```pdsl
UNIT GearsDocTargetResolve
PURPOSE: Resolve the single artifact path this run works on.
STATE:
  SET GEARS_CATALOG_MATCH: string | unset (default unset, scope workflow_run)
DO:
  SET GEARS_TARGET_PATH = the explicit document path in the request WHEN GEARS_TARGET_PATH == unset AND the request names one
  SET GEARS_TARGET_PATH = the path GEARS_DOC_PATH_RULE derives for the gear named in the request WHEN GEARS_TARGET_PATH == unset AND the request names a gear
  SET GEARS_TARGET_PATH = the path GEARS_DOC_PATH_RULE derives for GEARS_GEAR, the gear the previous preset handed off, WHEN GEARS_TARGET_PATH == unset AND GEARS_GEAR is set
  RUN GearsDocCatalogCheck WHEN GEARS_TARGET_PATH == unset AND (GEARS_DOC_KIND == UPSTREAM_REQS OR GEARS_DOC_KIND == PRD)
  EMIT "Which gear is this <GEARS_DOC_KIND> for? Reply with the gear name (the directory under gears/) or the document path.", plus ` {gears_md} already catalogs this need as "<GEARS_CATALOG_MATCH's heading>" (<its scenario markers>) — if that's what you mean, reply with a directory name for it (e.g. a kebab-case slug of the heading); otherwise name a different gear.` WHEN GEARS_CATALOG_MATCH is set, WHEN GEARS_TARGET_PATH == unset
  STOP_TURN WHEN GEARS_TARGET_PATH == unset
  SET GEARS_GEAR = the gear directory that contains GEARS_TARGET_PATH
RULES:
  ALWAYS keep one run bound to one artifact path; multi-artifact work belongs in cf-documenting-planning
  ALWAYS run GearsDocCatalogCheck for UPSTREAM_REQS/PRD before concluding no gear was named and asking the user to invent one — a wrong "this must be new" assumption here compounds into a duplicate gear existing alongside {gears_md}'s own already-planned entry for the same need (confirmed live: a user asked for "email storage", {gears_md} already has an "Emails Storage" catalog entry, and nothing had consulted it before this point)
```

```pdsl
UNIT GearsDocCatalogCheck
PURPOSE: Before treating an unnamed gear as brand-new, check whether {gears_md}'s own catalog already names this need under a different heading.
DO:
  LOAD {gears_md}
  RUN match the request's own described need against {gears_md}'s catalog entries (section headings and their scenario bullets), by meaning, not literal gear-directory-name substring
  SET GEARS_CATALOG_MATCH = the matched section's heading plus its implemented/planned (`[x]`/`[ ]`) scenario markers WHEN a plausible match is found, else unset
RULES:
  NEVER block or add a turn on its own; this only sets state that GearsDocTargetResolve's existing question incorporates
  NEVER treat an unrelated or weakly-related catalog section as a match; a false "this already exists" suggestion is its own kind of harm — leave GEARS_CATALOG_MATCH unset when genuinely unsure
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
  SET AUTHOR_TARGET_PATHS and REVIEW_TARGET_PATHS = [GEARS_TARGET_PATH] plus the file of GEARS_DOC_COMPANION when it is set and exists
  SET GEARS_UPSTREAM_FILES = every file matched by each GEARS_DOC_UPSTREAM entry, resolved under the gear the entry names (the target gear unless the entry names another gear); an entry marked accepted matches only ADR files whose status (frontmatter `status:` or a `**Status**:` line) is accepted, case-insensitively
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
  EMIT "<GEARS_DOC_KIND> needs at least one upstream source and the request names none: <GEARS_DOC_UPSTREAM>. Name it and run again." and STOP_TURN WHEN a GEARS_DOC_UPSTREAM entry is tagged required and at least one, and the request resolves it to no entries
  SET AVAILABLE_ARTIFACTS = one gears-upstream-doc descriptor per file in GEARS_UPSTREAM_FILES; every one is loaded as upstream context, and a required entry is satisfied when at least one of its files is in GEARS_UPSTREAM_FILES
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
  RUN GearsDocDispatchContext WHEN GEARS_STAGE != close
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
UNIT GearsDocDispatchContext
PURPOSE: Give every stage the upstream documents the authoring stage resolved, so review and fix check traceability against the same sources.
DO:
  SET GEARS_DISPATCH_CONTEXT = "Gears kit context (read-only): <GEARS_DOC_KIND> rules <GEARS_DOC_RULES>; target <GEARS_TARGET_PATH>; upstream documents to trace against <GEARS_UPSTREAM_FILES>"
RULES:
  ALWAYS paste GEARS_DISPATCH_CONTEXT verbatim into the prompt of every sub-agent this stage dispatches (author, reviewer, fixer)
  NEVER add an upstream document to REVIEW_TARGET_PATHS; upstream documents are read as context, not reviewed
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
  SET GEARS_NEXT_STAGE = close WHEN GEARS_STAGE == review AND ReviewFindingsReport is set AND it has no CRITICAL or MAJOR finding (remaining MINOR findings go to the close report)
  SET GEARS_NEXT_STAGE = fix when ReviewFindingsReport has a CRITICAL or MAJOR finding, else review again (an unset report never closes) WHEN GEARS_STAGE == review AND (ReviewFindingsReport is unset OR it has a CRITICAL or MAJOR finding)
  SET GEARS_NEXT_STAGE = review WHEN GEARS_STAGE == fix
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE GEARS_NEXT_STAGE, GEARS_TARGET_PATH, GEARS_GEAR, plus the loaded workflow's own handoff fields (FINDINGS, ReviewFindingsReport, APPROVED_REVIEW_FINDING_IDS, REVIEW_FIX_SCOPE, REVIEW_FIX_APPROVED, REVIEW_TARGET_PATHS, REVIEW_TARGET_SLICES, GATE_STATUS) when set
RULES:
  ALWAYS name the pinned action "<GEARS_DOC_SKILL> — <GEARS_NEXT_STAGE> <GEARS_TARGET_PATH>" so the user sees the stage
  NEVER edit files in the validate stage; a failing gate pins the author stage with the CI findings instead of fixing them in place
```

```pdsl
UNIT GearsDocDesignAmendImpactCheck
PURPOSE: When a DESIGN this gear already decomposed/implemented downstream is closing again (an amendment, not its first authoring), surface its downstream impact immediately instead of only at GearsCodeChainSweep's end-of-chain re-check.
STATE:
  SET GEARS_DESIGN_HAS_REALIZED_DOWNSTREAM: boolean (default false, scope workflow_run)
DO:
  RUN check whether gears/<GEARS_GEAR>/docs/DECOMPOSITION.md exists AND at least one file under gears/<GEARS_GEAR>/docs/features/ (per GEARS_DOC_PATH_RULE for FEATURE, `workflows/doc-feature.md` — not a `FEATURE*.md` glob at the `docs/` level, which no real gear uses) has its `featstatus-*-implemented` checkbox checked, and SET GEARS_DESIGN_HAS_REALIZED_DOWNSTREAM accordingly
  LOAD {workflow_change_impact_analysis}
  RUN ChangeImpactAnalysis with MODE=cascade-tracking, UPSTREAM_ARTIFACT_TYPE=DESIGN, UPSTREAM_ARTIFACT_ID=GEARS_TARGET_PATH, BASELINE_REF=HEAD (the last committed revision of GEARS_TARGET_PATH, i.e. before this close's own uncommitted edits), CURRENT_REF=the working tree now, WHEN GEARS_DESIGN_HAS_REALIZED_DOWNSTREAM
  EMIT the resulting report path and its findings summary prominently in this close's own report, labeled "Downstream impact of this DESIGN amendment", naming every affected FEATURE explicitly (not just a count), WHEN GEARS_DESIGN_HAS_REALIZED_DOWNSTREAM
RULES:
  ALWAYS run this only WHEN GEARS_DOC_KIND == DESIGN; every other artifact kind skips this unit entirely
  NEVER run ChangeImpactAnalysis here when GEARS_DESIGN_HAS_REALIZED_DOWNSTREAM is false — a DESIGN with no DECOMPOSITION/closed FEATURE yet has nothing downstream to impact
  NEVER block this DESIGN's own close on the impact-analysis result — ChangeImpactAnalysis's own contract is advisory-only ("NEVER block a merge from this workflow"), and this check inherits that; a downstream-impact finding is a strongly-surfaced recommendation, never an added DOD gate
  ALWAYS run this before GearsDocClose's own DOD check so its findings land in the same close report, not a separate one
  ALWAYS keep every affected FEATURE named in this unit's EMIT part of "current context" for GearsDocClose's own NextActionsOffer call, so re-validating that FEATURE is naturally synthesized as one of its 3-5 candidate actions — never invent a separate next-action field; NextActionsOffer has exactly one pinned slot and synthesizes the rest from context
```

```pdsl
UNIT GearsDocClose
PURPOSE: Check the definition of done, report, and offer the next artifact of the chain.
DO:
  RUN GearsDocDesignAmendImpactCheck WHEN GEARS_DOC_KIND == DESIGN
  RUN `cfs validate-toc <GEARS_TARGET_PATH>` and `cfs validate --artifact <GEARS_TARGET_PATH>` and keep the result as ci-findings
  LOAD {cf-studio-path}/.core/skills/studio/modules/runtime/artifact-dod-check.md
  SET DOD_INPUT_ITEMS = the items of "{gears_doc_phase}#definition-of-done", each normalized to item_id, summary, and required; for the upstream-ID-coverage item, set its coverage_hints to the cross-reference/coverage findings in the ci-findings result above (`cfs validate --artifact` already checks `covered_by` completeness and that every ID reference resolves), so a genuinely-covered artifact has something to match against instead of being marked unclear for lack of a hint
  SET TARGET_ARTIFACTS = doc-changes (ref GEARS_TARGET_PATH), ci-findings (ref the validation result above), and review-findings (ref GEARS_FORWARD_PAYLOAD)
  RUN ArtifactDodCheckContract
  EMIT a SKILL_RESULT envelope with skill = GEARS_DOC_SKILL, status = completed WHEN ARTIFACT_DOD_STATUS == ready else failed, produced_artifacts = doc-changes for GEARS_TARGET_PATH plus phase-status, report_outputs = ARTIFACT_DOD_COVERAGE, missing_artifacts = the ARTIFACT_DOD_COVERAGE entries with coverage_status uncovered or unclear, or [] WHEN ARTIFACT_DOD_STATUS == ready, assumptions = any recorded overrides, and suggested_next_skills = [GEARS_DOC_NEXT_SKILL]
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_NEXT_SKILL and NEXT_ACTION_PAYLOAD = GEARS_GEAR and GEARS_UPSTREAM_PATH = GEARS_TARGET_PATH, with no GEARS_STAGE or GEARS_TARGET_PATH, so the next preset starts authoring its own document for the same gear, WHEN ARTIFACT_DOD_STATUS == ready
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_TARGET_PATH, GEARS_GEAR, plus the uncovered or unclear ARTIFACT_DOD_COVERAGE entries WHEN ARTIFACT_DOD_STATUS == blocked, so the gate runs again and routes to author or fix from fresh evidence
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  ALWAYS list remaining MINOR review findings in the close report
  NEVER mark the phase done while `cfs validate --artifact` reports errors beyond the exception the phase definition of done allows, or while a CRITICAL or MAJOR finding is unresolved
```
