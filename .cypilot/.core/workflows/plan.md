---
cypilot: true
type: workflow
name: cypilot-plan
description: Decompose large tasks into self-contained phase files
version: 1.0
purpose: Universal workflow for generating execution plans with phased delivery
---

# Plan

> **⛔ CRITICAL CONSTRAINT**: This workflow ONLY generates execution plans. It NEVER executes the underlying task (generate, analyze, implement) directly. Even if the task seems small, this workflow's job is to produce phase files — not to do the work itself. If the task is small enough for direct execution, tell the user to use `/cypilot-generate` or `/cypilot-analyze` instead.

> **⛔ CRITICAL CONSTRAINT — FULL CONTEXT LOADING**: Before generating ANY plan, you MUST load and process ALL navigation rules (`ALWAYS open`, `OPEN and follow`, `ALWAYS open and follow`) from the **target workflow** (generate.md, analyze.md, or the relevant workflow). Every file referenced by those directives MUST be opened and its content used during decomposition and compilation. Skipping ANY navigation rule means phase files will be compiled with incomplete context, producing broken or shallow results. This is the #1 source of plan quality failures.

> **⛔ CRITICAL CONSTRAINT — KIT RULES ARE LAW** *(highest priority)*: Every rule in the kit's `rules.md` for the target artifact kind MUST be enforced in the generated plan — **completely, without omission or summarization**. Rules are inlined verbatim into phase files. If the full rules don't fit in a single phase, split the phase so each sub-phase gets ALL rules relevant to its scope — but NEVER trim, summarize, or selectively skip rules to fit a budget. The `checklist.md` items are equally mandatory for analyze tasks. A plan that drops kit rules produces artifacts that fail validation.

> **⛔ CRITICAL CONSTRAINT — DETERMINISTIC FIRST**: Every phase step that CAN be done by a deterministic tool (cpt command, script, shell command) MUST use that tool instead of LLM reasoning. Discover available tools dynamically in Phase 0 — do NOT assume a fixed set of commands. Tool capabilities change between versions. The CLISPEC file is the source of truth for what commands exist and what they can do.

> **⛔ CRITICAL CONSTRAINT — INTERACTIVE QUESTIONS COMPLETENESS** *(mandatory)*: You MUST find ALL interactive questions, user input requests, confirmation gates, review requests, and decision points from: (1) the target workflow, (2) `rules.md` for the target artifact kind, (3) `checklist.md`, (4) `template.md`, AND (5) **every file referenced by navigation rules** (`ALWAYS open`, `OPEN and follow`) in those files — recursively. Every interaction point found MUST appear in the compiled plan: pre-resolvable questions asked BEFORE plan generation, phase-bound questions embedded in phase files. **Missing even ONE interaction point = plan is INVALID.** See `{cypilot_path}/.core/requirements/plan-checklist.md` Section 2 for the complete extraction procedure.

> **⛔ CRITICAL CONSTRAINT — BRIEF BEFORE COMPILE**: Phase files MUST NOT be written directly. Every phase file MUST be compiled from a corresponding compilation brief (`brief-{NN}-{slug}.md`) that was written to disk in Phase 3.2. The brief is the contract between decomposition (what to include) and compilation (how to assemble). Skipping briefs produces phase files that silently omit kit content, miss load instructions, or inline wrong sections. **If you find yourself writing a phase file without first reading its brief from disk — STOP, you are violating the workflow.** Write the brief first, write it to disk, THEN compile from it. A phase file without a corresponding brief file on disk = INVALID plan.

ALWAYS open and follow `{cypilot_path}/.core/skills/cypilot/SKILL.md` FIRST WHEN {cypilot_mode} is `off`

**Type**: Operation

ALWAYS open and follow `{cypilot_path}/.core/requirements/execution-protocol.md` FIRST

ALWAYS open and follow `{cypilot_path}/.core/requirements/plan-template.md` WHEN compiling phase files

ALWAYS open and follow `{cypilot_path}/.core/requirements/plan-decomposition.md` WHEN decomposing tasks into phases

OPEN and follow `{cypilot_path}/.core/requirements/prompt-engineering.md` WHEN compiling phase files (phase files ARE agent instructions)

OPEN and follow `{cypilot_path}/.core/requirements/plan-checklist.md` WHEN validating plans (Phase 4.1 self-validation or /cypilot-analyze on plan)

For context compaction recovery during multi-phase workflows, follow `{cypilot_path}/.core/requirements/execution-protocol.md` Section "Compaction Recovery".

---

## Table of Contents

