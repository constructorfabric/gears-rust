# CF/Gears Kit (Constructor Studio-compatible)

**ID**: `gears`
**Format**: `Constructor Studio`
**Purpose**: Use CF/Gears’s documentation templates (`docs/spec-templates/*`) and expert checklists (`docs/checklists/*`).

## Artifact kinds

| Kind | Template source | Checklist source |
|------|------------------|------------------|
| PRD | `docs/spec-templates/PRD.md` | `docs/checklists/PRD.md` |
| DESIGN | `docs/spec-templates/DESIGN.md` | `docs/checklists/DESIGN.md` |
| ADR | `docs/spec-templates/ADR.md` | `docs/checklists/ADR.md` |
| FEATURE | `docs/spec-templates/FEATURE.md` | `docs/checklists/FEATURE.md` |
| DECOMPOSITION | `docs/spec-templates/DECOMPOSITION.md` | `docs/checklists/DECOMPOSITION.md` |

## Coding workflows

- `cf-gears-implement`: implement from a FEATURE artifact with traceability when required.
- `cf-gears-coding`: implement directly from DESIGN/ADR/PRD/upstream design context without FEATURE or `@cpt-*` implementation traceability.
