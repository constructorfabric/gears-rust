---
cypilot: true
type: requirement
name: Compilation Brief Template
version: 2.0
purpose: Template for per-phase compilation briefs — filled by LLM during plan Phase 3.2
---

# Compilation Brief Template

## Overview

Compilation briefs are short instruction documents (~50–100 lines) generated during plan Phase 3.2. Each brief tells the executing agent what files to read, how to use them, and how to compile the corresponding phase file.

The plan workflow agent fills this template once per phase using data from `plan.toml` and task context. No script is needed — the LLM generates briefs directly.

**Task-agnostic**: This template works for any plan type — kit-based artifact generation, code refactoring, migration, infrastructure, documentation, or any other task that was decomposed into phases.

---

## Template

````markdown
# Compilation Brief: Phase {number}/{total} — {title}

--- CONTEXT BOUNDARY ---
Disregard all previous context. This brief is self-contained.
Read ONLY the files listed below. Follow the instructions exactly.
---

## Phase Metadata

```toml
[phase]
number = {number}
total = {total}
type = "{type}"
title = "{title}"
depends_on = {depends_on}
input_files = {input_files}
output_files = {output_files}
outputs = {outputs}
inputs = {inputs}
```

## Load Instructions

This section tells the executing agent **what to read and how to use it**. Each item specifies a file (or set of files) with:
- **Path** and approximate size (`~N lines`)
- **Action**: how to use the content — inline into a phase file section, or read at runtime
- **Scope**: what parts to keep/skip (if only a subset is needed)

List all sources the agent needs. Omit anything not relevant to this phase.

{numbered list of load items — see examples below}

**Do NOT load**: {list files that exist but are irrelevant to this phase}

## Compile Phase File

Write to: `{plan_dir}/{phase_file}`

Phase file structure (all sections required):

