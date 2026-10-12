# UPSTREAM_REQS Rules

**Artifact**: UPSTREAM_REQS
**Kit**: gears

**Dependencies**:
- `{upstream_reqs_template}` — structural reference
- `{upstream_reqs_checklist}` — semantic quality criteria, review-only
- `{upstream_reqs_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{upstream_reqs_template}` for structure and section order
- [ ] Load the code or documentation of each requesting gear that the requirements come from
- [ ] Leave `{upstream_reqs_checklist}` and `{upstream_reqs_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Fill the frontmatter `description` with one sentence naming what the document covers and for which gear; retrieval ranks documents on it
- [ ] Follow `{upstream_reqs_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Generate canonical IDs as `cpt-{system}-upreq-{slug}`, unique within the artifact
- [ ] Use valid upstream requirement IDs and priority markers from the template

### Semantic
- [ ] Capture source module needs as WHAT and WHY, not implementation HOW
- [ ] Name the requesting module, the future module boundary, and the reason for every requirement
- [ ] State an observable acceptance signal for each upstream requirement
- [ ] Make every requirement traceable back to concrete requesting gear code or documentation
- [ ] Exclude product vision, roadmap goals, implementation choices, internal algorithms, and crate-level design decisions

---

## Tasks

### Content Creation
- [ ] Collect the needs of each requesting gear toward the future gear
- [ ] Include a traceability section linking to the future PRD and DESIGN, even before those artifacts exist

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Preserve existing stable IDs; add new IDs only for new upstream requirements
- [ ] Keep downstream PRD, DESIGN, and FEATURE coverage references explicit when they exist

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors

### Semantic
- [ ] No placeholders, TODOs, TBDs, dangling references, or unprioritized requirements

---

## Error Handling

### Recovery Options
- [ ] If a requirement cannot be traced to a requesting gear, drop it or name the missing source
- [ ] If structural validation fails, fix heading structure and the Table of Contents first

---

## Next Steps

### Options
- [ ] Turn the upstream requirements into the gear PRD through `cf-gears-doc-prd`
