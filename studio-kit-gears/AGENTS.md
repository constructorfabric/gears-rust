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
- CODE implements FEATURE scope and traceability when required.

## Loading Policy

Generation should enter through the matched Gears workflow. This file is only
always-loaded kit context; do not duplicate workflow-specific rules here.
