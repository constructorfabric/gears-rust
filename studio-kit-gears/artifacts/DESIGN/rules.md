# DESIGN Rules

**Artifact**: DESIGN
**Kit**: gears

**Dependencies**:
- `{design_template}` — structural reference
- `{design_checklist}` — semantic quality criteria, review-only
- `{design_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{design_template}` for structure and section order
- [ ] Load the gear's `PRD.md`, accepted `ADR/*.md`, and `UPSTREAM_REQS.md` when they exist
- [ ] Leave `{design_checklist}` and `{design_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Follow `{design_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Generate canonical CPT IDs from the template patterns: design, tech, principle, constraint, entity, component, interface, seq, db, dbtable, and topology
- [ ] Use valid Gears DESIGN IDs and priority markers from the template

### Semantic
- [ ] Trace design elements to PRD, ADR, and UPSTREAM_REQS IDs when those sources exist
- [ ] Preserve PRD intent and scope; document any deviation as an explicit approved scope change
- [ ] Cover referenced ADR decisions and keep ADR and PRD links valid
- [ ] Define ownership boundaries, public interfaces, lifecycle and state behavior, and error surfaces when relevant
- [ ] Preserve SDK-first public contracts, domain/API/infrastructure separation, and runtime-owned privileged access unless an approved design deviation says otherwise
- [ ] Define REST contract metadata, canonical OperationBuilder and operation-registration behavior, canonical Problem (RFC 9457) error envelopes, and safe wire-error behavior when the gear exposes HTTP APIs
- [ ] Document security boundaries, data protection, fault tolerance, observability, testability, and compliance posture when applicable; otherwise state why not applicable
- [ ] Document deviations from shared platform, security, API, testing, and architecture baselines with the deviation, rationale, and review owner
- [ ] Exclude spec-level details, decision debates, product requirements, implementation tasks, code snippets, infrastructure code, test code, schema implementation, API specs, and secrets

---

## Tasks

### Content Creation
- [ ] Model the gear architecture, boundaries, interfaces, data and control flow, and operational behavior
- [ ] Call out assumptions, constraints, dependencies, and migration impacts

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Generate IDs following the template's `cpt-{system}-{kind}-{slug}` patterns
- [ ] Preserve existing stable IDs; add new IDs only for new design elements

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors

### Semantic
- [ ] No placeholders, TODOs, TBDs in critical sections, dangling references, or unprioritized design elements

---

## Error Handling

### Recovery Options
- [ ] If structural validation fails, fix heading structure and the Table of Contents first
- [ ] If a PRD or ADR reference does not resolve, fix the reference or record the missing upstream artifact

---

## Next Steps

### Options
- [ ] Split the design into features through `cf-gears-decompose`
- [ ] Implement directly from the design through `cf-gears-coding` when no FEATURE traceability is required
