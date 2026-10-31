# CF/Gears Kit (Constructor Studio-compatible)

**ID**: `gears`
**Format**: `Constructor Studio` (targets Studio v1.8.0 or later — the FEATURE artifact's
CDSL clarity findings are declared warnings only from v1.8.0 onward; v1.7.0 reports them
as errors, see `artifacts/FEATURE/rules.md`)
**Purpose**: Own CF/Gears’s SDLC document templates, examples, and expert checklists (`artifacts/<KIND>/`), and the Studio workflows that author, validate, and review gear documents and code.

## Why use this kit

It turns this repo's own SDLC conventions - what a PRD/DESIGN/ADR must contain, which house
docs apply to what, which `make` targets gate what - into something an AI agent (or a human)
actually follows consistently, instead of re-deriving it from memory every time. It's also
installable standalone (`cfs kit install constructorfabric/gears-rust`) into any other
project, Gears-based or not.

| Capability | The problem it solves | Why gears-rust wants it | Why it's worth it elsewhere |
|---|---|---|---|
| Full SDLC chain — UPSTREAM_REQS → PRD → ADR + DESIGN → DECOMPOSITION → FEATURE → CODE, each with a template, rules, review checklist, and real worked examples, through an author → validate → review → fix → close lifecycle | Design docs drift from what's actually built, or get skipped under deadline pressure | One enforced path from "what's needed" to "what's shipped," with a real review gate at every stage, not just at code review | `cfs kit install` gives any project this same discipline instead of inventing SDLC templates from scratch |
| Two CODE modes — feature-led (full `@cpt-*` traceability to a FEATURE) and design-led (straight from DESIGN/ADR/PRD, no FEATURE layer) | Not every change deserves full FEATURE-level ceremony, but it still needs *some* structure and a defined done-state | Pick the right weight per task - a small fix and a new gear don't need the same process | Same flexibility for a project of any size; adoption isn't all-or-nothing |
| Repo-wide convention auto-citation — `docs/GEARS.md`, `guidelines/SECURITY.md`/`DEPENDENCIES.md`, Rust/REST DNA, the `toolkit_unified_system` chapter index, `docs/arch/` precedent - wired into both generation and review | House convention docs exist but get ignored: measured at **zero reads** for 4 directly-relevant docs across 3 real `cf-gears-implement` runs, before this kit wired them in | Every generate/review pass actually reads the rules relevant to what it's touching, not just what an agent happens to remember | Point the kit's `{variable}`s at *your own* convention docs and get the same guarantee for them |
| Reuse-before-build checks — `GEARS.md`'s own gear catalog consulted before naming a "new" gear or designing bespoke storage | Teams re-build a gear/capability that already exists or is already planned, because nobody checked the catalog first | Caught live: a PRD request for "email storage" would have scaffolded a duplicate gear right next to `GEARS.md`'s own existing (unimplemented) "Emails Storage" entry - now it's surfaced before the directory is even named | Point it at your own catalog and stop the same class of accidental duplication |
| Chain-drift detection — ADR checked against existing DESIGN/DECOMPOSITION/FEATURE (not just linked to them), a holistic sweep when a gear's last FEATURE closes, and an early, non-blocking check the moment DESIGN is amended mid-chain | A DESIGN change made for feature 5 silently invalidates feature 1, discovered (if ever) only much later, after far more was built on the stale assumption | Contradictions across the chain surface at the amendment itself, or at worst when the chain completes - never silently | Any multi-stage spec→code pipeline has this exact risk; this is a reusable backstop for it |
| Smart local CI routing — one table maps what a slice actually touched (deps, REST surface, GTS, FIPS libs, workflow files) to the exact `make` targets real CI gates on | Running the full check suite on every tiny change is slow; guessing which checks matter risks silently skipping one CI actually requires | Local dev loop matches CI exactly, with no manual guesswork and no reflexive "just run everything" | `{ci_commands}`/`cf-gears-init` adapt the same routing table to your own build tooling, not gears-rust's Makefile |
| Standalone-install facades — `{ci_commands}`/`{project_paths}` (user-editable, empty = "doesn't apply here"), `cf-gears-init` to auto-discover and populate them, and a sanity check that nudges you to run it when they still look untouched | A kit that hardcodes one repo's own build commands breaks with `make: *** No rule to make target` the moment it's installed anywhere else | Defense in depth even here - a reorganized build target still resolves through the facade instead of a dozen hardcoded call sites | This is *the* feature that makes the kit portable at all: `cf-gears-init` discovers your real tooling instead of you hand-patching kit internals |
| PR review suite — `cf-gears-pr-review` (SDLC traceability + guardrails), `cf-gears-toolkit-review` (idiomatic-Rust/ToolKit-framework conformance via 7 specialized sub-agents), `cf-gears-pr-status` | Reviewers either re-derive Rust/ToolKit idiom judgments ad hoc each time, or traceability review and code-quality review get conflated into one unfocused pass | Two genuinely complementary review lenses instead of one overloaded one, plus a structured, severity-tagged status report for a fast go/no-go read | `toolkit-pr-review`'s sub-agents and rule modules bundle standalone, so your own PRs get the same conformance pass |
| `cf-gears-change-impact-analysis` — read-only downstream blast-radius report for an upstream artifact change, in cascade-tracking or release-readiness-estimation mode | "What actually breaks if I change this PRD/DESIGN/ADR" is normally answered from memory and `git grep` | A concrete, evidence-backed answer before approving a change, not a surprise after | The same traceability engine runs on your own SDLC chain once the kit's artifact structure is in place |
| `cf-gears-gear-creator` — scaffolds a new ToolKit gear (or a feature on one) with the right DDD-light layers, SDK crate, OperationBuilder, SecureORM/AuthZ wiring, lifecycle/background tasks, SSE, and plugin points | New gears/features drift from framework idiom when scaffolded by hand, each one slightly differently from the last | Every new gear starts from the same correct skeleton instead of copy-pasting (and slowly diverging from) an old one | Bundled as a real, standalone-installable skill - the same scaffolding discipline for gears you write in your own repo |

