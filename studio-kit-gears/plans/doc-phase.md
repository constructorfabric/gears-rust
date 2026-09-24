# Gears Document Phase

The standing phase plan and definition of done for one Gears SDLC document
(UPSTREAM_REQS, PRD, ADR, DESIGN, DECOMPOSITION, or FEATURE). The
`cf-gears-doc-*` presets supply this file to the Studio thin skills as the
`phase-plan` (shape `phase-plan-doc`) and `phase-dod` (shape `phase-dod-doc`)
prerequisites, so a single-artifact run does not need a separate planning
session. Multi-artifact or cross-gear work still belongs in
`cf-documenting-planning`.

<!-- toc -->

- [Phase Plan](#phase-plan)
  - [Scope](#scope)
  - [Prerequisites](#prerequisites)
  - [Skill Sequence](#skill-sequence)
  - [Expected Outputs](#expected-outputs)
- [Definition of Done](#definition-of-done)

<!-- /toc -->

## Phase Plan

### Scope

One phase: author or revise exactly one artifact of the bound KIND at the
resolved target path under `gears/<gear>/docs/`. The user's request fixes the
content scope; the upstream artifacts below fix what the document must cover.

### Prerequisites

| KIND | Upstream artifact the phase reads | Required |
|------|-----------------------------------|----------|
| UPSTREAM_REQS | code or docs of the requesting gears | yes |
| PRD | `UPSTREAM_REQS.md` | when it exists |
| ADR | `PRD.md` | yes |
| DESIGN | `PRD.md`, accepted `ADR/*.md` | `PRD.md` yes |
| DECOMPOSITION | `DESIGN*.md` | yes |
| FEATURE | `DECOMPOSITION.md` entry for the feature | yes |

### Skill Sequence

Each stage is one invocation of the KIND preset; the preset routes it to the
Studio thin skill and pins itself with the next stage.

| Stage | Studio skill | Exit |
|-------|--------------|------|
| author | `cf-documenting-gen` | document written |
| validate | `cf-documenting-ci` | gate pass → review; gate fail → author (revise with the CI findings) |
| review | `cf-documenting-review` | no findings → close; findings → fix |
| fix | `cf-documenting-fix` | fixes applied and re-validated → review |
| close | kit-owned | definition of done checked; next artifact in the chain offered |

### Expected Outputs

`doc-changes`, `deterministic-report`, `review-findings`, `phase-status`.

## Definition of Done

- [ ] The artifact exists at the resolved path and follows the KIND template section order.
- [ ] `cfs toc <path>` has been applied and `cfs validate-toc <path>` passes.
- [ ] `cfs validate --artifact <path>` reports zero errors.
- [ ] Every upstream ID the KIND must cover is covered (see the KIND `rules.md`).
- [ ] The latest semantic review against the KIND checklist has no unresolved CRITICAL or MAJOR findings.
- [ ] Any remaining MINOR findings are listed in the close report, not silently dropped.
