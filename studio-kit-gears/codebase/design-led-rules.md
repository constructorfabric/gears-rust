# CODE Rules (DESIGN-led)

**Artifact**: CODE
**Kit**: gears

**Dependencies**:
- the source DESIGN, ADR, PRD, UPSTREAM_REQS, or explicit design context — the implementation contract
- `{codebase_checklist}` — code review criteria, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load the source design context; stop and ask when it cannot be resolved from the request or the repository
- [ ] Leave `{codebase_checklist}` to the review stage; authoring does not load it

---

## Requirements

### Structural
- [ ] Follow the gear layout in `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md`: SDK crate for public contracts, gear crate for domain, API, and infrastructure
- [ ] Never add, require, remove, or rewrite `@cpt-*` markers for DESIGN-led work
- [ ] Never update FEATURE implementation checkboxes or statuses

### Semantic
- [ ] Preserve PRD outcomes and DESIGN principles, constraints, components, sequences, data contracts, and security requirements in code behavior when those sources exist
- [ ] Preserve SDK-first public contracts, domain/API/infrastructure separation, runtime-owned privileged access, canonical OperationBuilder and operation-registration behavior, canonical API and error behavior, and safe wire errors unless the design context documents an approved deviation
- [ ] Record review evidence for work touching registry, autodetect, or ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior, or other security-boundary logic: the touched guardrail or deviation, rationale, owner, and validation performed
- [ ] Add compile-fail tests when the code exposes compile-time guarantees and a compile-fail harness exists; otherwise state why the gate does not apply
- [ ] Never broaden scope beyond the source design context without an upstream artifact change or a user-approved scope change

---

## Tasks

### Content Creation
- [ ] Derive implementation slices from the design's boundaries, interfaces, sequences, data contracts, security requirements, and acceptance constraints; each slice names the design element, ADR, requirement, or user-supplied constraint it implements
- [ ] Order slices by dependency and user-observable behavior, each independently testable
- [ ] Implement one slice at a time with TDD: failing test first, smallest passing code, then refactor
- [ ] Preserve existing behavior outside the current slice and the requested design scope

### IDs and Structure
- [ ] Preserve existing stable IDs and markers; move markers only with the code they describe

---

## Validation

### Structural
- [ ] `make gear-ci GEAR=<gear>` (fmt, clippy with the workspace lint set, tests) and `make dylint` pass

### Semantic
- [ ] No test, lint, or build failure is left unresolved when the command is available

---

## Error Handling

### Recovery Options
- [ ] If the design context is ambiguous for a slice, stop and ask instead of choosing a design silently

---

## Next Steps

### Options
- [ ] Commit the slice through `cf-git-commit`
- [ ] Add FEATURE traceability later through `cf-gears-doc-feature` and `cf-gears-implement`
