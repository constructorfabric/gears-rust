# Constructor Studio Kit: Gears (`gears`)

Compact session context. Detailed generation, review, validation, PR, and
traceability rules live in the matched workflow files and templates.

## Artifact Chain

`UPSTREAM_REQS -> PRD -> ADR + DESIGN -> DECOMPOSITION -> FEATURE -> CODE`

Use this chain as orientation when resolving upstream/downstream context:

- UPSTREAM_REQS captures requirements from existing modules toward a future module.
- PRD turns upstream needs into product requirements.
- ADR records significant architecture decisions.
- DESIGN maps requirements and decisions into system structure.
- DECOMPOSITION splits design scope into implementable FEATUREs.
- FEATURE defines implementation-ready behavior.
- CODE implements FEATURE scope and traceability when required, or implements
  directly from DESIGN/ADR/PRD context when no FEATURE exists.

## Loading Policy

Generation should enter through the matched Gears workflow. This file is only
always-loaded kit context; do not duplicate workflow-specific rules here.

## Shared Baseline Policy

Project-wide platform, security, API, testing, and architecture standards
belong in foundational repository docs and shared kit assets. Gear-level
PRD/DESIGN/FEATURE artifacts should document only local deviations, extensions,
or stricter constraints.

When a gear deviates from a shared baseline, document the deviation, rationale,
and review owner in the affected artifact or review record.
