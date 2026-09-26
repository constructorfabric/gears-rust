---
description: "Tracked improvement candidates for the gears Constructor Studio kit, with status, priority, and links."
---

# studio-kit-gears Improvements


<!-- toc -->

- [How to use this file](#how-to-use-this-file)
  - [Statuses](#statuses)
  - [Priority](#priority)
- [Index](#index)
- [Items](#items)
  - [KIT-001: Fix drift across chain artifacts](#kit-001-fix-drift-across-chain-artifacts)
  - [KIT-002: Scaffold stage for new gears](#kit-002-scaffold-stage-for-new-gears)
  - [KIT-003: Automated regression runs of the kit workflows](#kit-003-automated-regression-runs-of-the-kit-workflows)
  - [KIT-004: End-to-end gear chain orchestrator](#kit-004-end-to-end-gear-chain-orchestrator)
  - [KIT-005: Commit upstream docs before code stages](#kit-005-commit-upstream-docs-before-code-stages)
  - [KIT-006: Turn remaining contract rules into explicit steps](#kit-006-turn-remaining-contract-rules-into-explicit-steps)
  - [KIT-007: Deterministic table-of-contents generation](#kit-007-deterministic-table-of-contents-generation)
  - [KIT-008: Scope the architecture lint gate to the gear](#kit-008-scope-the-architecture-lint-gate-to-the-gear)
  - [KIT-009: Pin the `cfs` proxy version in CI](#kit-009-pin-the-cfs-proxy-version-in-ci)
  - [KIT-010: Backfill frontmatter descriptions in existing docs](#kit-010-backfill-frontmatter-descriptions-in-existing-docs)
  - [KIT-011: Reduce table-of-contents warnings](#kit-011-reduce-table-of-contents-warnings)
  - [KIT-012: Bundled SDLC kit and PDSL validator gaps](#kit-012-bundled-sdlc-kit-and-pdsl-validator-gaps)
  - [KIT-013: `cfs validate-toc` exit code disagrees with its JSON](#kit-013-cfs-validate-toc-exit-code-disagrees-with-its-json)
  - [KIT-014: Duplicate `WriteDocsReviewFixGate` unit in the Studio core](#kit-014-duplicate-writedocsreviewfixgate-unit-in-the-studio-core)
  - [KIT-015: Merge the two coding presets](#kit-015-merge-the-two-coding-presets)
  - [KIT-016: Validate the change-impact report path](#kit-016-validate-the-change-impact-report-path)
- [Item template](#item-template)

<!-- /toc -->

Tracked improvement candidates for the gears Constructor Studio kit, the
repository settings it depends on, and upstream Studio issues that affect it.
Candidates come from live runs of the kit workflows, reviews, and Studio
upgrades.

## How to use this file

- To change a status, edit the **Status** cell in the [index](#index) and the
  `Status:` line of the item. Keep both in sync.
- To add a candidate, take the next free ID (`KIT-NNN`), add a row to the
  index, and add an item section using the [template](#item-template).
- Never reuse or renumber an ID. A rejected or superseded item stays in the
  file with its final status and a one-line reason.
- Link the pull request or issue that moves an item forward in its `Links:`
  line.

### Statuses

| Status | Meaning |
|--------|---------|
| `proposed` | Identified, not yet agreed on |
| `accepted` | Agreed to do, not started |
| `in-progress` | Work is underway (link the branch or PR) |
| `partial` | Part of the scope is done; the item says what remains |
| `done` | Merged; link the PR |
| `reported` | Upstream issue filed; waiting on the upstream project |
| `rejected` | Decided against; the item says why |

### Priority

| Priority | Meaning |
|----------|---------|
| `high` | Blocks or degrades normal kit use, or makes upgrades expensive |
| `medium` | Noticeable friction or hidden risk |
| `low` | Polish or noise reduction |

## Index

| ID | Title | Area | Priority | Status |
|----|-------|------|----------|--------|
| [KIT-001](#kit-001-fix-drift-across-chain-artifacts) | Fix drift across chain artifacts | kit | high | proposed |
| [KIT-002](#kit-002-scaffold-stage-for-new-gears) | Scaffold stage for new gears | kit | high | proposed |
| [KIT-003](#kit-003-automated-regression-runs-of-the-kit-workflows) | Automated regression runs of the kit workflows | kit | high | proposed |
| [KIT-004](#kit-004-end-to-end-gear-chain-orchestrator) | End-to-end gear chain orchestrator | kit | medium | proposed |
| [KIT-005](#kit-005-commit-upstream-docs-before-code-stages) | Commit upstream docs before code stages | kit | medium | proposed |
| [KIT-006](#kit-006-turn-remaining-contract-rules-into-explicit-steps) | Turn remaining contract rules into explicit steps | kit | medium | partial |
| [KIT-007](#kit-007-deterministic-table-of-contents-generation) | Deterministic table-of-contents generation | kit | low | proposed |
| [KIT-008](#kit-008-scope-the-architecture-lint-gate-to-the-gear) | Scope the architecture lint gate to the gear | kit | low | proposed |
| [KIT-009](#kit-009-pin-the-cfs-proxy-version-in-ci) | Pin the `cfs` proxy version in CI | repo | medium | proposed |
| [KIT-010](#kit-010-backfill-frontmatter-descriptions-in-existing-docs) | Backfill frontmatter descriptions in existing docs | repo | low | partial |
| [KIT-011](#kit-011-reduce-table-of-contents-warnings) | Reduce table-of-contents warnings | repo | low | proposed |
| [KIT-012](#kit-012-bundled-sdlc-kit-and-pdsl-validator-gaps) | Bundled SDLC kit and PDSL validator gaps | upstream | medium | reported |
| [KIT-013](#kit-013-cfs-validate-toc-exit-code-disagrees-with-its-json) | `cfs validate-toc` exit code disagrees with its JSON | upstream | medium | proposed |
| [KIT-014](#kit-014-duplicate-writedocsreviewfixgate-unit-in-the-studio-core) | Duplicate `WriteDocsReviewFixGate` unit in the Studio core | upstream | low | proposed |
| [KIT-015](#kit-015-merge-the-two-coding-presets) | Merge the two coding presets | kit | low | proposed |
| [KIT-016](#kit-016-validate-the-change-impact-report-path) | Validate the change-impact report path | kit | low | proposed |

## Items

### KIT-001: Fix drift across chain artifacts

- **Status:** proposed
- **Priority:** high
- **Area:** kit
- **Problem:** A preset run is bound to one artifact. The consistency review
  checks the whole chain and finds drift in upstream documents, such as a
  DESIGN claim contradicted by the PRD or a stale link in DECOMPOSITION, but
  the fix stage can only edit the current file. In local runs, FEATURE and
  code reviews each surfaced four CRITICAL findings in upstream docs that the
  run could not fix.
- **Benefit:** The document chain stays consistent without manual detours.
- **Options:** a dedicated chain-reconciliation stage, or letting the fix
  stage edit upstream artifacts after a separate, explicit approval.
- **Links:** —

### KIT-002: Scaffold stage for new gears

- **Status:** proposed
- **Priority:** high
- **Area:** kit
- **Problem:** No stage creates the gear skeleton. The code router's tests
  stage improvises the crates and the registration surface, and conventions
  such as `package.metadata.docs.rs.all-features = true` surface only when
  `make dylint` fails.
- **Benefit:** Canonical crate layout and a complete registration surface
  (workspace members, example server, feature flags, cargo-shear exemptions,
  e2e config) on the first try.
- **Options:** reuse the scaffold phase and gates from the experimental gear
  kit (github.com/vasylcf/kit-gear-creation).
- **Links:** —

### KIT-003: Automated regression runs of the kit workflows

- **Status:** proposed
- **Priority:** high
- **Area:** kit
- **Problem:** The kit was verified on Studio v1.7.0 by driving every
  workflow by hand through headless agent sessions. The next Studio upgrade
  would need the same manual effort.
- **Benefit:** A scripted run of all kit workflows against a throwaway gear
  turns a Studio upgrade check into a report instead of a day of manual
  testing.
- **Options:** headless sessions with pre-answered gates, or the Studio
  eval-harness scaffold.
- **Links:** —

### KIT-004: End-to-end gear chain orchestrator

- **Status:** proposed
- **Priority:** medium
- **Area:** kit
- **Problem:** The user invokes each preset separately: PRD, DESIGN,
  DECOMPOSITION, FEATURE, then code.
- **Benefit:** One entry point from intake to code, with a similar-gear
  lookup before authoring starts.
- **Options:** an intake preset that records the route and hands off stage by
  stage; see the intake phase of the experimental gear kit.
- **Links:** —

### KIT-005: Commit upstream docs before code stages

- **Status:** proposed
- **Priority:** medium
- **Area:** kit
- **Problem:** `cf-codegen` runs in an isolated worktree that only sees
  committed files, so it cannot read uncommitted gear docs. The code router
  currently works around this by dispatching a main-context coder.
- **Benefit:** Restores worktree isolation for code generation, which is the
  safer default.
- **Options:** require the source docs to be committed before the tests and
  author stages, or commit them automatically when the git write policy
  allows it.
- **Links:** —

### KIT-006: Turn remaining contract rules into explicit steps

- **Status:** partial
- **Priority:** medium
- **Area:** kit
- **Problem:** The agent sometimes skips instructions stated only as `RULES`.
  Two cases were observed and fixed: the stage-end next-actions menu and the
  kit checklist for code reviewers are now explicit `DO` steps.
- **Benefit:** Stage contracts hold regardless of model variance.
- **Remaining:** audit the other router rules (payload forwarding, validate
  stage never editing files) and convert the load-bearing ones.
- **Links:** constructorfabric/gears-rust#4930

### KIT-007: Deterministic table-of-contents generation

- **Status:** proposed
- **Priority:** low
- **Area:** kit
- **Problem:** The author stage sometimes skips `cfs toc`, so the validate
  stage fails once and routes back to author.
- **Benefit:** Removes a wasted author → validate round trip.
- **Options:** run `cfs toc` as a kit-owned step after the author stage.
- **Links:** —

### KIT-008: Scope the architecture lint gate to the gear

- **Status:** proposed
- **Priority:** low
- **Area:** kit
- **Problem:** The code gate runs `make dylint` over the whole workspace (about
  14 minutes in CI) for a change in one gear.
- **Benefit:** A faster validate stage for code.
- **Options:** a gear-scoped dylint target, if `cargo gears lint` supports it.
- **Links:** —

### KIT-009: Pin the `cfs` proxy version in CI

- **Status:** proposed
- **Priority:** medium
- **Area:** repo
- **Problem:** `.cf-studio/version.toml` pins the Studio core, but the
  Makefile installs the `cfs` proxy from Studio's default branch
  (`CFS_PIPX_SPEC ?= git+https://github.com/constructorfabric/studio.git`).
  A change on that branch can break CI without any change in this repository.
- **Benefit:** Reproducible CFS checks.
- **Options:** pin `CFS_PIPX_SPEC` to the same tag as the core pin and bump
  both together.
- **Links:** —

### KIT-010: Backfill frontmatter descriptions in existing docs

- **Status:** partial
- **Priority:** low
- **Area:** repo
- **Problem:** Studio v1.7.0 warns (`toc-missing-description`) on every
  document without a frontmatter `description`. The SDLC templates now carry
  the field, so new documents get it; about 200 existing gear documents do
  not.
- **Benefit:** Removes the warning and lets retrieval pick the right document
  before reading it.
- **Remaining:** add a one-sentence `description` to the existing documents.
- **Links:** constructorfabric/gears-rust#4930

### KIT-011: Reduce table-of-contents warnings

- **Status:** proposed
- **Priority:** low
- **Area:** repo
- **Problem:** Studio v1.7.0 reports about 670 warnings across the repository,
  mostly duplicate headings and heading-depth jumps.
- **Benefit:** Less validator noise, so real problems stand out.
- **Links:** —

### KIT-012: Bundled SDLC kit and PDSL validator gaps

- **Status:** reported
- **Priority:** medium
- **Area:** upstream
- **Problem:** The Studio bundled SDLC kit still continues into units removed
  in v1.6, and `cfs pdsl validate` does not report undefined `CONTINUE`,
  `RUN`, or `EMIT_MENU` targets. The gears kit inherited the broken entry
  points from that kit.
- **Benefit:** A validator that catches dangling unit references on every
  Studio upgrade.
- **Links:** constructorfabric/studio#280

### KIT-013: `cfs validate-toc` exit code disagrees with its JSON

- **Status:** proposed
- **Priority:** medium
- **Area:** upstream
- **Problem:** `cfs validate-toc` exits with code 2 on a WARN result whose JSON
  reports `error_count: 0`. A caller that gates on the exit code, as the
  dispatched deterministic-validator agent did, reports a passing gate as
  failed.
- **Benefit:** Gate results that agree across exit code and JSON.
- **Next step:** reproduce and file an upstream Studio issue.
- **Links:** —

### KIT-014: Duplicate `WriteDocsReviewFixGate` unit in the Studio core

- **Status:** proposed
- **Priority:** low
- **Area:** upstream
- **Problem:** `workflows/documenting-review.md` and
  `skills/studio/modules/write-docs-review-run.md` both define
  `WriteDocsReviewFixGate` with different bodies; only the module's version is
  reachable. Reading either in isolation is misleading when extending the kit.
- **Next step:** file an upstream Studio issue, or add it to the KIT-012 issue.
- **Links:** —

### KIT-015: Merge the two coding presets

- **Status:** proposed
- **Priority:** low
- **Area:** kit
- **Problem:** `cf-gears-implement` (FEATURE-led, `@cpt-*` traceability) and
  `cf-gears-coding` (DESIGN-led) share one code stage router, phase plan, and
  checklist, but remain two presets bound to two rules files. Keeping two
  entry points and two rules files in step is extra surface for drift.
- **Benefit:** One coding entry point with a traceability-mode parameter and
  one rules file with mode-specific sections.
- **Options:** keep both skill names as thin aliases for discoverability, or
  replace them with a single preset.
- **Links:** raised in review of constructorfabric/gears-rust#4930

### KIT-016: Validate the change-impact report path

- **Status:** proposed
- **Priority:** low
- **Area:** kit
- **Problem:** `cf-gears-change-impact-analysis` builds its report path as
  `.change-impact/{UPSTREAM_ARTIFACT_ID}/report.md` from an unvalidated
  string, so an ID containing `/` or `..` can place the report outside
  `.change-impact/`.
- **Benefit:** The report always stays inside its namespace.
- **Options:** require `UPSTREAM_ARTIFACT_ID` to be a canonical `cpt-*` ID or a
  single safe path segment before rendering the report.
- **Links:** constructorfabric/gears-rust#4941

## Item template

```markdown
### KIT-NNN: <Title>

- **Status:** proposed
- **Priority:** high | medium | low
- **Area:** kit | repo | upstream
- **Problem:** <what is wrong or missing, with the evidence that surfaced it>
- **Benefit:** <what changes for kit users once this is done>
- **Options:** <candidate approaches, optional>
- **Links:** <PRs, issues, branches, or —>
```
