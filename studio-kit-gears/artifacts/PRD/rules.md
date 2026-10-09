# PRD Rules

**Artifact**: PRD
**Kit**: gears

**Dependencies**:
- `{prd_template}` — structural reference
- `{prd_checklist}` — semantic quality criteria, review-only
- `{prd_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{prd_template}` for structure and section order
- [ ] Load the gear's `UPSTREAM_REQS.md` when it exists; every upstream requirement must be covered
- [ ] Leave `{prd_checklist}` and `{prd_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Fill the frontmatter `description` with one sentence naming what the document covers and for which gear; retrieval ranks documents on it
- [ ] Follow `{prd_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Give every requirement-like item a stable canonical CPT ID from the template patterns: actor, fr, nfr, interface, contract, and usecase
- [ ] Use valid Gears PRD IDs and priority markers from the template

### Semantic
- [ ] Author requirements as WHAT and WHY, not implementation HOW
  - VALID: "The gear returns the submitted message together with the caller's tenant"
  - INVALID: "The handler deserializes the body with serde and calls `ctx.tenant_id()`"
- [ ] Make functional and non-functional requirements observable or measurable
- [ ] Define actors, user journeys, public interfaces, and success criteria when relevant
- [ ] State non-applicability with a reason for every omitted critical domain
- [ ] Cover every UPSTREAM_REQS ID with at least one FR or NFR through a `Covers` field when an UPSTREAM_REQS document exists
- [ ] Exclude implementation tasks, architecture decisions, schema definitions, API specs, test cases, infrastructure specs, security implementation details, and code-level documentation

---

## Tasks

### Content Creation
- [ ] Capture the gear's purpose, actors, and scope boundaries from the request and upstream context
- [ ] Write FR and NFR entries with priorities, each observable or measurable
- [ ] Describe public interfaces and integration contracts at the product level
- [ ] Write use cases and acceptance criteria that a reviewer can check

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Generate IDs following the template's `cpt-{system}-{kind}-{slug}` patterns
- [ ] Preserve existing stable IDs; add new IDs only for new requirements
- [ ] Link covered UPSTREAM_REQS IDs when upstream requirements exist

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors

### Semantic
- [ ] No placeholders, TODOs, TBDs in critical sections, dangling references, or unprioritized requirements
- [ ] `constraints.toml` is followed through deterministic validation, never restated in the document

---

## Error Handling

### Recovery Options
- [ ] If structural validation fails, fix heading structure and the Table of Contents first
- [ ] If ID validation fails, check the ID pattern against the template and uniqueness within the gear

---

## Next Steps

### Options
- [ ] Continue with the gear DESIGN through `cf-gears-doc-design`
- [ ] Record significant decisions through `cf-gears-doc-adr`
