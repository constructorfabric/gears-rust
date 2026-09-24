# FEATURE Rules

**Artifact**: FEATURE
**Kit**: gears

**Dependencies**:
- `{feature_template}` — structural reference
- `{feature_checklist}` — semantic quality criteria, review-only
- `{feature_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{feature_template}` for structure and section order
- [ ] Load the gear's `DECOMPOSITION.md` entry for this feature, plus `DESIGN*.md`, `PRD.md`, and relevant `ADR/*.md`
- [ ] Load the CDSL rules in `{cf-studio-path}/.core/architecture/specs/CDSL.md` before writing flows, algorithms, or states
- [ ] Leave `{feature_checklist}` and `{feature_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Follow `{feature_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Generate canonical IDs from the template patterns: featstatus, feature, flow, algo, state, and dod
- [ ] Put the `featstatus` checkbox and the DECOMPOSITION `feature` backreference directly under the H1 title
- [ ] Give every CDSL step line a checkbox, a phase token, and an `inst-*` ID that is unique within its flow or algorithm

### Semantic
- [ ] Write every CDSL step in plain, language-agnostic English: no programming operators, no function syntax, no type annotations
  - VALID: "**IF** the session lifecycle state is hard_deleted **RETURN** 404 Not Found"
  - INVALID: "**IF** session.lifecycle_state == 'hard_deleted' **RETURN** 404"
  - INVALID: "**RETURN** `Vec<Capability>`"
- [ ] Trace the FEATURE to DECOMPOSITION, DESIGN, PRD, ADR, or UPSTREAM_REQS IDs when those sources exist
- [ ] Preserve PRD coverage integrity and DESIGN principles, constraints, components, sequences, and data references
- [ ] Preserve SDK-first public contracts, domain/API/infrastructure separation, runtime-owned privileged access, and canonical API and error behavior when they apply
- [ ] Define testable acceptance criteria and deterministic completion signals
- [ ] Define security, reliability, data integrity, observability, rollback, test-layering, and compile-time-gate behavior when applicable; otherwise state why not applicable
- [ ] Document feature-local deviations from shared baselines with the deviation, rationale, and review owner
- [ ] Exclude new system-level type definitions, API endpoints, architecture decisions, product requirements, sprint tasks, code snippets, test implementation, infrastructure code, and secrets

---

## Tasks

### Content Creation
- [ ] Define flows, algorithms, states, data contracts, and the definition of done in CDSL
- [ ] Include implementation constraints that code must satisfy, without prescribing incidental code structure

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Keep the `featstatus` checkbox consistent with the flow, algorithm, state, and definition-of-done checkboxes
- [ ] Preserve existing stable IDs; add new IDs only for new feature or CDSL elements

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors, including the CDSL structure and clarity checks

### Semantic
- [ ] Every CDSL step can be implemented, tested, and traced
- [ ] No placeholders, TODOs, TBDs, dangling references, or unprioritized CDSL elements

---

## Error Handling

### Recovery Options
- [ ] If a CDSL clarity check fails, rewrite the step description in plain English and keep its checkbox, phase token, and `inst-*` ID unchanged
- [ ] If the DECOMPOSITION backreference does not resolve, fix the feature ID against `DECOMPOSITION.md`

---

## Next Steps

### Options
- [ ] Implement the feature through `cf-gears-implement`
