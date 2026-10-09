# CF/Gears Kit (Constructor Studio-compatible)

**ID**: `gears`
**Format**: `Constructor Studio` (targets Studio v1.7.0 or later)
**Purpose**: Own CF/Gears’s SDLC document templates, examples, and expert checklists (`artifacts/<KIND>/`), and the Studio workflows that author, validate, and review gear documents and code.

## Artifact kinds

| Kind | Template | Rules | Checklist |
|------|----------|-------|-----------|
| UPSTREAM_REQS | `artifacts/UPSTREAM_REQS/template.md` | `artifacts/UPSTREAM_REQS/rules.md` | `artifacts/UPSTREAM_REQS/checklist.md` |
| PRD | `artifacts/PRD/template.md` | `artifacts/PRD/rules.md` | `artifacts/PRD/checklist.md` |
| ADR | `artifacts/ADR/template.md` | `artifacts/ADR/rules.md` | `artifacts/ADR/checklist.md` |
| DESIGN | `artifacts/DESIGN/template.md` | `artifacts/DESIGN/rules.md` | `artifacts/DESIGN/checklist.md` |
| DECOMPOSITION | `artifacts/DECOMPOSITION/template.md` | `artifacts/DECOMPOSITION/rules.md` | `artifacts/DECOMPOSITION/checklist.md` |
| FEATURE | `artifacts/FEATURE/template.md` | `artifacts/FEATURE/rules.md` | `artifacts/FEATURE/checklist.md` |
| CODE | — | `codebase/rules.md`, `codebase/design-led-rules.md` | `codebase/checklist.md` |

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

## Workflows

- `cf-gears-doc-upstream-reqs`, `cf-gears-doc-prd`, `cf-gears-doc-adr`,
  `cf-gears-doc-design`, `cf-gears-decompose`, `cf-gears-doc-feature`: author
  and review one SDLC document.
- `cf-gears-implement`: implement from a FEATURE artifact with `@cpt-*`
  traceability.
- `cf-gears-coding`: implement directly from DESIGN/ADR/PRD/upstream design
  context without FEATURE or `@cpt-*` implementation traceability.
- `cf-gears-change-impact-analysis`: read-only downstream impact report for an
  upstream artifact change.
- `cf-gears-pr-review`, `cf-gears-pr-status`: pull request review and status
  reports.
