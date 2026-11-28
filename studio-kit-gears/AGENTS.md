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

## Consistency Policy

Consistency across the chain is the pipeline's most important property — more
important than finishing any single artifact in isolation. Every authoring and
review stage checks the artifact being worked on against its immediate upstream
(FEATURE against DESIGN+DECOMPOSITION+PRD, DECOMPOSITION against DESIGN, DESIGN
against PRD+ADR, CODE against DESIGN/FEATURE); see each artifact's own
`checklist.md` for the exact bullets. An ADR added after DESIGN already exists
must be checked against that DESIGN for contradiction, not just linked to it
(`artifacts/ADR/rules.md`, `artifacts/ADR/checklist.md`'s SEM-ADR-001). Closing
the last FEATURE of a gear's DECOMPOSITION additionally sweeps the whole
realized chain once — not just that one FEATURE against DESIGN as authored, but
every already-closed FEATURE against DESIGN as it stands now
(`modules/code-stage-router.md`'s `GearsCodeChainSweep`) — because per-artifact
checks alone only ever caught drift one pair at a time, never a DESIGN
amendment made for a later FEATURE silently invalidating an earlier one's
already-closed slice. Both mechanisms close out gears-rust#5195 ("Fix drift
across chain artifacts"). Never report an artifact or a slice done while a
known contradiction with another artifact in the chain is unresolved.

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