## Artifact kinds

| Kind | `{template}` | `{rules}` | `{checklist}` | `{example}` |
|------|----------|-------|-----------|---------|
| UPSTREAM_REQS | `artifacts/UPSTREAM_REQS/template.md` | `artifacts/UPSTREAM_REQS/rules.md` | `artifacts/UPSTREAM_REQS/checklist.md` | `artifacts/UPSTREAM_REQS/examples` |
| PRD | `artifacts/PRD/template.md` | `artifacts/PRD/rules.md` | `artifacts/PRD/checklist.md` | `artifacts/PRD/examples` |
| ADR | `artifacts/ADR/template.md` | `artifacts/ADR/rules.md` | `artifacts/ADR/checklist.md` | `artifacts/ADR/examples` |
| DESIGN | `artifacts/DESIGN/template.md` | `artifacts/DESIGN/rules.md` | `artifacts/DESIGN/checklist.md` | `artifacts/DESIGN/examples` |
| DECOMPOSITION | `artifacts/DECOMPOSITION/template.md` | `artifacts/DECOMPOSITION/rules.md` | `artifacts/DECOMPOSITION/checklist.md` | `artifacts/DECOMPOSITION/examples` |
| FEATURE | `artifacts/FEATURE/template.md` | `artifacts/FEATURE/rules.md` | `artifacts/FEATURE/checklist.md` | `artifacts/FEATURE/examples` |
| CODE | — | `codebase/rules.md` + `codebase/design-led-rules.md` | `codebase/checklist.md` | — |

Template = structure/sections (authoring). Rules = required content (authoring + part of
review). Checklist = review criteria (review-only; authoring never loads it). Example =
reference implementation (review-only).

