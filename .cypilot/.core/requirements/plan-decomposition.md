---
cypilot: true
type: requirement
name: Plan Decomposition Strategies
version: 1.0
purpose: Define how to split tasks into phases by type — generate, analyze, implement
---

# Plan Decomposition Strategies

<!-- toc -->

- [Plan Decomposition Strategies](#plan-decomposition-strategies)
  - [Overview](#overview)
  - [Strategy Selection](#strategy-selection)
  - [Strategy 1: Generate (by Template Sections)](#strategy-1-generate-by-template-sections)
  - [Strategy 2: Analyze (by Checklist Categories)](#strategy-2-analyze-by-checklist-categories)
  - [Strategy 3: Implement (by CDSL Blocks)](#strategy-3-implement-by-cdsl-blocks)
  - [Budget Enforcement](#budget-enforcement)
  - [Execution Context Prediction](#execution-context-prediction)
    - [Estimation Formula](#estimation-formula)
    - [Execution Context Budget](#execution-context-budget)
    - [Auto-Split on Predicted Overflow](#auto-split-on-predicted-overflow)
    - [Example: Overflow Detection](#example-overflow-detection)
    - [Integration with Decomposition](#integration-with-decomposition)
  - [Phase Dependencies](#phase-dependencies)
  - [Single-Context Bypass](#single-context-bypass)

<!-- /toc -->

---

## Overview

When the plan workflow decomposes a task into phases, it MUST select one of three strategies based on the task type. Each strategy defines how to split the work into independent, self-contained units that fit within the line budget.

**Core principle**: Each phase MUST be independently executable. An agent reading a phase file MUST be able to complete it without knowledge of other phases (except the explicit Prior Context summary).

---

## Strategy Selection

| Task Type | Trigger | Strategy |
|-----------|---------|----------|
| `generate` | User requests creation or update of an artifact (PRD, DESIGN, FEATURE, ADR, DECOMPOSITION) | Split by template sections |
| `analyze` | User requests validation, review, or audit of an artifact or codebase | Split by checklist categories |
| `implement` | User requests code implementation from a FEATURE spec | Split by CDSL blocks |

**Detection rules**:
- If the task mentions "create", "generate", "write", "update", or "draft" → `generate`
- If the task mentions "validate", "review", "check", "audit", or "analyze" → `analyze`
- If the task mentions "implement", "code", "build", or "develop" → `implement`
- If ambiguous, ask the user to clarify

---

## Strategy 1: Generate (by Template Sections)

**Applies to**: Creating or updating artifacts using kit templates.

**How to decompose**:

1. Load the template for the target artifact kind (e.g., `{prd_template}`, `{design_template}`)
2. Identify all H2 sections in the template
3. Group adjacent sections into phases of 2-4 sections each
4. Each phase creates or updates one group of sections

**Grouping rules**:

- Group sections that share data dependencies (e.g., "Actors" before "Use Cases" that reference actors)
- Keep the first group small (1-2 sections) so the agent establishes the file structure
- Keep the last group small (1-2 sections) for final synthesis sections (Acceptance Criteria, Dependencies)
- If a single section would exceed 300 lines when compiled, give it its own phase

**Phase structure**:

| Phase | Typical Content | Input |
|-------|----------------|-------|
| 1 | Frontmatter + Overview + Problem/Purpose (sections 1-2) | Template, project context |
| 2 | Core content sections (sections 3-5) | Template, Phase 1 output summary |
| 3 | Detail sections (sections 6-8) | Template, Phase 1-2 output summary |
| N | Synthesis + Acceptance Criteria (final sections) | Template, all prior phase summaries |

**Example — generating a PRD (8 sections → 4 phases)**:

```
Phase 1: Frontmatter, Overview, Problem Statement (sections 1-2)
Phase 2: Goals, Actors, Operational Concept (sections 3-5)
Phase 3: Functional Requirements, Non-Functional Requirements (sections 6-7)
Phase 4: Use Cases, Acceptance Criteria, Dependencies (sections 8-10)
```

---

## Strategy 2: Analyze Artifacts (by Checklist Categories)

**Applies to**: Validating, reviewing, or auditing **artifacts** (PRD, DESIGN, FEATURE, ADR, DECOMPOSITION).

> **Note**: For **codebase** analysis, see Strategy 2b below.

**How to decompose**:

1. Load the checklist for the target artifact kind (e.g., `{prd_checklist}`, `{design_checklist}`)
2. Identify checklist categories (typically grouped by H2 or H3 headings)
3. Group categories into phases following the validation pipeline order
4. Each phase performs one category group of checks and produces a partial report

**Validation pipeline order** (MUST follow this sequence):

1. **Structural** — file exists, frontmatter valid, headings match template, TOC correct
2. **Semantic** — content quality, completeness, consistency within the artifact
3. **Cross-reference** — IDs defined, IDs referenced correctly, no dangling references
4. **Traceability** — requirements traced to design, design traced to features, features traced to code
5. **Synthesis** — overall assessment, priority-ranked issues, actionable recommendations

**Phase structure**:

| Phase | Category | Input |
|-------|----------|-------|
| 1 | Structural checks | Target artifact content, template structure |
| 2 | Semantic checks | Target artifact content, checklist criteria |
| 3 | Cross-reference checks | Target artifact + all referenced artifacts |
| 4 | Traceability checks | Target artifact + codebase markers |
| 5 | Synthesis | Partial reports from phases 1-4 |

**Grouping rules**:

- Structural and semantic checks MAY be combined into one phase if the checklist is short (< 20 items)
- Cross-reference and traceability MAY be combined if the artifact has few external references
- Synthesis is always the final phase
- If the full checklist has < 15 items, combine all checks into 2 phases (checks + synthesis)

---

## Strategy 2b: Analyze Codebase (by Scope + Runtime Reading)

**Applies to**: Validating code against design requirements, checking traceability markers, auditing implementation quality.

> **⚠️ EXCEPTION TO SELF-CONTAINMENT**: Codebase analysis is the ONE case where runtime file reading is permitted. Code files are too large to inline (typical codebase: 5K-50K lines). Phase files contain **instructions for what to read and check**, not the code itself.

**How to decompose**:

1. Load the design artifact (FEATURE, DESIGN) that defines requirements
2. Identify codebase directories from `artifacts.toml` or user input
3. Group checks by scope: file-level, module-level, cross-module, integration
4. Each phase checks one scope level with explicit file patterns

**Phase structure**:

| Phase | Scope | Inlined (kit content) | Runtime reads (project content) |
|-------|-------|-----------------------|-------------------------------|
| 1 | Setup | Codebase checklist, file patterns | Design artifact, directory listing |
| 2 | File-level | Checklist criteria, naming rules | Individual source files |
| 3 | Module-level | Module-level checklist criteria | Design artifact (boundaries), module entry points |
| 4 | Cross-module | Interface checklist criteria | Design artifact (contracts), import graphs |
| 5 | Traceability | `@cpt-*` marker format, ID rules | Design artifact (IDs), source files with markers |
| 6 | Synthesis | Acceptance criteria | Partial reports from phases 1-5 |

**What MUST be inlined** (stable kit content):

- Checklist criteria (what to verify) — from kit's `checklist.md`
- Codebase rules — from kit's `codebase/rules.md`
- Expected `@cpt-*` marker format — from kit
- File patterns and directory structure (metadata, not content)
- Acceptance criteria for each check

**What is read at runtime** (dynamic project content):

- Design artifacts (FEATURE, DESIGN) — requirements to check against
- Source code files — too large to inline, may change between phases
- Directory listings — dynamic
- Import/dependency graphs — computed
- Intermediate results from prior phases (`out/`)

**Phase file format for code analysis**:

```markdown
## Input

### Checklist Criteria (from kit)

{Inlined per compilation brief: checklist items for this scope level}

### File Patterns

- Source directory: `src/`
- Test directory: `tests/`
- File pattern: `*.py`
- Exclude: `__pycache__`, `*.pyc`

## Task

1. Read design artifact `architecture/FEATURE-auth.md` — extract requirement IDs
2. List files matching pattern `src/**/*.py`
3. For each file, read and check:
   - [ ] Function names follow snake_case
   - [ ] All public functions have docstrings
   - [ ] `@cpt-*` markers present for requirement IDs from step 1
4. Record findings in `out/phase-{N}-findings.md`
```

**Grouping rules**:

- If codebase has < 10 files, combine file-level and module-level into one phase
- If codebase has > 50 files, split file-level by directory (one phase per top-level dir)
- Traceability phase is ALWAYS separate (requires cross-referencing design IDs)
- Synthesis is always the final phase

---

## Strategy 3: Implement (by CDSL Blocks)

**Applies to**: Implementing code from a FEATURE specification.

**How to decompose**:

1. Load the FEATURE spec for the target feature
2. Identify all CDSL blocks: actor flows, algorithms/processes, state machines
3. Each CDSL block + its related test scenarios = 1 phase
4. Add a final integration phase for wiring and cross-cutting concerns

**Phase structure**:

| Phase | Content | Input |
|-------|---------|-------|
| 1 | Project scaffolding: file structure, imports, base types | FEATURE overview, project structure rules |
| 2..N-1 | One CDSL block: implementation + unit tests | CDSL block from FEATURE, coding rules, Phase 1 output |
| N | Integration: wiring, entry points, integration tests | All CDSL blocks summary, Phase 1 output |

**Grouping rules**:

- Each flow, algorithm, or state machine is its own phase
- If a CDSL block has < 3 steps, combine it with a related block
- If a CDSL block would produce > 500 lines of code, split by step groups
- Tests for a CDSL block are ALWAYS in the same phase as the implementation
- The scaffolding phase (phase 1) MUST NOT implement any business logic
- The integration phase (final) MUST NOT introduce new business logic

**Example — implementing a feature with 3 flows + 2 algorithms**:

```
Phase 1: Scaffolding — file structure, base types, imports
Phase 2: Flow 1 — generate-plan flow + tests
Phase 3: Flow 2 — execute-phase flow + tests
Phase 4: Flow 3 — check-status flow + tests
Phase 5: Algorithm 1 — decompose algorithm + tests
Phase 6: Algorithm 2 — compile-phase algorithm + tests
Phase 7: Integration — wiring, entry points, integration tests
```

---

## Budget Enforcement

Every phase MUST fit within the line budget after compilation (rules + input + task inlined).

| Metric | Target | Maximum | Action |
|--------|--------|---------|--------|
| Compiled phase file | ≤ 500 lines | ≤ 1000 lines | Split into sub-phases |
| Rules section | ≤ 200 lines | ≤ 300 lines | Narrow rule scope to phase |
| Input section | ≤ 300 lines | ≤ 500 lines | Split input across phases |
| Task steps | 3-7 steps (≤10 max) | 10 steps | Split task |

**Budget enforcement algorithm**:

1. Compile the phase file per its compilation brief (inline filtered rules, input, task)
2. Count total lines
3. If ≤ 500: accept
4. If 501-1000: accept with warning, suggest splitting
5. If > 1000: MUST split — identify the largest section (rules or input) and create sub-phases

**Splitting strategy when over budget**:

- If Rules section is largest: **NEVER trim or summarize rules** — instead, narrow the phase scope (fewer template sections / checklist categories) so the phase handles less work but still carries the full applicable rules. Split into more phases if necessary. Kit rules completeness is the highest priority.
- If Input section is largest: split the input content across two phases (e.g., template sections 1-3 in phase A, 4-6 in phase B)
- If Task section is largest: split the task steps into two sequential phases with explicit handoff

> **Invariant**: The union of all phases' Rules sections MUST cover 100% of the kit's `rules.md` for the target artifact kind. No rule may be dropped from the plan.

---

## Execution Context Prediction

Phase files contain **stable kit content** (rules, checklists, templates) inlined, while **project content** (artifacts, code, intermediate results) is read at runtime. This separation ensures:

1. **Stability**: Kit rules don't change between phase executions
2. **Freshness**: Project files may be modified between phases; reading at runtime gets the latest version
3. **Predictability**: Decomposition can estimate total context size accurately

### What Gets Inlined vs Runtime Read

| Content Type | Inline at Compile | Read at Runtime | Rationale |
|--------------|-------------------|-----------------|------------|
| **Kit rules** (`rules.md`) | ✓ | | Stable, defines constraints |
| **Kit checklist** (`checklist.md`) | ✓ | | Stable, defines what to check |
| **Kit template** (`template.md`) | ✓ | | Stable, defines structure |
| **Kit examples** (`example.md`) | ✓ | | Stable, reference quality |
| **Acceptance criteria** | ✓ | | Defined at plan time |
| **Project artifacts** (PRD, DESIGN, FEATURE) | | ✓ | May change between phases |
| **Source code** | | ✓ | Changes frequently |
| **Intermediate results** (`out/*.md`) | | ✓ | Created by prior phases |
| **Directory listings** | | ✓ | Dynamic |

### Estimation Formula

For each phase, estimate the **total execution context** (phase file + runtime reads):

```
execution_context = phase_file_lines              # compiled phase (inlined kit content)
                  + sum(runtime_artifact_lines)   # project artifacts to read
                  + sum(runtime_code_lines)       # source files to read (if code analysis)
                  + sum(intermediate_input_lines) # out/*.md from prior phases
                  + estimated_output_lines        # what agent will generate
```

**Estimation heuristics**:

| Component | How to estimate |
|-----------|----------------|
| `phase_file_lines` | Base (~100) + rules (~200) + checklist (~150) + template excerpt (~100) + task (~50) = ~600 lines typical |
| `runtime_artifact_lines` | Count lines of each artifact in `input_files` — read at runtime |
| `runtime_code_lines` | Estimate from file patterns: `find src -name "*.py" | wc -l` × avg lines per file |
| `intermediate_input_lines` | Prior phase `outputs` — typically 20-100 lines per file |
| `estimated_output_lines` | For generate: ~lines of sections assigned. For analyze: ~50-150 per category |

### Execution Context Budget

| Level | Threshold | Action |
|-------|-----------|--------|
| **Safe** | ≤ 1500 lines | Accept phase as-is — optimal zone, >95% rule adherence |
| **Warning** | 1501-2000 lines | Accept with warning; document risk |
| **Overflow** | > 2000 lines | **MUST split** — phase will exceed effective working memory |

> **Why 2000 lines**: Rule-following quality degrades above ~8K tokens (~2000 lines). Active constraints (MUST rules) are the heaviest context type. Better to have more phases than risk context overflow and missed checks.

### Auto-Split on Predicted Overflow

If a phase's predicted execution context exceeds **2000 lines**, decompose further:

1. **Identify the largest contributor** — which component dominates?
2. **Split strategy by contributor**:

| Largest contributor | Split strategy |
|-------------------|----------------|
| **Runtime artifacts** (large PRD/DESIGN) | Split checks by artifact: one phase per artifact |
| **Runtime code** (large codebase) | Split by directory or module: one phase per top-level dir |
| **Intermediate inputs** (many prior outputs) | Add consolidation sub-phase that summarizes findings |
| **Phase file itself** (too many rules) | Narrow scope: fewer checklist categories per phase |

3. **Re-estimate** each sub-phase after splitting — repeat until all are within budget

### Example: Overflow Detection

```
Phase 3: "Analyze PRD + DESIGN consistency" (analyze)
  phase_file_lines:        600  (rules + checklist + template + task)
  runtime_artifacts:       architecture/PRD.md (400 lines)
                           architecture/DESIGN.md (800 lines)
  intermediate_inputs:     out/phase-02-ids.md (50 lines)
  estimated_output_lines:  ~200 (consistency report)

  TOTAL: ~2050 lines → OVERFLOW
  Action: Split into two phases:
    - Phase 3a: Analyze PRD structure + IDs (~1050 lines)
    - Phase 3b: Analyze DESIGN against PRD IDs (~1100 lines)
```

```
Phase 2: "Check FEATURE completeness" (analyze)
  phase_file_lines:        550
  runtime_artifacts:       architecture/FEATURE-auth.md (180 lines)
  intermediate_inputs:     none
  estimated_output_lines:  ~100

  TOTAL: ~830 lines → SAFE
  Action: accept
```

### Integration with Decomposition

During Phase 2 (Decompose), after creating the initial phase list:

1. **Estimate** execution context for each phase using the formula above
2. **Flag** any phase in WARNING or OVERFLOW zone
3. **Auto-split** OVERFLOW phases before proceeding to compilation
4. **Report** estimates to user in the decomposition summary:

```
Decomposition ({strategy} strategy):
  Phase 1: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  Phase 2: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  Phase 3: {title} — ~{N} lines (phase: {P}, runtime: {R}) ⚠ WARNING
  Phase 4: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  
  Total phases: {N}
  Overflow phases: {count} (auto-split if any)
  Budget: 2000 lines max per phase
```

---

## Phase Dependencies

Phases within a plan MUST declare dependencies explicitly in the TOML frontmatter.

**Dependency rules**:

- Phase 1 has no dependencies (`depends_on = []`)
- Later phases depend on the phase that creates their input files
- Phases that operate on independent sections MAY run in parallel (no mutual dependencies)
- The synthesis/integration phase (final) depends on all prior phases

**Dependency graph example** (generate strategy, 4 phases):

```
Phase 1: [] — creates file with sections 1-2
Phase 2: [1] — adds sections 3-5 (needs file from phase 1)
Phase 3: [1] — adds sections 6-7 (needs file from phase 1, independent of phase 2)
Phase 4: [2, 3] — adds sections 8-10 (needs all prior sections for synthesis)
```

**Parallel execution**: Phases 2 and 3 in this example have no mutual dependency and could theoretically execute in parallel. However, the plan workflow MUST present phases sequentially by default. Parallel execution is an optimization the user may choose.

---

## Single-Context Bypass

If the total compiled content (all rules + all input + task) fits within 500 lines, the plan workflow MUST **stop and redirect** the user to the appropriate direct workflow. The plan workflow never executes tasks itself.

**Bypass check**:

1. Estimate total compiled size: template lines + rules lines + checklist lines + existing content lines
2. If estimate ≤ 500: **stop plan generation** and tell the user to run `/cypilot-generate` or `/cypilot-analyze` directly
3. If estimate > 500: proceed with plan generation

This prevents unnecessary overhead for small tasks and enforces the constraint that `/cypilot-plan` only produces plans.
