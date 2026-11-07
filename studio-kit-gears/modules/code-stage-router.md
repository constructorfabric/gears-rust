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
  RUN GearsCiFacadeSanityCheck WHEN GEARS_STAGE == validate
  LOAD the Studio workflow coding-tests.md, coding-gen.md, coding-ci.md, coding-review.md, or coding-fix.md from {cf-studio-path}/.core/workflows/ WHEN GEARS_STAGE is tests, author, validate, review, or fix respectively
  RUN GearsCodePinNextStage WHEN GEARS_STAGE != close
  CONTINUE the entry unit of the loaded workflow (CodingTestsPreset, CodingGenBootstrap, CodingCiEntry, CodingReviewEntry, or CodingFixBootstrap) WHEN GEARS_STAGE != close
  CONTINUE GearsCodeClose WHEN GEARS_STAGE == close
RULES:
  ALWAYS forward GEARS_FORWARD_PAYLOAD unchanged as NEXT_ACTION_PAYLOAD to the loaded workflow, so review findings and approvals reach cf-coding-fix
  ALWAYS use {ci_commands}'s `gear_ci` and `dylint` commands (substituting GEARS_GEAR for `<GEAR>`), plus `cfs validate` when GEARS_CODE_MODE == feature-led, plus whatever {testing_policy}'s routing table maps to GEARS_DEPS_CHANGED, GEARS_REST_CHANGED, GEARS_GTS_TOUCHED, GEARS_LIBS_FIPS_TOUCHED, and GEARS_GEAR (each condition's own command also comes from {ci_commands}, never hardcoded here), as the remembered project gate commands for cf-coding-ci — NEVER run a routed check whose triggering condition is false, NEVER substitute the routed checks for `gear_ci`/`dylint`, and NEVER run a command {ci_commands} maps to an empty string (that means this repo has no equivalent, skip it rather than erroring)
  ALWAYS RUN GearsCodePinNextStage again immediately before the loaded workflow's NextActionsOffer, so the pin reflects the stage outcome and replaces any cf-coding-* pin it set
  ALWAYS end every routed stage through the loaded workflow's completion unit and its NextActionsOffer menu with the pinned GEARS_CODE_SKILL next stage marked (suggested); NEVER end a stage with free prose instead of that menu
```

```pdsl
UNIT GearsCodeDispatchContext
PURPOSE: Build the kit context block every sub-agent dispatch of this stage must carry.
DO:
  SET GEARS_DISPATCH_CONTEXT = "Gears kit context (read-only): implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH> (the quoted GEARS_SOURCE_CONTEXT when inline); slice <GEARS_SLICE>; gear gears/<GEARS_GEAR>/" WHEN GEARS_STAGE != review
  SET GEARS_DISPATCH_CONTEXT = "Gears kit review methodology (apply in addition to the Studio methodologies, and cite its item IDs in findings): <GEARS_CODE_CHECKLIST>; implementation rules <GEARS_CODE_RULES>; source contract <GEARS_SOURCE_PATH> (the quoted GEARS_SOURCE_CONTEXT when inline); slice <GEARS_SLICE>; gear gears/<GEARS_GEAR>/; mode <GEARS_CODE_MODE>" WHEN GEARS_STAGE == review
  LOAD {project_paths}
  RUN `git status --porcelain -- <GEARS_SOURCE_PATH> gears/<GEARS_GEAR>/ Cargo.toml Cargo.lock deny.toml <example_server_path from {project_paths}, omitted from the path list entirely when empty> <each glob in {project_paths}'s fips_lib_paths, omitted entirely when the list is empty>` (the slice, the gear, the registration surface, and the FIPS-relevant libs {testing_policy}'s routing table keys on — both path facts sourced from {project_paths}, never hardcoded here) and SET GEARS_WORKTREE_DIRTY = true when it lists any path, else false
  SET GEARS_DEPS_CHANGED = true WHEN that status output lists `Cargo.toml`, `Cargo.lock`, or `deny.toml`, else false — {testing_policy}'s `make deny` row gates on all three, not just the lockfile pair
  SET GEARS_REST_CHANGED = true WHEN that status output lists a path under `gears/<GEARS_GEAR>/**/api/rest/` or `gears/<GEARS_GEAR>/**/src/api/`, else false — a narrower proxy for {testing_policy}'s broader "any `.rs` file when the gear exposes REST" condition, by design (see that table's own note)
  SET GEARS_GTS_TOUCHED = true WHEN the slice touches GTS identifiers, type schemas, well-known instances, discriminator/const-enum-like values, or `x-gts-traits`/`x-gts-traits-schema` (the same condition the `{guidelines_gts}` review bullet already names), else false
  SET GEARS_LIBS_FIPS_TOUCHED = true WHEN {project_paths}'s fips_lib_paths is non-empty AND that status output lists a path under any of its globs, else false — always false when fips_lib_paths is an empty list, never an error
RULES:
  ALWAYS paste GEARS_DISPATCH_CONTEXT verbatim into the prompt of every sub-agent this stage dispatches (author, coder, test writer, reviewer, bug finder, fixer)
  NEVER dispatch a sub-agent in this stage without GEARS_DISPATCH_CONTEXT
  ALWAYS dispatch a main-context coder (cf-generate-coder-smart, or cf-generate-coder-casual for small slices) instead of the worktree-isolated cf-codegen WHEN GEARS_WORKTREE_DIRTY == true, because an isolated worktree only sees committed files
  ALWAYS derive GEARS_DEPS_CHANGED, GEARS_REST_CHANGED, GEARS_GTS_TOUCHED, and GEARS_LIBS_FIPS_TOUCHED from the same git status call as GEARS_WORKTREE_DIRTY, never a second scan
  ALWAYS read example_server_path and fips_lib_paths from {project_paths}, never hardcode a literal path here — a standalone install repoints these without editing this file
```

```pdsl
UNIT GearsCiFacadeSanityCheck
PURPOSE: Catch a never-configured {ci_commands}/{project_paths} before a gate command fails confusingly, and recommend cf-gears-init instead of treating it as a code defect.
STATE:
  SET GEARS_CI_FACADE_HEALTHY: true | false | unset (default unset, scope workflow_run)
DO:
  SKIP the rest of this unit WHEN GEARS_CI_FACADE_HEALTHY is already set for this workflow_run — check once per run, not once per slice or stage
  LOAD {ci_commands} and {project_paths}
  SET GEARS_CI_FACADE_HEALTHY = false WHEN either file is missing entirely, OR every operation/entry in both files together is mapped to an empty string/list (a facade blanked out everywhere, not selectively trimmed, reads as "never configured" rather than "deliberately disabled"), OR `gear_ci`/`dylint`/`full_check` are still their exact shipped gears-rust defaults (literal `make ...` invocations) AND this project has no `Makefile` at its root defining those same targets — else true
  EMIT, WHEN GEARS_CI_FACADE_HEALTHY == false, "This project's CI/build facades ({ci_commands}, {project_paths}) look unconfigured for this project — missing, fully blanked, or still pointing at constructorfabric/gears-rust's own Makefile targets and paths in a project that has neither. The gate commands below will likely fail with a 'command not found' error, not a real code defect. Recommend running `cf-gears-init` first." then CONTINUE anyway — never block or skip the actual gate on this alone, since a false positive here must not stop real work
RULES:
  ALWAYS run this at most once per workflow_run
  NEVER treat a selectively-emptied facade (some operations/entries blank, most populated) as unhealthy — {ci_commands}/{project_paths}'s own documented convention is that an empty entry is a deliberate, supported "no equivalent here," not a sign of neglect
  ALWAYS recommend `cf-gears-init` by name rather than describing the problem without a next action
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
  RUN GearsCodeDispatchContext — close is the one stage GearsCodeStageRoute skips it for (it dispatches no creative sub-agent), but GEARS_DEPS_CHANGED/GEARS_REST_CHANGED/GEARS_GTS_TOUCHED/GEARS_LIBS_FIPS_TOUCHED are set only inside that unit, and the next line needs them; without this explicit call they would be read here in their unset default state every time
  RUN GearsCiFacadeSanityCheck — close also runs gear_ci/dylint/full_check directly, independent of whether this run passed through a validate stage that would otherwise have triggered this check
  RUN {ci_commands}'s `gear_ci` and `dylint` commands (substituting GEARS_GEAR for `<GEAR>`; skip either that's mapped to an empty string) plus `cfs validate` when GEARS_CODE_MODE == feature-led, and keep the result as ci-findings
  LOAD {testing_policy} and {ci_commands}, and RUN every additional local check {testing_policy}'s routing table maps to GEARS_DEPS_CHANGED, GEARS_REST_CHANGED, GEARS_GTS_TOUCHED, GEARS_LIBS_FIPS_TOUCHED, and GEARS_GEAR for this slice — using each condition's command string from {ci_commands}, never a literal command written here — folding each result into ci-findings — these catch what `gear_ci`/`dylint` alone do not and that CI gates on separately; {testing_policy} is the single source of truth for which condition maps to which named operation, {ci_commands} is the single source of truth for what that operation actually runs, neither is duplicated here
  LOAD {cf-studio-path}/.core/skills/studio/modules/runtime/artifact-dod-check.md
  SET DOD_INPUT_ITEMS = the items of "{gears_code_phase}#definition-of-done", each normalized to item_id, summary, and required; for the security-boundary-evidence item, set its coverage_hints to the security-boundary evidence recorded for the slice (or the explicit statement that no guardrail was touched), so a genuinely-satisfied item has something to match against instead of being marked unclear for lack of a hint
  SET TARGET_ARTIFACTS = code-changes (ref GEARS_SLICE), unit-tests (ref the slice's tests), ci-findings (ref the gate result above), and review-findings (ref GEARS_FORWARD_PAYLOAD, summary including the security-boundary evidence or no-guardrail-touched statement)
  RUN ArtifactDodCheckContract
  SET GEARS_LAST_FEATURE_IN_CHAIN = false — initialize once, then only ever upgrade to true below; neither branch below gets its own "else false", since a second sequential SET's implicit else would silently overwrite the other branch's true back to false for the opposite mode
  SET GEARS_LAST_FEATURE_IN_CHAIN = true WHEN ARTIFACT_DOD_STATUS == ready AND every slice of GEARS_SOURCE_PATH is implemented AND GEARS_CODE_MODE == design-led — design-led mode has no DECOMPOSITION/FEATURE layer above it, so "every slice of this run's source implemented" already is the whole planned scope; NEVER treat a missing backreference in feature-led mode the same way (see next line)
  SET GEARS_LAST_FEATURE_IN_CHAIN = true WHEN ARTIFACT_DOD_STATUS == ready AND every slice of GEARS_SOURCE_PATH is implemented AND GEARS_CODE_MODE == feature-led AND resolving GEARS_SOURCE_PATH's DECOMPOSITION backreference (a required field on every FEATURE per `{gears_code_phase}`'s source rules, not optional) to the gear's `DECOMPOSITION.md` shows every feature entry there as `[x]` — an unresolved backreference leaves the flag at its already-initialized false; a FEATURE missing its required backreference is a structural problem `cfs validate` should already have caught upstream, not a signal to treat this as chain-complete
  RUN GearsCodeChainSweep WHEN GEARS_LAST_FEATURE_IN_CHAIN — run this BEFORE the SKILL_RESULT EMIT below, not after: that EMIT's own completed/failed status must reflect whether this sweep blocked, not just this slice's own DoD
  EMIT a SKILL_RESULT envelope with skill = GEARS_CODE_SKILL, status = completed WHEN ARTIFACT_DOD_STATUS == ready AND (GEARS_LAST_FEATURE_IN_CHAIN == false OR GEARS_CHAIN_SWEEP_STATUS == ready) else failed, produced_artifacts = code-changes and unit-tests for GEARS_SLICE plus phase-status, report_outputs = ARTIFACT_DOD_COVERAGE plus the security-boundary evidence recorded for the slice (or the statement that no guardrail was touched), missing_artifacts = the ARTIFACT_DOD_COVERAGE entries with coverage_status uncovered or unclear, or [] WHEN status == completed, assumptions = any recorded overrides, and suggested_next_skills = [GEARS_CODE_SKILL, cf-git-commit] WHEN status == completed else [GEARS_CODE_SKILL] — a chain sweep that just blocked is not a completed close, whatever this slice's own DoD said
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT when set, GEARS_SLICE, GATE_STATUS, plus the uncovered or unclear ARTIFACT_DOD_COVERAGE entries WHEN ARTIFACT_DOD_STATUS == blocked, so the gate runs again and routes to author or fix from fresh evidence
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE tests, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SOURCE_CONTEXT when set WHEN ARTIFACT_DOD_STATUS == ready AND GEARS_SOURCE_PATH still has unimplemented slices
  SET NEXT_ACTION_PINNED_SKILL = cf-git-commit WHEN ARTIFACT_DOD_STATUS == ready AND every slice of GEARS_SOURCE_PATH is implemented AND (GEARS_LAST_FEATURE_IN_CHAIN == false OR GEARS_CHAIN_SWEEP_STATUS == ready)
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SLICE (this run's own, since a `CI`-labeled finding names no artifact, just a failing command), plus the canonical `review-findings` artifact carrying every remaining GEARS_CHAIN_SWEEP_FINDING for the next round (never a GEARS_*-named field — see the stripping note below), WHEN GEARS_LAST_FEATURE_IN_CHAIN AND GEARS_CHAIN_SWEEP_STATUS == blocked AND at least one finding carries the `CI` label — a failing lint/dependency/build command has no FEATURE or DESIGN to blame; re-running this gear's own validate stage is the only thing that makes sense, and it takes priority over the semantic branches below since nothing else can close while the build itself is red
  SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_GEAR, plus GEARS_SOURCE_PATH and GEARS_SLICE taken from the first GEARS_CHAIN_SWEEP_FINDING that names a FEATURE/slice (never the just-closed GEARS_SOURCE_PATH/GEARS_SLICE — the finding may implicate a different FEATURE entirely), plus the canonical `review-findings` artifact carrying every remaining GEARS_CHAIN_SWEEP_FINDING for the next round (never a GEARS_*-named field for this — GearsCodeStageResolve strips GEARS_* fields when building GEARS_FORWARD_PAYLOAD, which would silently drop them), WHEN GEARS_LAST_FEATURE_IN_CHAIN AND GEARS_CODE_MODE == feature-led AND GEARS_CHAIN_SWEEP_STATUS == blocked AND no finding carries the `CI` label AND at least one finding names a FEATURE/slice
  SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL_DESIGN (`cf-gears-doc-design`) and NEXT_ACTION_PAYLOAD = GEARS_GEAR, GEARS_UPSTREAM_PATH = the gear's `DESIGN.md`, plus the canonical `review-findings` artifact carrying every GEARS_CHAIN_SWEEP_FINDING that names DESIGN/DECOMPOSITION itself rather than a specific FEATURE (never a GEARS_*-named field — same stripping risk as above), WHEN GEARS_LAST_FEATURE_IN_CHAIN AND GEARS_CODE_MODE == feature-led AND GEARS_CHAIN_SWEEP_STATUS == blocked AND no finding carries the `CI` label AND no finding names a FEATURE/slice — a DESIGN-level contradiction has no code fix to route to
  EMIT, WHEN GEARS_LAST_FEATURE_IN_CHAIN AND GEARS_CODE_MODE == design-led AND GEARS_CHAIN_SWEEP_STATUS == blocked AND no finding carries the `CI` label, a menu quoting every `CODE_OR_DESIGN`-labeled GEARS_CHAIN_SWEEP_FINDING and asking which side is wrong — design-led mode has no FEATURE/DECOMPOSITION spec to arbitrate a code-vs-DESIGN mismatch, so this is a genuine judgment call the agent must not make silently for the user:
    1 fix the code -> SET NEXT_ACTION_PINNED_SKILL = GEARS_CODE_SKILL and NEXT_ACTION_PAYLOAD = GEARS_STAGE validate, GEARS_GEAR, GEARS_SOURCE_PATH, GEARS_SLICE, plus the canonical `review-findings` artifact carrying the chosen findings
    2 fix DESIGN -> SET NEXT_ACTION_PINNED_SKILL = GEARS_DOC_SKILL_DESIGN (`cf-gears-doc-design`) and NEXT_ACTION_PAYLOAD = GEARS_GEAR, GEARS_UPSTREAM_PATH = the gear's `DESIGN.md`, plus the canonical `review-findings` artifact carrying the chosen findings
  STOP_TURN WHEN that menu was just emitted, so the user's choice resolves on the next turn rather than the agent guessing
  LOAD {cf-studio-path}/.core/skills/studio/modules/ui/next-actions.md
  RUN NextActionsOffer
RULES:
  ALWAYS list remaining MINOR review findings in the close report
  NEVER mark the slice done while a gate command fails or a CRITICAL or MAJOR finding is unresolved
  NEVER run a {testing_policy}-routed check whose triggering condition is false for this slice — the routing table decides what's relevant; this unit never re-runs every CI lane on every close
  ALWAYS run GearsCodeChainSweep when GEARS_LAST_FEATURE_IN_CHAIN is true, even if every per-slice check up to now has passed — per-slice closes only ever checked one FEATURE against DESIGN as it stood when that slice was authored, never the whole realized chain against each other
  NEVER pick a side of a design-led CODE_OR_DESIGN finding on the user's behalf; ask, every time, exactly once per blocked sweep
```

```pdsl
UNIT GearsCodeChainSweep
PURPOSE: When the gear's whole planned FEATURE set (feature-led) or whole design-led run just finished, sweep the whole realized chain for drift and run the fuller local CI pass once — neither of which any single slice's close ever does. Implements the chain-reconciliation option named in gears-rust#5195 ("Fix drift across chain artifacts") at the one point a reconciliation pass is guaranteed to have something complete to reconcile against.
STATE:
  SET GEARS_CHAIN_SWEEP_STATUS: ready | blocked | unset (default unset, scope workflow_run)
  SET GEARS_CHAIN_SWEEP_FINDING: list | unset (default unset, scope workflow_run)
DO:
  LOAD {ci_commands}
  RUN {ci_commands}'s `full_check` command (substituting GEARS_GEAR for `<GEAR>`; fmt, clippy, full test suite, dependency-policy check — not the narrower per-slice `gear_ci`) and fold the result into ci-findings, each carrying the fixed target label `CI`; consult {testing_policy} for whether this gear's heavier lanes (`test_fips`, `test_graph_storage`, `test_onnx_embedding_plugin`/`test_onnx_embedding_compile_check`, `test_magika`) apply and run those too, using each one's command from {ci_commands} (substituting GEARS_GEAR for `<GEAR>` wherever that placeholder appears) — skip any that {ci_commands} maps to an empty string rather than erroring — since this is the point a partial local pass stops being good enough; always additionally RUN `e2e_local`'s command (substituting GEARS_GEAR for `<GEAR>`) unconditionally at this point too — it is the one heavier lane with no diff-based trigger condition in {testing_policy}, since it is gear-scoped discovery (a no-op when GEARS_GEAR has no matching e2e-launcher suite), not path-scoped routing
  LOAD {cf-studio-path}/.core/skills/studio/modules/subagents/dispatch.md
  RUN SubAgentDispatch for the cf-semantic-reviewer-artifact dispatch group WHEN GEARS_CODE_MODE == feature-led
  DISPATCH cf-semantic-reviewer-artifact from {cf-studio-path}/.core/skills/studio/agents/cf-semantic-reviewer-artifact.md with target_paths=[the gear's `DESIGN.md`], kit_rules_path={design_rules}, checklist_path={design_checklist}, template_path={design_template}, example_path={design_example}, cross_ref_paths=[the gear's now-complete `DECOMPOSITION.md`, every file under `docs/features/`, every `ADR/*.md`], rules_mode=STRICT, traceability_mode=the gear's already-resolved FULL/DOCS-ONLY mode, and the task instruction: re-check `{design_checklist}`'s SEM-DESIGN-005 (DECOMPOSITION/FEATURE Realization Consistency) and SEM-DESIGN-003 (ADR Consistency and Coverage) sections specifically against the now-complete DECOMPOSITION/FEATURE set, not the authoring-time snapshot each FEATURE's own close checked against; report findings from any other category too — every CRITICAL/MAJOR finding gates this sweep below, not just these two — this is the blocking backstop for a DESIGN amendment made for a later FEATURE silently invalidating an earlier one's already-closed slice (GearsDocDesignAmendImpactCheck in modules/doc-stage-router.md surfaces the same drift non-blockingly, right when DESIGN is amended, via cf-gears-change-impact-analysis — this sweep is what actually gates CODE close when that earlier, advisory-only signal was missed or ignored); label each finding with the specific FEATURE path and implicated slice/ID it names, or `DESIGN` when the contradiction is in DESIGN/DECOMPOSITION itself WHEN GEARS_CODE_MODE == feature-led
  RUN SubAgentDispatch for the cf-semantic-reviewer-code dispatch group WHEN GEARS_CODE_MODE == design-led
  DISPATCH cf-semantic-reviewer-code from {cf-studio-path}/.core/skills/studio/agents/cf-semantic-reviewer-code.md with design_artifact_path=the gear's `DESIGN.md`, code_paths=[`gears/<GEARS_GEAR>/<GEARS_GEAR>/src/`, `gears/<GEARS_GEAR>/<GEARS_GEAR>-sdk/src/` when that crate exists], kit_rules_path={codebase_design_led_rules}, rules_mode=STRICT, traceability_mode=DOCS-ONLY, cross_ref_paths=[], and the task instruction: re-check `{codebase_checklist}`'s SEM-CODE-007 (Overall Design Consistency) specifically against the gear's now-fully-realized source tree, not the slice-by-slice snapshot each slice's own close checked against — design-led mode has no DECOMPOSITION/FEATURE layer to cross-reference, so the realized code itself is what this sweep reconciles DESIGN against; report findings from any other category too — every CRITICAL/MAJOR finding gates this sweep below, not just SEM-CODE-007; label every finding `CODE_OR_DESIGN` (whether the code or DESIGN is the one that's wrong is not this reviewer's call to make) WHEN GEARS_CODE_MODE == design-led
  SET GEARS_CHAIN_SWEEP_STATUS = blocked WHEN the DISPATCH above did not return a report (no response, malformed output, or the dispatch itself errored) — an absent report is not the same as a clean one and must never read as "no findings" (same "an unset report never closes" principle GearsCodeStageResolve already applies to ReviewFindingsReport); SET GEARS_CHAIN_SWEEP_FINDING = a single synthetic finding labeled `CI` describing the dispatch failure, so GearsCodeClose's routing below has something concrete to act on (re-running this gear's own validate/chain-sweep stage) instead of nothing
  SET GEARS_CHAIN_SWEEP_STATUS = ready WHEN the DISPATCH above returned a report AND the fuller CI pass is green AND that report surfaces no CRITICAL or MAJOR finding in ANY category, else blocked — SEM-DESIGN-005/SEM-DESIGN-003 (feature-led) or SEM-CODE-007 (design-led) are this sweep's deepest-scrutiny focus, not its only gating categories; a CRITICAL/MAJOR finding anywhere else in the same holistic review blocks exactly like GearsCodeClose's own "never mark done with an unresolved CRITICAL/MAJOR" rule already requires
  SET GEARS_CHAIN_SWEEP_FINDING = every CRITICAL or MAJOR finding from any category (each already labeled per the DISPATCH instructions above), plus every failing fuller-CI command labeled `CI`, WHEN GEARS_CHAIN_SWEEP_STATUS == blocked AND the dispatch did return a report; leave it as the synthetic `CI` finding line 198 already set WHEN GEARS_CHAIN_SWEEP_STATUS == blocked AND the dispatch did not return a report — never overwrite that with an empty list, it's the one thing GearsCodeClose's routing below has to act on; SET GEARS_CHAIN_SWEEP_FINDING = [] WHEN GEARS_CHAIN_SWEEP_STATUS == ready
RULES:
  ALWAYS run this exactly once per completed FEATURE set (feature-led) or completed design-led run, not once per slice
  ALWAYS give every GEARS_CHAIN_SWEEP_FINDING an explicit target label (a FEATURE path/slice, `DESIGN`, `CODE_OR_DESIGN`, or `CI`); GearsCodeClose routes on it and cannot re-route a finding with no label without guessing
  NEVER skip the re-review because every individual slice already passed its own close — that's precisely the drift this sweep exists to catch
  NEVER silently absorb a chain-sweep finding into a normal slice-level fix; route FEATURE-labeled findings back through validate on the FEATURE the finding names, and never pick a side of a `CODE_OR_DESIGN` finding on GearsCodeClose's own authority — that one is the user's call
  NEVER read a missing, malformed, or failed DISPATCH response as "no findings" — an absent report blocks the sweep, it never defaults to ready
  NEVER let GEARS_CHAIN_SWEEP_FINDING end up empty while GEARS_CHAIN_SWEEP_STATUS == blocked — GearsCodeClose's routing below has nothing to act on otherwise
```
