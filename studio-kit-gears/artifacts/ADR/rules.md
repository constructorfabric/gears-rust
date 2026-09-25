# ADR Rules

**Artifact**: ADR
**Kit**: gears

**Dependencies**:
- `{adr_template}` — structural reference
- `{adr_checklist}` — semantic quality criteria, review-only
- `{adr_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{adr_template}` for structure and section order
- [ ] Load the gear's `PRD.md`, `DESIGN*.md`, and existing `ADR/*.md` when they exist, to link affected IDs and avoid rewriting accepted decisions
- [ ] Leave `{adr_checklist}` and `{adr_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Fill the frontmatter `description` with one sentence naming what the document covers and for which gear; retrieval ranks documents on it
- [ ] Follow `{adr_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Generate the canonical ID as `cpt-{system}-adr-{slug}` and keep it stable
- [ ] Use valid Gears ADR IDs and status values from the template

### Semantic
- [ ] Compare at least two viable options unless the user explicitly records a constrained decision
- [ ] Record why the chosen option wins and why rejected options lose
- [ ] State decision drivers, decision scope, and review or supersession expectations
- [ ] Document performance, security, reliability, data, integration, operations, testing, compliance, UX, and business impact when applicable; otherwise state why not applicable
- [ ] Never hide tradeoffs, consequences, migration obligations, or reversibility notes
- [ ] Exclude complete architecture descriptions, product requirements, implementation tasks, schema definitions, code, secrets, test implementation, and operational runbooks

---

## Tasks

### Content Creation
- [ ] Capture context, options, decision, and consequences

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Link affected PRD, DESIGN, FEATURE, or UPSTREAM_REQS IDs when known
- [ ] When the gear's `DESIGN.md` exists, reference the new ADR ID from its Key ADRs section in the same change; the preset binds that section as the author stage's companion target, and `cfs validate --artifact` rejects an ADR that an existing DESIGN does not reference (`ref-missing-from-kind`)
- [ ] When the gear has no `DESIGN.md` yet, leave DESIGN untouched: validation then reports one expected `ref-target-not-in-scope` error for the DESIGN reference, which clears once `cf-gears-doc-design` authors DESIGN and references this ADR
- [ ] Preserve accepted ADR history; supersede with a new ADR instead of silently rewriting a final decision
- [ ] Re-check that the chosen `NNNN` is still free immediately before writing the file; if another ADR took it, use the next free number

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors

### Semantic
- [ ] No placeholders, TODOs, TBDs in critical sections, dangling references, or missing status fields

---

## Error Handling

### Recovery Options
- [ ] If structural validation fails, fix heading structure and the Table of Contents first
- [ ] If the decision revises an accepted ADR, author a superseding ADR instead of editing the accepted one

---

## Next Steps

### Options
- [ ] Reflect the decision in the gear DESIGN through `cf-gears-doc-design`
