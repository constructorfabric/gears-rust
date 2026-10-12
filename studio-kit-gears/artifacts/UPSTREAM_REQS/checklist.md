# UPSTREAM_REQS Expert Checklist

**Artifact**: Upstream Requirements
**Version**: 1.0
**Last Updated**: 2026-03-19
**Purpose**: Quality checklist for UPSTREAM_REQS artifacts

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Severity Dictionary](#severity-dictionary)
3. [MUST HAVE](#must-have)
   - [SOURCE Expertise (SRC)](#source-expertise-src)
   - [REQUIREMENTS Expertise (REQ)](#requirements-expertise-req)
   - [PRIORITY Expertise (PRI)](#priority-expertise-pri)
   - [DOC (DOC)](#doc-doc)
4. [MUST NOT HAVE](#must-not-have)
5. [Validation Summary](#validation-summary)
   - [Final Checklist](#final-checklist)
   - [Reporting](#reporting)

---

## Prerequisites

Before starting the review, confirm:

- [ ] I understand this checklist validates UPSTREAM_REQS artifacts
- [ ] I will check ALL items in MUST HAVE sections
- [ ] I will verify ALL items in MUST NOT HAVE sections
- [ ] I will document any violations found
- [ ] I will provide specific feedback for each failed check

---

## Severity Dictionary

- **CRITICAL**: Blocks downstream PRD/DESIGN work or loses requirements.
- **HIGH**: Major ambiguity; should be fixed before the gear is designed.
- **MEDIUM**: Meaningful improvement; fix when feasible.
- **LOW**: Minor improvement; optional.

---

# MUST HAVE

---

## SOURCE Expertise (SRC)

### SRC-UPREQ-001: Requesting Gears Identified
**Severity**: CRITICAL

- [ ] At least one requesting gear is listed
- [ ] Each requesting gear exists in the codebase
- [ ] Each requesting gear has a clear justification for why it needs this future gear
- [ ] No vague justifications (e.g., "might be useful" is insufficient)

### SRC-UPREQ-002: Source Traceability
**Severity**: HIGH

- [ ] Each requirement references its source gear
- [ ] Source gear path is valid (`gears/{name}`)
- [ ] Requirements can be traced back to concrete needs in the requesting gear's code or documentation

### SRC-UPREQ-003: Backward References
**Severity**: HIGH

- [ ] Each requesting gear's documentation (PRD or DESIGN) references the `upreq` IDs it contributed
- [ ] References use the canonical `cpt-{system}-upreq-{slug}` format

---

## REQUIREMENTS Expertise (REQ)

### REQ-UPREQ-001: Requirements Clarity
**Severity**: CRITICAL

- [ ] Each requirement uses observable behavior language (MUST, MUST NOT, SHALL)
- [ ] Each requirement describes WHAT is needed, not HOW to implement it
- [ ] Each requirement is specific enough to be verifiable
- [ ] No placeholder content (TODO, TBD, FIXME)

### REQ-UPREQ-002: Requirements Completeness
**Severity**: HIGH

- [ ] All requesting gears have at least one requirement
- [ ] Requirements cover the requesting gear's actual needs (not a subset)
- [ ] No duplicate requirements across requesting gears (consolidate shared needs)

### REQ-UPREQ-003: ID Format and Uniqueness
**Severity**: CRITICAL

- [ ] All IDs follow `cpt-{system}-upreq-{slug}` format
- [ ] All IDs are unique within the document
- [ ] All IDs have priority markers (`p1`-`p9`)

---

## PRIORITY Expertise (PRI)

### PRI-UPREQ-001: Priority Assignment
**Severity**: HIGH

- [ ] Every requirement has a priority assigned
- [ ] Priority summary table is consistent with individual requirement priorities
- [ ] At least one requirement exists (artifact must not be empty)

---

## DOC (DOC)

### DOC-UPREQ-001: Structure Compliance
**Severity**: HIGH

- [ ] Document follows the UPSTREAM_REQS template structure
- [ ] All required sections present and non-empty
- [ ] Table of Contents is present and accurate

### DOC-UPREQ-002: Traceability Section
**Severity**: MEDIUM

- [ ] Traceability section links to future PRD and DESIGN (even if not yet created)

---

# MUST NOT HAVE

### ARCH-UPREQ-NO-001: No Implementation Details
**Severity**: CRITICAL

UPSTREAM_REQS captures WHAT is needed, not HOW. The following must NOT appear:

- [ ] No API specifications or interface contracts (belongs in DESIGN)
- [ ] No data schemas or database tables (belongs in DESIGN)
- [ ] No architecture decisions (belongs in ADR)
- [ ] No code examples or pseudocode (belongs in FEATURE/CODE)

### BIZ-UPREQ-NO-001: No Product Vision
**Severity**: HIGH

UPSTREAM_REQS captures specific gear requirements, not business vision:

- [ ] No business goals or success criteria (belongs in PRD)
- [ ] No market analysis or competitive positioning (belongs in PRD)
- [ ] No user personas beyond requesting gears (belongs in PRD)

---

# Validation Summary

## Final Checklist

- [ ] All MUST HAVE items checked
- [ ] All MUST NOT HAVE items verified
- [ ] All violations documented with severity

## Reporting

Report **only** problems (do not list what is OK).

For each issue include:

- **Checklist Item**: `{CHECKLIST-ID}` — {title}
- **Severity**: CRITICAL|HIGH|MEDIUM|LOW
- **Issue**: What is wrong
- **Evidence**: Quote or "No mention found"
- **Proposal**: Concrete fix