## How a preset runs

Every document preset (`cf-gears-doc-*`, `cf-gears-decompose`) and code preset
(`cf-gears-implement`, `cf-gears-coding`) is a stage router over the Studio
thin skills. One invocation runs one stage, then pins the same preset as the
suggested next action with the next stage:

- documents: author (`cf-documenting-gen`) → validate (`cf-documenting-ci`) →
  review (`cf-documenting-review`) → fix (`cf-documenting-fix`) → close
- code: tests (`cf-coding-tests`) → author (`cf-coding-gen`) → validate
  (`cf-coding-ci`) → review (`cf-coding-review`) → fix (`cf-coding-fix`) → close

The kit supplies the `phase-plan`, `phase-dod`, and `acceptance-criteria`
prerequisites the Studio skills require from `plans/doc-phase.md` and
`plans/code-phase.md`, so a single-artifact run needs no separate planning
session. The shared routing lives in `modules/doc-stage-router.md` and
`modules/code-stage-router.md`. Close checks the definition of done and offers
the next artifact of the chain.

Template and rules changes apply to newly authored documents only. Existing
gear documents keep their structure until someone revises them; the backlog
for bringing them in line (for example the frontmatter `description`) is
tracked separately in the kit improvements backlog.

## Repo-wide convention docs and standalone installs

The CODE and doc-artifact rules cite repo-wide convention docs that live outside this kit
folder by `{variable}` name, never by a hardcoded path, since the kit never assumes where
its resources end up on disk. `.cf-studio-kit.toml` declares each as a kit resource so a
kit installed outside the `gears-rust` checkout gets its own bundled copy, and
`{cfs_cmd} resolve-vars` resolves the name to wherever that install actually placed it.
`guidelines/DNA` is additionally a git submodule in `gears-rust` itself —
`{ensure_dna_script}` initializes it when working inside a `gears-rust` checkout that
hasn't fetched it yet; a standalone install has no submodule to init at all and instead
relies entirely on the bundled copy from install time.

**This matters before `{ensure_dna_script}` ever gets a chance to run, too**: `cfs`'s own
kit-model loader validates every registered resource's `source` path eagerly, with no
lazy/optional mode, so a resource pointed at a file or subdirectory *inside* an
uninitialized submodule fails `cfs init`/`cfs generate-agents`/`cfs kit validate` outright
with `Resource '<id>': source path '...' does not exist` — before `{ensure_dna_script}` or
any other kit prerequisite ever runs, and with no manifest-level opt-out (confirmed: no
`required`/`optional` field exists in `cfs`'s resource schema). `{dna_root}` is therefore
pointed at the submodule's own root (`guidelines/DNA`), not a path inside it: an
uninitialized git submodule's mount-point directory still exists on disk (empty, but
`exists()` is true — confirmed empirically), so this resource always passes that check
regardless of fetch state, and `{ensure_dna_script}` remains the thing that actually fetches
real content into it before anything relies on it. `make cfs-repair`/`cfs-validate`/
`cfs-validate-kits` additionally depend on `ensure-submodules` as defense in depth, for any
*other* resource that might someday point inside an unfetched submodule.

### Which guide is used where