- [Plan](#plan)
  - [Table of Contents](#table-of-contents)
  - [Overview](#overview)
  - [Context Budget \& Overflow Prevention (CRITICAL)](#context-budget--overflow-prevention-critical)
  - [Phase 0: Resolve Variables \& Discover Tools](#phase-0-resolve-variables--discover-tools)
    - [0.1 Discover Available Tools](#01-discover-available-tools)
  - [Phase 1: Assess Scope](#phase-1-assess-scope)
    - [1.1 Identify Task Type](#11-identify-task-type)
    - [1.1b Extract Target Workflow Navigation Rules (CRITICAL)](#11b-extract-target-workflow-navigation-rules-critical)
    - [1.2 Estimate Compiled Size](#12-estimate-compiled-size)
    - [1.3 Scan for User Interaction Points (CRITICAL)](#13-scan-for-user-interaction-points-critical)
    - [1.4 Identify Target](#14-identify-target)
  - [Phase 2: Decompose](#phase-2-decompose)
    - [For `generate` tasks:](#for-generate-tasks)
    - [For `analyze` tasks:](#for-analyze-tasks)
    - [For `implement` tasks:](#for-implement-tasks)
    - [Intermediate Results Analysis](#intermediate-results-analysis)
    - [Review Phases](#review-phases)
    - [Execution Context Prediction](#execution-context-prediction)
  - [Phase 3: Compile Phase Files](#phase-3-compile-phase-files)
    - [3.1 Write Plan Manifest](#31-write-plan-manifest)
    - [3.2 Generate Compilation Briefs (from Template)](#32-generate-compilation-briefs-from-template)
    - [3.3 Compile Phase Files (Agent + Context Boundary)](#33-compile-phase-files-agent--context-boundary)
      - [Context boundary (CRITICAL)](#context-boundary-critical)
      - [Compilation steps](#compilation-steps)
    - [3.4 Validate Phase Files](#34-validate-phase-files)
  - [Phase 4: Finalize Plan](#phase-4-finalize-plan)
    - [Plan Lifecycle Strategy](#plan-lifecycle-strategy)
    - [Phase 4.1: Validate Plan Before Execution (MANDATORY)](#phase-41-validate-plan-before-execution-mandatory)
    - [New-Chat Startup Prompt](#new-chat-startup-prompt)
  - [Phase 5: Execute Phases](#phase-5-execute-phases)
    - [5.1 Load Phase](#51-load-phase)
    - [5.2 Execute](#52-execute)
    - [5.3 Save Intermediate Results](#53-save-intermediate-results)
    - [5.4 Report](#54-report)
    - [5.5 Update Status](#55-update-status)
    - [5.6 Phase Handoff](#56-phase-handoff)
      - [Context boundary for continue mode](#context-boundary-for-continue-mode)
    - [5.7 Abandoned Plan Recovery](#57-abandoned-plan-recovery)
  - [Phase 6: Check Status](#phase-6-check-status)
  - [Plan Storage Format](#plan-storage-format)
  - [Execution Log](#execution-log)

<!-- /toc -->

---

## Overview

The plan workflow **generates** execution plans — it decomposes large agent tasks into self-contained phase files. Each phase file is a compiled prompt — all rules, constraints, context, and paths pre-resolved and inlined — executable by any AI agent without Cypilot knowledge.

**This workflow produces FILES, not results.** The output is a set of phase files in `.plans/`, not the artifact or analysis itself.

**When to use this workflow**:
- User explicitly invokes `/cypilot-plan` with a task description
- Task involves creating or updating a large artifact (estimated > 500 lines of compiled context)
- Task involves validating an artifact with a long checklist (> 15 items)
- Task involves implementing a feature with multiple CDSL blocks

**When NOT to use**:
- Task fits in a single context window (< 500 lines compiled) — redirect user to `/cypilot-generate` or `/cypilot-analyze`
- Task is a simple edit or fix
- User explicitly requests direct execution without a plan

**Workflow output**: plan.toml + N phase files in `{cypilot_path}/.plans/{task-slug}/`

**Workflow summary**:
1. Assess scope and estimate compiled size
2. Select decomposition strategy (generate / analyze / implement)
3. Decompose task into phases
4. Compile each phase into a self-contained phase file
5. Write plan manifest and phase files to disk
6. Generate startup prompt for execution in a new chat
7. (Optionally) Execute phases one at a time if user requests

---

## Context Budget & Overflow Prevention (CRITICAL)

This workflow is itself designed to PREVENT context overflow. Follow these rules strictly:

- Do NOT load all kit dependencies at once — load them incrementally per phase during compilation
- Do NOT hold all phase files in context simultaneously — compile and write one at a time
- If compiling a phase file would exceed YOUR current context budget, write what you have so far and use the Compaction Recovery protocol
- The plan manifest (`plan.toml`) serves as your recovery checkpoint — always write it before starting compilation

**Context load budget for this workflow**:
- Phase 0-1: ~200 lines (execution protocol + info output)
- Phase 2: ~300 lines (decomposition strategies + target artifact overview)
- Phase 3: ~500 lines per phase file (template + rules + input for ONE phase at a time)
- Phase 4: ~50 lines (plan manifest)
- Phase 5-6: ~500 lines (one phase file at a time)

---

## Phase 0: Resolve Variables & Discover Tools

Run the execution protocol to discover Cypilot configuration:

```
EXECUTE: {cypilot_command} info
```

Store these resolved variables:

| Variable | Source | Used For |
|----------|--------|----------|
| `{cypilot_path}` | info output | Plan storage location |
| `{project_root}` | info output | Resolving file paths |
| Kit paths | info output, per kit | Loading templates, rules, checklists |

### 0.1 Discover Available Tools

Read the CLISPEC file to discover all available commands and their capabilities:

```
READ: {cypilot_path}/.core/skills/cypilot/cypilot.clispec
```

For each command, extract:
- **Name** and synopsis
- **What it does** (deterministic capability)
- **Input/output format** (JSON output for machine parsing)
- **Relevant use cases** for plan phases

Build a **tool capability map** for use during decomposition and compilation. For each COMMAND block in the CLISPEC, record:

```
{command_name} — {DESCRIPTION line} [outputs: {OUTPUT format}]
```

This map MUST be built dynamically from the CLISPEC at plan generation time — **never hardcoded**. If new commands appear in future CLISPEC versions, they will be discovered and used automatically.

**Also check for kit-provided scripts**:

```
SCAN: {kit_scripts_path}/ for *.py, *.sh files
```

Add any kit scripts to the tool map with their purpose (inferred from filename and docstring/header).

---

## Phase 1: Assess Scope

Determine whether a plan is needed or the task can be executed directly.

### 1.1 Identify Task Type

Ask the user or infer from the request:

| Signal | Task Type | Target Workflow |
|--------|-----------|----------------|
| "create", "generate", "write", "update", "draft" + artifact kind | `generate` | `generate.md` |
| "validate", "review", "check", "audit", "analyze" + artifact kind | `analyze` | `analyze.md` |
| "implement", "code", "build", "develop" + feature name | `implement` | `generate.md` (code mode) |

### 1.1b Extract Target Workflow Navigation Rules (CRITICAL)

Open the target workflow file (`{cypilot_path}/.core/workflows/{target_workflow}`) and extract **every** navigation directive:

1. **Scan** for all lines matching `ALWAYS open`, `OPEN and follow`, `ALWAYS open and follow`
2. **List** every referenced file path with its condition (WHEN clause)
3. **Evaluate** each condition against the current task context
4. **Open and read** every file whose condition is met
5. **Record** a manifest of loaded files for later verification

Example extraction:

```
Target workflow: analyze.md
Navigation rules found:
  [1] ALWAYS: execution-protocol.md → loaded ✓
  [2] ALWAYS WHEN code mode: code-checklist.md → N/A (artifact mode)
  [3] ALWAYS WHEN consistency mode: consistency-checklist.md → loaded ✓
  [4] OPEN WHEN prompt review: prompt-engineering.md → N/A

Kit dependencies (from execution-protocol.md resolution):
  [5] rules.md for target kind → loaded ✓
  [6] checklist.md for target kind → loaded ✓
  [7] template.md for target kind → loaded ✓ (if generate)
  [8] example.md for target kind → loaded ✓ (if generate)
  [9] constraints.toml → loaded ✓
```

**Gate**: Do NOT proceed to Phase 1.2 until ALL applicable navigation rules have been processed and their referenced files loaded. Report the manifest to the user:

```
Context loaded for plan generation:
  Workflow: {target_workflow} ({N} navigation rules processed)
  Kit files: {M} files loaded ({rules}, {checklist}, {template}, ...)
  Total context: ~{L} lines
  
  All navigation rules processed? [YES/NO]
```

If any required file could not be loaded, STOP and report the error.

### 1.2 Estimate Compiled Size

Estimate the total compiled context needed:

```
estimated_size = template_lines + rules_lines + checklist_lines + existing_content_lines
```

**Decision**:
- If `estimated_size ≤ 500`: Report to user and **stop** (do NOT execute the task):
  ```
  This task fits in a single context window (~{N} lines).
  A plan is not needed. Run the task directly:
    /cypilot-generate {target}   — for generation tasks
    /cypilot-analyze {target}    — for analysis/review tasks
  ```
  **STOP HERE.** Do not proceed with plan generation or task execution.
- If `estimated_size > 500`: Proceed to Phase 2.

### 1.3 Scan for User Interaction Points (CRITICAL)

> **⛔ MANDATORY**: This step MUST be completed exhaustively. Missing interaction points is the #2 source of plan failures (after missing rules).

**Scan scope** — you MUST scan ALL of these sources:

1. **Target workflow** (`generate.md`, `analyze.md`, etc.)
2. **`rules.md`** for the target artifact kind
3. **`checklist.md`** for the target artifact kind
4. **`template.md`** for the target artifact kind
5. **Every file referenced by navigation rules** in the above files — follow `ALWAYS open`, `OPEN and follow`, `ALWAYS open and follow` directives **recursively**

**Recursive scanning procedure**:

1. Start with the target workflow file
2. Extract all navigation directives (ALWAYS open, OPEN and follow)
3. For each referenced file:
   a. Open and scan for interaction patterns
   b. Extract any navigation directives in THAT file
   c. Recursively scan those files too
4. Continue until no new files are discovered
5. Record the complete file manifest with interaction points found in each

Scan for interaction points — places where the agent is expected to ask the user something, wait for input, or request review. Look for these patterns:

| Pattern | Type | Regex/Keywords | Example |
|---------|------|----------------|---------|
| Questions to user | `question` | `ask the user`, `ask user`, `what is`, `which`, `?` at end | "Ask the user which modules to include" |
| Expected user input | `input` | `user provides`, `user specifies`, `user enters`, `input from user` | "User provides project name and tech stack" |
| Confirmation gates | `confirm` | `wait for`, `confirm`, `approval`, `before proceeding` | "Wait for user confirmation before proceeding" |
| Review requests | `review` | `review`, `present for`, `show to user`, `user inspects` | "Present output for user review before writing" |
| Choice/decision points | `decision` | `choose`, `select`, `option A or B`, `decide` | "User selects between option A and B" |

Collect all found interaction points into a list:

```
Interaction points found:
  [Q1] question: "What is the target system slug?" (from: rules.md)
  [Q2] input: "User provides existing content to preserve" (from: template.md)
  [R1] review: "Review generated sections before writing" (from: generate.md)
  [D1] decision: "Choose ID naming convention" (from: rules.md)
```

**Classify each interaction point**:
- **Pre-resolvable** — can be answered NOW, before plan generation (e.g., project name, tech stack, naming conventions). Ask the user immediately and record answers.
- **Phase-bound** — must be answered during a specific phase (e.g., "review this section's output"). Embed into the appropriate phase file.
- **Cross-phase** — affects multiple phases (e.g., "choose architecture style"). Ask NOW and inline the answer into all affected phase files.

Ask all pre-resolvable and cross-phase questions to the user NOW:

```
Before generating the plan, I need a few decisions:

  [Q1] What is the target system slug? (used in all ID patterns)
  [D1] Which ID naming convention? Option A: ... / Option B: ...
  
Phase-bound interactions (will be handled during execution):
  [R1] Review of generated sections (Phase 2, Phase 4)
```

Record all answers in a `decisions` block to include in phase files.

**Completeness verification** (MANDATORY before proceeding):

```
Interaction points scan complete:
  Files scanned: {N} (list all files)
  Interaction points found: {M}
    - Pre-resolvable: {count} (asked and answered above)
    - Phase-bound: {count} (will be embedded in phase files)
    - Cross-phase: {count} (asked and answered above)
  
  All source files scanned? [YES/NO]
  All interaction points classified? [YES/NO]
```

**Gate**: Do NOT proceed to Phase 1.4 if any source file was not scanned or any interaction point was not classified. Missing interaction points will cause plan validation to FAIL.

**If zero interaction points found**: This is valid for some tasks (e.g., pure structural analysis, deterministic validation). Report "No interaction points detected — task is fully autonomous" and proceed to Phase 1.4. The User Decisions section will be omitted from phase files.

### 1.4 Identify Target

Resolve the target artifact or feature:

- **For generate/analyze**: identify artifact kind, file path, and kit
- **For implement**: identify FEATURE spec path and its CDSL blocks

Report to user:

```
Plan scope:
  Type: {generate|analyze|implement}
  Target: {artifact kind or feature name}
  Estimated size: ~{N} lines (exceeds single-context limit of 500)
  Proceeding with plan generation...
```

---

## Phase 2: Decompose

Open and follow `{cypilot_path}/.core/requirements/plan-decomposition.md`.

Select the appropriate strategy based on task type and apply it:

### For `generate` tasks:

1. Load the template for the target artifact kind
2. List all H2 sections
3. Group into phases of 2-4 sections per the grouping rules
4. Record phase boundaries

### For `analyze` tasks:

1. Load the checklist for the target artifact kind
2. List all checklist categories
3. Group into phases following the validation pipeline order (structural → semantic → cross-ref → traceability → synthesis)
4. Record phase boundaries

### For `implement` tasks:

1. Load the FEATURE spec
2. List all CDSL blocks (flows, algorithms, state machines)
3. Assign one CDSL block + tests per phase
4. Add scaffolding (phase 1) and integration (final phase)
5. Record phase boundaries

**Output**: a list of phases with:
- Phase number and title
- Sections/categories/blocks covered
- Dependencies on other phases
- Input files and output files
- Assigned interaction points (phase-bound questions, review gates)
- Intermediate results: what this phase produces that later phases need

### Intermediate Results Analysis

During decomposition, identify **data flow between phases** — cases where one phase produces output that a later phase or the final assembly needs.

Common patterns:

| Pattern | Example | What to save |
|---------|---------|--------------|
| **Incremental artifact** | Phase 1 writes PRD §1-3, Phase 2 writes §4-6 | Each phase appends to the target file |
| **Extracted data** | Phase 1 extracts actor list, Phase 2 uses it for requirements | Save actor list to `out/actors.md` |
| **Analysis notes** | Phase 1 structural check finds issues, Phase 3 synthesis references them | Save findings to `out/phase-01-findings.md` |
| **Generated IDs** | Phase 1 creates ID scheme, all later phases reference it | Save ID registry to `out/id-registry.md` |
| **Decision log** | Phase 1 resolves ambiguities, later phases depend on decisions | Save decisions to `out/decisions.md` |

For each phase, record:
- **`outputs`** — files this phase creates or updates (in `out/` or in the project)
- **`inputs`** — files from prior phases this phase needs to read

Rules:
- If a phase produces data that ANY later phase needs → it MUST save to `{cypilot_path}/.plans/{task-slug}/out/{filename}`
- If a phase only produces the final target artifact and nothing else depends on it → save directly to the project path
- If a final/synthesis phase needs to assemble outputs from all prior phases → list ALL prior output files as inputs
- Intermediate files use descriptive names: `out/phase-{NN}-{what}.md` (e.g., `out/phase-01-actors.md`)

### Review Phases

If the source workflow requires user review at certain points (e.g., "present output for review before writing to disk"), insert **review gates** between phases:

- A review gate is NOT a separate phase — it is a handoff point where the phase's Output Format includes the generated content for user inspection
- The phase's Acceptance Criteria should include: "User has reviewed and approved the output"
- The handoff prompt (Phase 5.5) naturally provides the review opportunity

If the source workflow expects a **major review** (e.g., full artifact review before finalization), add a dedicated **Review phase** that:
- Loads all outputs from prior phases
- Presents a consolidated view
- Lists specific review questions from the source workflow
- Blocks further execution until user approves

### Execution Context Prediction

After creating the initial phase list, estimate **total execution context** for each phase per the Execution Context Prediction section in `plan-decomposition.md`:

1. For each phase, calculate: `phase_file_lines + sum(input_files lines) + sum(inputs lines) + estimated_output_lines`
2. Flag phases that exceed **2000 lines** (OVERFLOW) — these MUST be split further
3. Flag phases that exceed **1500 lines** (WARNING) — note for user

> **Budget = 2000 lines max** per phase (phase file + runtime reads). Better to have more phases than risk context overflow during execution.

If any phase is in OVERFLOW, apply the auto-split strategies from `plan-decomposition.md` and re-estimate until all phases are within budget.

Report decomposition to user:

```
Decomposition ({strategy} strategy):
  Phase 1: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  Phase 2: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  Phase 3: {title} — ~{N} lines (phase: {P}, runtime: {R}) ⚠ WARNING
  ...
  Phase N: {title} — ~{N} lines (phase: {P}, runtime: {R}) ✓
  
  Total phases: {N}
  Overflow phases: 0
  Budget: 2000 lines max per phase
  
  Proceed with compilation? [y/n]
```

Wait for user confirmation before proceeding.

---

## Phase 3: Compile Phase Files

Open and follow `{cypilot_path}/.core/requirements/plan-template.md`.

Compilation is split into three steps to **minimize agent context usage**: a deterministic script inlines kit content, then the agent fills only the creative sections one phase at a time.

### 3.1 Write Plan Manifest

Write `plan.toml` **before** compilation so the script can read it:

```toml
[plan]
task = "{task description}"
type = "{generate|analyze|implement}"
target = "{artifact kind}"          # e.g. "PRD", "DESIGN", "FEATURE"
kit_path = "{absolute path to kit}" # e.g. "/abs/path/config/kits/sdlc"
created = "{ISO 8601 timestamp}"
total_phases = {N}

[[phases]]
number = 1
title = "{phase title}"
slug = "{short-slug}"
file = "phase-01-{slug}.md"
brief_file = "brief-01-{slug}.md"  # compilation brief (MUST exist before phase file)
status = "pending"
depends_on = []
input_files = []                    # project files to read at runtime
output_files = ["{target file}"]    # project files this phase creates/modifies
outputs = ["out/phase-01-{what}.md"] # intermediate results for later phases
inputs = []                         # intermediate results from prior phases
template_sections = [1, 2, 3]      # H2 numbers from template.md (generate tasks)
checklist_sections = []             # H2 numbers from checklist.md (analyze tasks)

# ... one [[phases]] block per phase
```

**New fields vs. old schema**:
- `kit_path` — absolute path to kit directory (from `cpt info` output)
- `template_sections` — which H2 sections from `template.md` to inline (for generate)
- `checklist_sections` — which H2 sections from `checklist.md` to inline (for analyze)
- `slug` — short slug for filename generation

Write to: `{cypilot_path}/.plans/{task-slug}/plan.toml`

### 3.2 Generate Compilation Briefs (from Template)

For each phase in `plan.toml`, generate a **compilation brief** — a short instruction file (~50–80 lines) that tells the agent exactly what kit files to read, what to skip, and how to compile the phase file.

ALWAYS open and follow `{cypilot_path}/.core/requirements/brief-template.md` to fill one brief per phase.

**Steps**:

1. **Estimate kit file sizes** (for context budget):
   ```
   EXECUTE: wc -l {kit_path}/artifacts/{kind}/rules.md {kit_path}/artifacts/{kind}/template.md {kit_path}/artifacts/{kind}/checklist.md
   ```
2. **List examples** (if any):
   ```
   EXECUTE: ls {kit_path}/artifacts/{kind}/examples/*.md 2>/dev/null
   ```
3. **Fill the brief template** for each phase using data from `plan.toml`:
   - Substitute phase metadata (number, title, depends_on, files)
   - Fill Load Instructions — which kit files to read based on `template_sections` and `checklist_sections`
   - Omit unused Load Instructions (e.g., no checklist for generate phases)
   - Set context budget estimate from `wc -l` results
4. **Write each brief** to `{cypilot_path}/.plans/{task-slug}/brief-{NN}-{slug}.md`

**What a brief contains** (per phase):
- Context boundary instruction (disregard prior context)
- Phase metadata (from plan.toml)
- Load instructions: which kit files to read, which sections to keep/skip
- Phase file structure guide (10 sections)
- Context budget estimate

**What a brief does NOT contain**:
- The phase file itself — that's the agent's job in 3.3
- No kit content is copied — the brief points to files with read instructions

**Gate — Brief files MUST exist on disk before compilation**:

> **⛔ MANDATORY**: Before proceeding to 3.3, verify every brief file was written to disk:
>
> ```
> for each [[phases]] in plan.toml:
>   VERIFY: {plan_dir}/{brief_file} exists on disk
>   FAIL if: any brief file missing — STOP and write missing briefs
> ```
>
> Do NOT proceed to 3.3 until ALL brief files pass this check. This gate prevents the most common plan generation failure: skipping briefs and writing phase files directly from accumulated context.

### 3.3 Compile Phase Files (Agent + Context Boundary)

For each phase, one at a time:

#### Context boundary (CRITICAL)

Before compiling each phase, apply the context boundary protocol:

```
--- CONTEXT BOUNDARY ---
Disregard all previous context. The brief below is self-contained.
Read ONLY the files listed in the brief. Follow its instructions exactly.
---
```

This ensures each phase compilation starts with minimal context (~50 lines brief + ~400-600 lines kit files = ~700 lines total) instead of accumulated context from prior phases.

#### Compilation steps

1. **Read the brief FROM DISK** — `{cypilot_path}/.plans/{task-slug}/{brief_file}`
   ⛔ You MUST read the brief file from disk using a file read tool, not from memory or accumulated context. If the file does not exist on disk, it was never written — go back to 3.2 and write it. Compiling a phase without reading its brief from disk = INVALID.
2. **Read kit files** per the brief's Load Instructions:
   - Rules: read `rules.md`, inline MUST/MUST NOT rules (skip Prerequisites, Tasks, Next Steps)
   - Template: read specified H2 sections, inline into Input
   - Example: read for style reference, inline into Input
3. **Write the phase file** per the brief's Compile Phase File section:
   - TOML frontmatter, Preamble, What, Prior Context, User Decisions, Rules, Input, Task, Acceptance Criteria, Output Format
4. **Apply deterministic-first principle** to Task steps:
   - Use `EXECUTE: {cypilot_command} ...` for deterministic steps
   - Use LLM reasoning only for creative/synthesis steps
   - Add "Read <file>" steps for `input_files` and `inputs`
   - If review gate: add "Present output to user for review. Wait for approval."
5. **Report**: "Phase {N} compiled → {filename} ({lines} lines)"
6. **Apply context boundary** before next phase

**Continue mode** (default): compile next phase in same chat with context boundary.
**New chat mode** (recommended for 4+ phases): copy prompt to new chat for guaranteed clean context.

### 3.4 Validate Phase Files

After all phases are compiled, validate each one:

1. **Brief files exist**: verify every `brief_file` in `plan.toml` has a corresponding file on disk. Missing brief = compilation was done without brief = INVALID plan. If any brief is missing, the plan MUST be regenerated from Phase 3.2.
2. **Phase-brief consistency**: for each phase, verify the phase file's Rules section covers the same kit file ranges specified in the brief's Load Instructions. Drift between brief and compiled phase = content was added or dropped outside the brief contract.
3. Scan for unresolved `{...}` variables outside code fences → MUST be zero
4. Count total lines → MUST be ≤ 1000
5. If > 1000: split into sub-phases per budget enforcement rules in `plan-decomposition.md`
6. **Kit rules completeness check** *(highest priority)*: verify Rules section contains all MUST/MUST NOT rules from `rules.md`. If any rule is missing → add it. If adding pushes over budget → narrow scope and re-split. NEVER drop rules.
7. **Context budget check**: estimate `phase_file_lines + sum(input_files lines) + sum(inputs lines) + output_lines`. Must be ≤ 2000 lines. If over, split the phase.
8. After the last phase: verify that the **union of all phases' Rules sections covers 100% of applicable rules from `rules.md`** — no rule left behind.

---

## Phase 4: Finalize Plan

> **Note**: `plan.toml` was already written in Phase 3.1 and phase files compiled in Phase 3.2-3.3.

**Status values** in `plan.toml`: `pending`, `in_progress`, `done`, `failed`

### Plan Lifecycle Strategy

After writing the plan, ask the user how to handle plan files after completion:

```
Plan files are stored in {cypilot_path}/.plans/{task-slug}/.
How should completed plans be handled?

  [1] .gitignore — add .plans/ to .gitignore (some editors block gitignored files)
  [2] Cleanup phase — add a final phase that deletes plan files after all phases pass
  [3] Archive — move completed plans to {cypilot_path}/.plans/.archive/ (gitignored)
  [4] Keep as-is — leave plan files in place, user manages manually
```

Record the user's choice in `plan.toml`:

```toml
[plan]
# ... other fields ...
lifecycle = "gitignore"  # or "cleanup", "archive", "manual"
```

**If `gitignore`**: append `.plans/` to `.gitignore` (or create it). All plan files become invisible to git and some editors.
**If `cleanup`**: add a final phase (N+1) titled "Cleanup" that deletes the plan directory after verifying all phases passed.
**If `archive`**: do NOT gitignore `.plans/` — plan files must remain accessible to editors during execution. After all phases complete, move the plan directory to `{cypilot_path}/.plans/.archive/{task-slug}/` and add only `.plans/.archive/` to `.gitignore`.
**If `manual`**: do nothing — user is responsible for cleanup.

Report plan summary to user:

```
Plan created: {cypilot_path}/.plans/{task-slug}/
  Phases: {N}
  Files: plan.toml + {N} phase files
  Lifecycle: {choice}
```

### Phase 4.1: Validate Plan Before Execution (MANDATORY)

> **⛔ CRITICAL**: You MUST offer plan validation as the FIRST next step. Do NOT skip this. LLMs frequently forget this step — that's why it's mandatory.

After writing the plan, **before** generating the startup prompt:

1. **Self-validate** against `{cypilot_path}/.core/requirements/plan-checklist.md`
2. **Report validation results** to user:

```
═══════════════════════════════════════════════
Plan Self-Validation: {task-slug}
───────────────────────────────────────────────

| Category | Status |
|----------|--------|
| 1. Structural | PASS/FAIL |
| 2. Interactive Questions | PASS/FAIL |
| 3. Rules Coverage | PASS/FAIL |
| 4. Context Completeness | PASS/FAIL |
| 5. Phase Independence | PASS/FAIL |
| 6. Budget Compliance | PASS/FAIL |
| 7. Lifecycle & Handoff | PASS/FAIL |

Overall: PASS/FAIL
═══════════════════════════════════════════════
```

3. **If any category FAIL**: list specific issues and offer to fix them before proceeding
4. **If all PASS**: proceed to next steps

**Offer next steps** (MANDATORY — present ALL options):

```
What would you like to do next?

  [1] Validate plan thoroughly — run /cypilot-analyze on the plan
      (recommended before execution, catches issues self-validation may miss)
  
  [2] Start execution — begin with Phase 1
      (use the startup prompt below)
  
  [3] Review plan files — inspect phase files before execution
  
  [4] Modify plan — adjust phases, add/remove content
```

**Wait for user choice** before generating the startup prompt. Do NOT auto-proceed to execution.

### New-Chat Startup Prompt

After reporting the summary, generate a **copy-pasteable prompt** that the user can paste into a fresh chat to start execution. This is critical because plan generation may exhaust the current context window.

Output the prompt inside a **single fenced code block** so the user can copy it in one click:

````
To start execution, open a new chat and paste this prompt:
````

Then output:

````markdown
```
I have a Cypilot execution plan ready at:
  {cypilot_path}/.plans/{task-slug}/plan.toml

Please read the plan manifest, then execute Phase 1.
The phase file is self-contained — follow its instructions exactly.
After completion, report results and generate the prompt for Phase 2.
```
````

The entire prompt MUST be inside a single ` ``` ` code fence — no text mixed in. This makes it trivially copy-pasteable from any chat UI.

---

## Phase 5: Execute Phases

When the user requests phase execution:

### 5.1 Load Phase

1. Read `plan.toml` to find the next pending phase (respecting dependencies)
2. Update phase status to `in_progress` in `plan.toml`
3. Read the phase file
4. Follow the phase file instructions exactly — it is self-contained

### 5.2 Execute

The phase file contains everything needed. Follow its Task section step by step.

### 5.3 Save Intermediate Results

After completing the Task but before reporting:

1. Check `plan.toml` for this phase's `outputs` list
2. Verify each output file was created/updated during execution
3. If any output is missing, flag it in the report as a failure

Intermediate results in `out/` serve as the **data contract** between phases. If a phase fails to produce its declared outputs, dependent phases cannot execute.

### 5.4 Report

After completing the phase, produce the completion report in the format specified by the phase file's Output Format section.

### 5.5 Update Status

Update `plan.toml`:
- If all acceptance criteria pass: set status to `done`
- If any criterion fails: set status to `failed`, record the failure reason

```toml
[[phases]]
number = 1
title = "PRD Overview and Actors"
file = "phase-01-overview.md"
status = "done"
depends_on = []
completed = "2026-03-12T14:30:00Z"
```

### 5.6 Phase Handoff

> **Note**: If the phase file's Output Format section already included a handoff prompt (compiled from plan-template.md Section 9), this step is already done — do NOT generate a duplicate. This step is a fallback for phase files compiled before the handoff prompt was added to the template.

After reporting phase results, generate a **copy-pasteable prompt** for the next phase. This allows the user to continue in the same chat or start a fresh one if context is running low.

First output the status line as plain text:

````
Phase {N}/{M}: {status}

Next phase prompt (copy-paste into new chat if needed):
````

Then output the prompt inside a **single fenced code block**:

````markdown
```
I have a Cypilot execution plan at:
  {cypilot_path}/.plans/{task-slug}/plan.toml

Phase {N} is complete ({status}).
Please read the plan manifest, then execute Phase {N+1}: "{title}".
The phase file is: {cypilot_path}/.plans/{task-slug}/phase-{NN}-{slug}.md
It is self-contained — follow its instructions exactly.
After completion, report results and generate the prompt for Phase {N+2}.
```
````

The entire prompt MUST be inside a single ` ``` ` code fence — no text mixed in. Then ask:

````
Continue in this chat? [y] execute next phase here | [n] copy prompt above to new chat
(Recommended: new chat for guaranteed clean context)
````

#### Context boundary for continue mode

If user chooses **continue** (`[y]`), apply the context boundary **before** loading the next phase:

```
--- CONTEXT BOUNDARY ---
Previous phase execution is complete. Disregard all prior context.
Read ONLY the next phase file — it is self-contained.
Do not reference any information from before this boundary.
---
```

Then proceed to Phase 5.1 (Load Phase) for the next phase. The phase file on disk is the **sole source of truth** — the agent reads it fresh, not from memory.

**If last phase** (CRITICAL — do NOT forget this):

Instead of a next-phase prompt, you MUST:

1. **Report plan completion**:

```
═══════════════════════════════════════════════
ALL PHASES COMPLETE ({M}/{M})
───────────────────────────────────────────────
Plan: {cypilot_path}/.plans/{task-slug}/plan.toml
Target: {artifact kind or feature}
Phases completed: {M}
Lifecycle strategy: {lifecycle}
═══════════════════════════════════════════════
```

2. **Execute lifecycle strategy** per `plan.toml` setting:
   - `gitignore`: verify `.plans/` is in `.gitignore`
   - `cleanup`: delete the plan directory
   - `archive`: move to `.plans/.archive/{task-slug}/`
   - `manual`: remind user to clean up manually

3. **Ask about plan files** (MANDATORY — LLMs forget this):

```
Plan execution complete. What would you like to do with the plan files?

  [1] Keep — leave plan files for reference
  [2] Archive — move to .plans/.archive/ (gitignored)
  [3] Delete — remove plan directory entirely
  [4] Already handled — lifecycle strategy was {lifecycle}
```

4. **Offer validation of generated artifact**:

```
Would you like to validate the generated {artifact/code}?

  [1] Yes — run /cypilot-analyze on the output
  [2] No — done for now
```

### 5.7 Abandoned Plan Recovery

If a plan is abandoned mid-execution (user stops responding, context lost, session ends):

1. **The plan.toml serves as checkpoint** — all completed phases are marked `done`, failed phases marked `failed`
2. **To resume**: read plan.toml, find first `pending` or `in_progress` phase, execute it
3. **If `in_progress` phase has partial outputs**: verify them before continuing or re-execute the phase from scratch
4. **Recovery prompt** (user can paste this to resume):

```
I have an incomplete Cypilot execution plan at:
  {cypilot_path}/.plans/{task-slug}/plan.toml

Please read the plan manifest, check which phases are done/pending,
and resume execution from the first incomplete phase.
```

---

## Phase 6: Check Status

When the user asks for plan status:

1. Read `plan.toml`
2. Report:

```
Plan: {task description}
  Type: {type}
  Target: {target}
  Progress: {done}/{total} phases

  Phase 1: {title} — {status}
  Phase 2: {title} — {status}
  ...
  Phase N: {title} — {status}
```

If the plan has failed phases, suggest:
- Retry: "Retry phase {N}" — re-execute the failed phase
- Skip: "Skip phase {N}" — mark as skipped and continue (if no dependencies)
- Abort: "Abort plan" — stop execution

---

## Plan Storage Format

All plan data lives in `{cypilot_path}/.plans/{task-slug}/`:

```
.plans/
  generate-prd-myapp/
    plan.toml                    # Plan manifest with phase metadata
    brief-01-overview.md         # Compilation brief (generated by script)
    brief-02-requirements.md     # Compilation brief (generated by script)
    brief-03-usecases.md         # Compilation brief (generated by script)
    brief-04-synthesis.md        # Compilation brief (generated by script)
    phase-01-overview.md         # Self-contained phase file (compiled by agent)
    phase-02-requirements.md     # Self-contained phase file (compiled by agent)
    phase-03-usecases.md         # Self-contained phase file (compiled by agent)
    phase-04-synthesis.md        # Self-contained phase file (compiled by agent)
    out/                         # Intermediate results between phases
      phase-01-actors.md         # Actor list extracted in phase 1
      phase-01-id-scheme.md      # ID naming scheme decided in phase 1
      phase-02-req-ids.md        # Requirement IDs generated in phase 2
```

**Naming conventions**:
- Task slug: `{type}-{artifact_kind}-{project_slug}` (e.g., `generate-prd-myapp`)
- Phase file: `phase-{NN}-{slug}.md` where NN is zero-padded (01, 02, ...)
- Plan manifest: always `plan.toml`

**Cleanup**: Plan directories are ephemeral. The lifecycle strategy (set during plan creation) determines what happens after completion — see Phase 4.

---

## Execution Log

During plan generation and execution, maintain a brief log for observability:

```
[plan] Assessing scope: generate PRD for myapp
[plan] Estimated size: ~1200 lines → plan needed
[plan] Strategy: generate (by template sections)
[plan] Decomposition: 4 phases
[plan] Compiling phase 1/4: Overview and Actors
[plan] Phase 1 compiled: 380 lines (within budget)
[plan] Compiling phase 2/4: Requirements
[plan] Phase 2 compiled: 420 lines (within budget)
[plan] ...
[plan] Plan written: .plans/generate-prd-myapp/ (4 phases)
[exec] Phase 1/4: in_progress
[exec] Phase 1/4: done (all criteria passed)
[exec] Phase 2/4: in_progress
...
```

This log is output to the user during execution, not saved to disk.
