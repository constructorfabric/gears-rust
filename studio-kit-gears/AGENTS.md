# Constructor Studio Kit: Gears (`gears`)

Agent quick reference.

## What it is

Artifact-first SDLC pipeline (PRD → ADR + DESIGN → DECOMPOSITION → FEATURE → CODE) with templates, review-only checklists/examples, and embedded workflow rules for deterministic validation + traceability.

## Artifact kinds

| Kind | Semantic intent (when to use) | References |
| --- | --- | --- |
| PRD | Product intent: actors + problems + FR/NFR + use cases + success criteria. | `{prd_template}`, `{prd_checklist}`, `{prd_example}` |
| ADR | Decision log: why an architecture choice was made (context/options/decision/consequences). | `{adr_template}`, `{adr_checklist}`, `{adr_example}` |
| DESIGN | System design: architecture, components, boundaries, interfaces, drivers, principles/constraints. | `{design_template}`, `{design_checklist}`, `{design_example}` |
| DECOMPOSITION | Executable plan: FEATURE list, ordering, dependencies, and coverage links back to PRD/DESIGN. | `{decomposition_template}`, `{decomposition_checklist}`, `{decomposition_example}` |
| FEATURE | Precise behavior + DoD: CDSL flows/algos/states + test scenarios for implementability. | `{feature_template}`, `{feature_checklist}`, `{feature_example}` |
| UPSTREAM_REQS | Seed artifact: requirements from existing modules toward a future module that does not exist yet. PRD must trace back to these. | `{upstream_reqs_template}`, `{upstream_reqs_checklist}` |
| CODE | Implementation of FEATURE with optional `@cpt-*` markers and checkbox cascade/coverage validation. | `{codebase_checklist}` |
