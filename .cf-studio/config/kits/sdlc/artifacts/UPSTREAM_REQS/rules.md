# UPSTREAM_REQS Rules

**Artifact**: UPSTREAM_REQS
**Kit**: sdlc

**Dependencies**:
- `{upstream_reqs_template}` — structural reference
- `{upstream_reqs_checklist}` — semantic quality criteria

## Table of Contents

1. [Prerequisites](#prerequisites)
   - [Load Dependencies](#load-dependencies)
2. [Requirements](#requirements)
   - [Structural](#structural)
   - [Versioning](#versioning)
   - [Semantic](#semantic)
   - [Traceability](#traceability)
   - [Constraints](#constraints)
   - [Deliberate Omissions (MUST NOT HAVE)](#deliberate-omissions-must-not-have)
3. [Tasks](#tasks)
   - [Phase 1: Setup](#phase-1-setup)
   - [Phase 2: Content Creation](#phase-2-content-creation)
   - [Phase 3: IDs and Structure](#phase-3-ids-and-structure)
   - [Phase 4: Quality Check](#phase-4-quality-check)
   - [Phase 5: Table of Contents](#phase-5-table-of-contents)
4. [Validation](#validation)
   - [Phase 1: Structural Validation (Deterministic)](#phase-1-structural-validation-deterministic)
   - [Phase 2: Semantic Validation (Checklist-based)](#phase-2-semantic-validation-checklist-based)
   - [Phase 3: Validation Report](#phase-3-validation-report)
   - [Phase 4: Table of Contents Validation](#phase-4-table-of-contents-validation)
5. [Error Handling](#error-handling)
   - [Missing Dependencies](#missing-dependencies)
   - [Escalation](#escalation)
   - [Missing Config](#missing-config)
6. [Next Steps](#next-steps)
   - [Options](#options)

---

## Prerequisites

### Load Dependencies

- [ ] Load `{upstream_reqs_template}` for structure
- [ ] Load `{upstream_reqs_checklist}` for semantic guidance
- [ ] Read project config for ID prefix
- [ ] Load `{cf-studio-path}/.core/architecture/specs/traceability.md` for ID formats
- [ ] Load `{constraints}` for kit-level constraints
- [ ] Load `{cf-studio-path}/.core/architecture/specs/kit/constraints.md` for constraints specification

---

## Requirements

### Structural

- [ ] UPSTREAM_REQS follows `{upstream_reqs_template}` structure
- [ ] Artifact frontmatter (optional): use `cpt:` format for document metadata
- [ ] All required sections present and non-empty
- [ ] All IDs follow `cpt-{system}-upreq-{slug}` convention
- [ ] All requirements have priority markers (`p1`–`p9`)
- [ ] No placeholder content (TODO, TBD, FIXME)
- [ ] No duplicate IDs within document

### Versioning

- [ ] When editing existing UPSTREAM_REQS that has frontmatter: increment version in frontmatter
- [ ] Keep changelog of significant changes

### Semantic

- [ ] Purpose MUST explain what future module is needed and what gap it fills
- [ ] Purpose MUST be ≤ 2 paragraphs
- [ ] Requesting modules table MUST list at least one module
- [ ] Each requesting module MUST exist in the codebase
- [ ] Each requesting module MUST have a clear justification
- [ ] Requirements MUST be grouped by requesting module
- [ ] Every requirement MUST use observable behavior language (MUST, MUST NOT, SHALL)
- [ ] Every requirement MUST describe WHAT is needed, not HOW to implement it
- [ ] Every requirement MUST have a unique ID: `cpt-{system}-upreq-{slug}`
- [ ] Every requirement MUST have a priority marker (`p1`–`p9`)
- [ ] Every requirement MUST have a rationale
- [ ] Every requirement MUST reference its source module
- [ ] Each requesting module's documentation (PRD or DESIGN) MUST reference the `upreq` IDs it contributed
- [ ] Priority summary table MUST be consistent with individual requirement priorities
- [ ] No placeholder content (TODO, TBD, FIXME)
- [ ] No duplicate IDs within document

### Traceability

- [ ] When PRD is created for this module → PRD MUST trace back to UPSTREAM_REQS
- [ ] When PRD exists: all `upreq` IDs MUST be addressed in the PRD (covered or explicitly excluded with reasoning)
- [ ] Requesting modules' docs MUST reference the `upreq` IDs they contributed

### Constraints

- [ ] ALWAYS open and follow `{constraints}` (kit root)
- [ ] Treat `constraints.toml` as primary validator for:
  - where IDs are defined
  - where IDs are referenced
  - which cross-artifact references are required / optional / prohibited

**References**:
- `{cf-studio-path}/.core/requirements/kit-constraints.md`
- `{cf-studio-path}/.core/schemas/kit-constraints.schema.json`

### Deliberate Omissions (MUST NOT HAVE)

UPSTREAM_REQS must NOT contain the following — report as violation if found:

- **ARCH-UPREQ-NO-001**: No Implementation Details (CRITICAL) — no API specs, data schemas, architecture decisions, code examples
- **BIZ-UPREQ-NO-001**: No Product Vision (HIGH) — no business goals, success criteria, market analysis, user personas

---

## Tasks

### Phase 1: Setup

- [ ] Load `{upstream_reqs_template}` for structure
- [ ] Load `{upstream_reqs_checklist}` for semantic guidance
- [ ] Read project config for ID prefix
- [ ] Identify requesting modules and their needs

### Phase 2: Content Creation

- [ ] Write Overview: purpose and requesting modules table
- [ ] For each requesting gear: document requirements with rationale and source
- [ ] Write Priorities summary table
- [ ] Write Traceability section

### Phase 3: IDs and Structure

- [ ] Generate requirement IDs: `cpt-{system}-upreq-{slug}`
- [ ] Assign priorities based on requesting modules' needs
- [ ] Verify uniqueness with `cypilot list-ids`

### Phase 4: Quality Check

- [ ] Self-review against `{upstream_reqs_checklist}` MUST HAVE items
- [ ] Ensure no MUST NOT HAVE violations
- [ ] Verify each requesting module's docs reference the contributed `upreq` IDs

### Phase 5: Table of Contents

- [ ] Run `cypilot toc <artifact-file>` to generate/update Table of Contents
- [ ] Verify TOC is present and complete with `cypilot validate-toc <artifact-file>`

---

## Validation

### Phase 1: Structural Validation (Deterministic)

- [ ] Run `cypilot validate --artifact <path>` for:
  - Template structure compliance
  - ID format validation
  - Priority markers present
  - No placeholders
  - No duplicate IDs

### Phase 2: Semantic Validation (Checklist-based)

- [ ] Read `{upstream_reqs_checklist}` in full
- [ ] For each MUST HAVE item: check if requirement is met
  - If not met: report as violation with severity
- [ ] For each MUST NOT HAVE item: scan document for violations

### Phase 3: Validation Report

```
UPSTREAM_REQS Validation Report
════════════════════════════════

Structural: PASS/FAIL
Semantic: PASS/FAIL (N issues)

Issues:
- [SEVERITY] CHECKLIST-ID: Description
```

### Phase 4: Table of Contents Validation

- [ ] Table of Contents section exists (`## Table of Contents` or `<!-- toc -->` markers)
- [ ] All TOC anchors point to actual headings in the document
- [ ] All headings are represented in the TOC
- [ ] TOC order matches document heading order
- [ ] Run `cypilot validate-toc <artifact-file>` — must report PASS

---

## Error Handling

### Missing Dependencies

- [ ] If `{upstream_reqs_template}` cannot be loaded → STOP, cannot proceed without template
- [ ] If `{upstream_reqs_checklist}` cannot be loaded → STOP, cannot proceed without semantic validation checklist

### Escalation

- [ ] Ask user when requesting module's needs are unclear or contradictory
- [ ] Ask user when uncertain whether a requirement belongs here or in PRD

### Missing Config

- [ ] If project config unavailable → use default project prefix `cpt-{dirname}`
- [ ] Ask user to confirm or provide custom prefix

---

## Next Steps

### Options

- [ ] UPSTREAM_REQS complete → `/cypilot-generate PRD` — create PRD for the new gear (tracing back to UPSTREAM_REQS)
- [ ] Need to add more requesting modules → continue editing UPSTREAM_REQS
- [ ] Want checklist review only → `/cypilot-analyze semantic` — semantic validation