| Guide (`{variable}`) | Resolves to | Used by | Stage |
|---|---|---|---|
| `{gears_md}` | `docs/GEARS.md` | CODE, DESIGN, PRD, UPSTREAM_REQS, DECOMPOSITION rules | Authoring |
| `{toolkit_unified_system}` | `docs/toolkit_unified_system/` | CODE, DESIGN rules; `plans/code-phase.md` | Authoring |
| `{guidelines_security}` | `guidelines/SECURITY.md` | CODE rules; `artifacts/ISO_29148.md` | Authoring |
| `{gears_security}` | `docs/security/SECURITY.md` | CODE, DESIGN rules | Authoring |
| `{guidelines_dependencies}` | `guidelines/DEPENDENCIES.md` | CODE rules | Authoring |
| `{dna_root}` | `guidelines/DNA/` | CODE rules, cited as `{dna_root}/languages/RUST.md` and, for REST/SSE gears only, `{dna_root}/REST/` | Authoring |
| `{ensure_dna_script}` | `scripts/ensure-dna.py` | CODE rules (prerequisite) | Authoring |
| `{testing_policy}` | `docs/TESTING.md` | CODE, DESIGN rules | Authoring |
| `{fips_requirements}` | `docs/security/fips/` | CODE rules/checklist, DESIGN, PRD rules (FIPS-touching work only) | Both |
| `{guidelines_gts}` | `guidelines/GTS.md` | CODE, DESIGN checklist (review-only; not loaded at authoring) | Review |
| `{toolkit_pr_review_docs}` | `docs/toolkit-pr-review/` | CODE checklist | Review |
| `{toolkit_pr_review_scripts}` | `tools/scripts/toolkit-pr-review/` | `{skill_toolkit_pr_review}`'s own mandatory `review.py prepare` step (see its `SKILL.md`) | Review |
| `{architecture_manifest}` | `docs/ARCHITECTURE_MANIFEST.md` | PRD rules/checklist/template, DESIGN checklist, `artifacts/ISO_29148.md` | Both |
| `{iso_29148}` | `artifacts/ISO_29148.md` | PRD, UPSTREAM_REQS rules | Authoring |
| `{guidelines_readme}` | `guidelines/README.md` | PRD rules (Operational Concept, NFR sections) — indexes the other `guidelines/*` variables | Authoring |
| `{repo_adrs}` | `docs/adrs/` | ADR, DESIGN rules | Authoring |
| `{repo_arch}` | `docs/arch/` | CODE, ADR, PRD, DESIGN rules | Authoring |
| `{e2e_guide}` | `testing/e2e/README.md` | CODE rules (how to actually run the E2E suite) | Authoring |
| `{repo_playbook}` | `docs/REPO_PLAYBOOK.md` | PRD, UPSTREAM_REQS, DESIGN, DECOMPOSITION rules | Authoring |

## Workflows

| Preset | `{variable}` | Does |
|---|---|---|
| `cf-gears-doc-upstream-reqs` | `{workflow_doc_upstream_reqs}` | author/review UPSTREAM_REQS |
| `cf-gears-doc-prd` | `{workflow_doc_prd}` | author/review PRD |
| `cf-gears-doc-adr` | `{workflow_doc_adr}` | author/review ADR |
| `cf-gears-doc-design` | `{workflow_doc_design}` | author/review DESIGN |
| `cf-gears-decompose` | `{workflow_decompose}` | author/review DECOMPOSITION |
| `cf-gears-doc-feature` | `{workflow_doc_feature}` | author/review FEATURE |
| `cf-gears-implement` | `{workflow_implement}` | code from a FEATURE artifact, with `@cpt-*` traceability |
| `cf-gears-coding` | `{workflow_coding}` | code directly from DESIGN/ADR/PRD/upstream context, no FEATURE/`@cpt-*` |
| `cf-gears-gear-creator` | `{workflow_gear_creator}` | scaffold a new gear or add a feature to an existing one (DDD-light layers, SDK, OperationBuilder, SecureORM); thin wrapper over the `gear-creator` skill |
| `cf-gears-change-impact-analysis` | `{workflow_change_impact_analysis}` | read-only downstream impact report for an upstream artifact change |
| `cf-gears-pr-review` | `{workflow_pr_review}` | SDLC-traceability + guardrail PR review (delegates to `cf-gears-toolkit-review` for its Rust/ToolKit pass) |
| `cf-gears-pr-status` | `{workflow_pr_status}` | PR status report |
| `cf-gears-init` | `{workflow_init}` | one-time (or re-run-as-needed) setup: discovers this project's real build/test commands and key paths, writes them into `{ci_commands}`/`{project_paths}` |
| `cf-gears-toolkit-review` | `{workflow_toolkit_review}` | idiomatic-Rust / ToolKit-framework conformance review (PR or local branch), standalone or via `cf-gears-pr-review`; thin wrapper over the `toolkit-pr-review` skill |