1. **TOML frontmatter** — use Phase Metadata above (wrap in ```toml code fence)
2. **Preamble** — write verbatim: "This is a self-contained phase file. All rules, constraints, and kit content are included below. Project files listed in the Task section must be read at runtime. Follow the instructions exactly, run any EXECUTE commands as written, and report results against the acceptance criteria at the end."
3. **What** — describe deliverable + scope boundary (2-5 sentences)
4. **Prior Context** — summarize prior phases (≤ 20 lines), include pre-resolved user decisions
5. **User Decisions** — already-decided items + phase-bound questions with checkboxes
6. **Rules** — constraints the agent MUST follow (from load items marked "inline → Rules")
7. **Input** — reference content (from load items marked "inline → Input")
8. **Task** — 3-10 concrete steps with verifiable outcomes. Deterministic-first: use EXECUTE for CLI commands/scripts, LLM reasoning only for creative steps. Add "Read <file>" steps for runtime-read items.
9. **Acceptance Criteria** — 3-10 binary pass/fail checks
10. **Output Format** — completion report + next-phase prompt (see plan-template.md Section 9)

## Context Budget

- Phase file target: ≤ 600 lines
- Inlined content estimate: ~{N} lines
- Total execution context (phase file + runtime reads): ≤ 2000 lines
- If Rules section exceeds 300 lines, narrow phase scope — NEVER drop rules

## After Compilation

Report: "Phase {number} compiled → {phase_file} (N lines)"
Then apply context boundary and proceed to next brief.
````

---

## Load Instructions: How to Fill

The Load Instructions section is the core of the brief. It is a numbered list of items — each item is a file (or file group) that the executing agent needs. The LLM decides what items to include based on the task type and phase scope.

### Item anatomy

Each item follows this pattern:

```
N. **Label**: Read `{path}` (lines {from}-{to}, ~{N} lines)
   - {action}: what to do with the content
   - {scope}: what to keep/skip (optional, if only parts are needed)
```

**Line ranges** help the executing agent read only the relevant portion of a file. Specify `lines {from}-{to}` when:
- Only a section of a large file is needed (e.g., H2 sections 3-5 of a 800-line doc)
- A specific function, class, or config block is the target
- The file is too large to read in full within the context budget

Omit line ranges when the entire file should be read. Use `~` for approximate ranges when exact lines aren't known at brief time (the executing agent will locate the exact boundaries).

### Actions

There are two actions. Choose one per item:

| Action | Meaning | Goes into phase section |
|--------|---------|------------------------|
| **Inline** | Copy content into the phase file at compile time | Rules, Input, or both |
| **Runtime read** | Agent reads the file during execution, NOT compiled in | Task (as "Read <file>" step) |

**When to inline**: Content that is stable and defines constraints or structure — rules, templates, checklists, examples, style guides, coding standards, API specs.

**When to runtime-read**: Content that may change between phases or is too large to inline — project files, source code, prior phase outputs, database schemas, config files, external docs.

### Examples by task type

**Kit-based artifact generation** (e.g., generate ADR, PRD):

```
1. **Rules**: Read `{kit}/artifacts/ADR/rules.md` (lines 30-450, ~420 lines)
   - Inline → Rules section
   - Keep: MUST/MUST NOT requirements, structural/semantic rules, constraints
   - Skip: lines 1-29 (Prerequisites, Load Dependencies), lines 451+ (Tasks, Next Steps)

2. **Template**: Read `{kit}/artifacts/ADR/template.md` (lines 10-48, ~38 lines)
   - Inline → Input section (H2 sections 1-4 only)

3. **Example**: Read `{kit}/artifacts/ADR/examples/example.md` (~91 lines)
   - Inline → Input section as "Reference Example" (full file)

4. **Project context**: `whatsnew.toml` (lines ~140-172), `workflows/plan.md` (lines 1-80)
   - Runtime read — add "Read <file>" steps to Task
```

**Code refactoring** (e.g., extract module, rename API):

```
1. **Coding standards**: Read `docs/CONTRIBUTING.md` (lines 45-98, ~53 lines)
   - Inline → Rules section ("Code Style" and "Testing" sections only)

2. **Design doc**: Read `architecture/DESIGN.md` (lines 120-210, ~90 lines)
   - Inline → Input section (component diagram and interface contracts)

3. **Source files to modify**: `src/api/handlers.py` (lines 1-250), `src/api/routes.py` (~80 lines)
   - Runtime read — add "Read <file>" steps to Task

4. **Test files**: `tests/test_handlers.py` (lines ~30-120, relevant test class)
   - Runtime read — verify tests pass after changes
```

**Migration** (e.g., upgrade framework, migrate database):

```
1. **Migration guide**: Read `docs/migration-v3-to-v4.md` (lines 1-200, ~200 lines)
   - Inline → Rules section (breaking changes, required steps — full file)

2. **Changelog**: Read `CHANGELOG.md` (lines 3-52, ~50 lines)
   - Inline → Input section (v4.0 section only)

3. **Config files**: `pyproject.toml` (~40 lines), `setup.cfg` (lines 1-25)
   - Runtime read — modify during execution

4. **Prior phase output**: `{plan_dir}/out/phase-01-audit.md` (~60 lines)
   - Runtime read — contains dependency audit from Phase 1
```

**Free-form task** (e.g., write documentation, create CI pipeline):

```
1. **Style guide**: Read `docs/STYLE.md` (lines 1-80, ~80 lines)
   - Inline → Rules section (full file)

2. **Existing examples**: Read `docs/api-reference.md` (lines 1-60, ~60 lines)
   - Inline → Input section as reference for tone and format

3. **Source files**: `src/core/api.py` (lines ~15-90, public class), `src/core/models.py` (lines ~1-50)
   - Runtime read — extract docstrings and signatures
```

---

## Fill Rules

When generating briefs from this template:

1. **Include only what this phase needs** — every item must have a clear reason. If a file exists but isn't relevant to the phase, put it in the "Do NOT load" list.
2. **Line counts** — run `wc -l` or estimate from prior reads. Approximate values are fine (prefix with ~).
3. **One brief per phase** — generate briefs sequentially, apply context boundary between each.
4. **File naming** — `brief-{NN}-{slug}.md` where NN is zero-padded phase number and slug is from plan.toml.
5. **Inline budget** — if inlined content exceeds ~500 lines, consider splitting into multiple load items with narrower scope, or moving some items to runtime reads.
