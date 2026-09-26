# Gears Code Phase

The standing phase plan and definition of done for implementing one slice of
Gears code, either from a FEATURE (`cf-gears-implement`, with `@cpt-*`
traceability) or from DESIGN context (`cf-gears-coding`). The presets supply
this file to the Studio thin skills as the `phase-plan` (shape
`phase-plan-doc`) and `phase-dod` (shape `phase-dod-doc`) prerequisites.

<!-- toc -->

- [Phase Plan](#phase-plan)
  - [Scope](#scope)
  - [Relevant Files](#relevant-files)
  - [Skill Sequence](#skill-sequence)
  - [Expected Outputs](#expected-outputs)
- [Definition of Done](#definition-of-done)

<!-- /toc -->

## Phase Plan

### Scope

One phase per implementation slice. The source FEATURE (or DESIGN) fixes the
behavior; the slice names the FEATURE CDSL IDs (or design elements) it
implements. Work outside the slice waits for the next phase.

### Relevant Files

The canonical gear layout from
`docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md` is the
`relevant-files-map`:

| Path | Holds |
|------|-------|
| `gears/<gear>/<gear>-sdk/` | public contracts: client trait, models, errors |
| `gears/<gear>/<gear>/src/domain/` | domain logic |
| `gears/<gear>/<gear>/src/api/` | REST handlers, DTOs, OperationBuilder registration |
| `gears/<gear>/<gear>/src/infra/` | storage and external adapters |
| `gears/<gear>/<gear>/tests/` | integration tests |
| workspace `Cargo.toml`, example server, feature flags | registration surface for a new gear |

### Skill Sequence

| Stage | Studio skill | Exit |
|-------|--------------|------|
| tests | `cf-coding-tests` | failing tests for the slice exist |
| author | `cf-coding-gen` | slice implemented |
| validate | `cf-coding-ci` | gate pass → review; gate fail → author (fix with the CI findings) |
| review | `cf-coding-review` | no findings → close; findings → fix |
| fix | `cf-coding-fix` | fixes applied and re-validated → review |
| close | kit-owned | definition of done checked; next slice or next step offered |

The deterministic gate for the slice is the gear-scoped CI lane plus the
architecture lints: `make gear-ci GEAR=<gear>` (fmt, clippy with the workspace
lint set, tests) and `make dylint`, and, for FEATURE-led work, `cfs validate`.

### Expected Outputs

`unit-tests`, `code-changes`, `deterministic-report`, `review-findings`,
`phase-status`.

## Definition of Done

- [ ] Tests for the slice's behavior exist and pass.
- [ ] `make gear-ci GEAR=<gear>` and `make dylint` pass.
- [ ] For FEATURE-led work: `cfs validate` reports zero errors and the FEATURE checkboxes reflect what is implemented; when the traceability mode is FULL, every implemented CDSL ID also has `@cpt-*` markers.
- [ ] The latest code review against `docs/checklists/CODING.md` has no unresolved CRITICAL or MAJOR findings.
- [ ] Remaining MINOR findings are listed in the close report.
