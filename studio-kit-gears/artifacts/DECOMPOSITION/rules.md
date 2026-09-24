# DECOMPOSITION Rules

**Artifact**: DECOMPOSITION
**Kit**: gears

**Dependencies**:
- `{decomposition_template}` — structural reference
- `{decomposition_checklist}` — semantic quality criteria, review-only
- `{decomposition_example}` — reference implementation, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load `{decomposition_template}` for structure and section order
- [ ] Load the gear's `DESIGN*.md` and `PRD.md`, plus `ADR/*.md` and `UPSTREAM_REQS.md` when they exist
- [ ] Leave `{decomposition_checklist}` and `{decomposition_example}` to the review stage; authoring does not load them

---

## Requirements

### Structural
- [ ] Follow `{decomposition_template}` structure and section order
- [ ] Keep an accurate Table of Contents that matches the final headings
- [ ] Generate canonical IDs as `cpt-{system}-status-{slug}` and `cpt-{system}-feature-{slug}` for the decomposition status and feature entries
- [ ] Use valid Gears feature IDs, statuses, and priority markers from the template
- [ ] Keep checkbox and status consistency: a parent is checked only when all nested referenced blocks are checked, and no checkbox ID repeats within a feature block

### Semantic
- [ ] Trace every feature candidate to PRD, DESIGN, ADR, or UPSTREAM_REQS IDs when those sources exist
- [ ] Provide 100 percent explicit coverage of required design elements and requirements passthrough
- [ ] List Requirements Covered, Design Components, Sequences, and Data for every feature, using `None` only when explicitly true
- [ ] Keep each feature independently implementable and testable where practical
- [ ] Never create orphan features with no upstream coverage or no clear definition of done
- [ ] Exclude implementation details, new requirement definitions, and architecture decisions

---

## Tasks

### Content Creation
- [ ] Split the design scope into cohesive FEATURE candidates with explicit dependencies
- [ ] Make dependencies, ordering constraints, and parallelization opportunities explicit

### IDs and Structure
- [ ] Generate the Table of Contents with `cfs toc <path>` once the final headings are in place
- [ ] Preserve existing stable IDs; add new IDs only for new feature candidates

---

## Validation

### Structural
- [ ] `cfs validate-toc <path>` passes (validation never rewrites the document)
- [ ] `cfs validate --artifact <path>` reports zero errors

### Semantic
- [ ] No placeholders, TODOs, TBDs, dangling references, or unprioritized features

---

## Error Handling

### Recovery Options
- [ ] If coverage is incomplete, add the missing design element to an existing feature or a new one rather than dropping it
- [ ] If structural validation fails, fix heading structure and the Table of Contents first

---

## Next Steps

### Options
- [ ] Specify the first feature through `cf-gears-doc-feature`