## Other kit resources

PR tooling, not tied to one artifact kind:

| `{variable}` | Resolves to | Used by |
|---|---|---|
| `{pr_code_review_template}` | `artifacts/PR-CODE-REVIEW-TEMPLATE/template.md` | `cf-gears-pr-review` output format |
| `{pr_status_report_template}` | `artifacts/PR-STATUS-REPORT-TEMPLATE/template.md` | `cf-gears-pr-status` output format |
| `{scripts}` | `scripts/` (directory) | `python3 {scripts}/pr.py`, `{scripts}/prompts/pr/*` |
| `{change_impact_config}` | `scripts/change-impact-analysis.toml` | `cf-gears-change-impact-analysis` thresholds |
| `{ci_commands}` | `scripts/ci-commands.toml` | `GearsCodeClose`/`GearsCodeChainSweep`'s CI/build command facade — edit this file (not `code-stage-router.md` or `docs/TESTING.md`) to repoint every gate command for a project whose build system doesn't match this repo's own Makefile; `cf-gears-init` does this for you |
| `{project_paths}` | `scripts/project-paths.toml` | `GearsCodeDispatchContext`/`codebase/rules.md`'s path facade — the canonical reference gear, the example-server registration surface, and the FIPS-relevant lib paths, for a project whose layout doesn't match this repo's own; `cf-gears-init` does this for you |
| `{skill_toolkit_pr_review}` | `.claude/skills/toolkit-pr-review/` (directory) | bundles the real `toolkit-pr-review` skill (its `SKILL.md` and any future sibling files) for standalone installs; this is exactly what `cf-gears-toolkit-review`'s single `LOAD .../FOLLOW` instruction resolves to |
| `{skill_gear_creator}` | `.claude/skills/gear-creator/` (directory) | bundles the real `gear-creator` skill (its `SKILL.md` and any future sibling files) for standalone installs; this is exactly what `cf-gears-gear-creator`'s single `LOAD .../FOLLOW` instruction resolves to |
| `{agent_toolkit_pr_review_architecture}` … `{agent_toolkit_pr_review_toolkit}` (7 resources) | `.claude/agents/toolkit-pr-review-{architecture,async,design,errors,security,tests,toolkit}.md` | the real definitions `toolkit-pr-review`'s 7 sub-agents dispatch to — bundled since a standalone install's own agent-discovery may not see them |

Shared chain infrastructure:

| `{variable}` | Resolves to | Role |
|---|---|---|
| `{agents}` | `AGENTS.md` | always-loaded kit session context (`public=true`) |
| `{constraints}` | `constraints.toml` | allowed ID kinds and downstream reference rules per artifact |
| `{gears_doc_stage_router}` | `modules/doc-stage-router.md` | author→validate→review→fix routing for doc presets |
| `{gears_code_stage_router}` | `modules/code-stage-router.md` | tests→author→validate→review→fix routing for code presets |
| `{gears_doc_phase}` | `plans/doc-phase.md` | phase-plan/phase-dod for doc presets, no separate planning session needed |
| `{gears_code_phase}` | `plans/code-phase.md` | same, for code presets |

60 kit-scoped variables in total (plus 6 Studio-system ones — `{cf-studio-path}`,
`{project_root}`, `{repo}`, `{base_dir}`, `{master_repo}`, `{cfs_cmd}` — that are not
kit resources; `{cfs_cmd}` resolves to the Studio CLI invocation via the root router's
own `CommandResolution`, same as every `{cfs_cmd} ...` command used throughout this doc).
Run `{cfs_cmd} resolve-vars --kit gears --flat` for the live, authoritative list.
