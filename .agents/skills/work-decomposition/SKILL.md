---
name: work-decomposition
description: Turn an accepted kitsoki proposal (or an epic + its children) into a validated, reviewed YAML decomposition of right-sized agent briefs. Use when the user wants to decompose work into briefs, "break this proposal into tickets/tasks", produce a decomposition.yaml, or check out the brief-manifest shape from docs/proposals/work-decomposition.md before that pipeline is built as a story. Drives discovery → manifest → deterministic structural lint → adversarial feasibility/completeness review, by hand.
---

# Work decomposition

This skill is the **Claude-driven version** of the pipeline proposed in
[`docs/proposals/work-decomposition.md`](../../proposals/work-decomposition.md):
it takes an *accepted* proposal (or an epic + its linked children) and produces
a `decomposition.yaml` of agent briefs — structurally validated and
adversarially reviewed — so you can **check out the manifest shape and the
slicing quality before the `stories/decompose/` story is built**.

The same discipline the proposal encodes applies here, just run by you instead
of the engine: the **interpretive** decisions (how to slice, is it feasible, is
it complete) are your judgment; the **structural** truth (unique ids, acyclic
DAG, coverage, path bounds) is a deterministic script whose exit code is the
gate — the kitsoki moat: separate interpretation from deterministic execution.

> Scope: this **decomposes** an accepted proposal. Authoring/scoping the
> proposal itself is the `proposal-authoring` skill. Implementing a single
> brief is the `stories/implementation/` pipeline (and `kitsoki-story-authoring`
> for story-layer work). This skill stops at a reviewed `decomposition.yaml`.

## The output shape — the heart

The deliverable is a manifest matching
[`schemas/decomposition.json`](schemas/decomposition.json): a `coverage_note`
plus a `briefs[]` array, each brief carrying `id, title, kind, goal, scope[],
depends_on[], acceptance[], test_plan, agent_brief` (and optional `risk`).

```yaml
coverage_note: >
  These four briefs fully cover the proposal: #1 adds the engine seam, #2 the
  story that uses it, #3 the TUI surface, #4 the docs+flow fixtures. Nothing in
  the proposal's "What changes" is left unowned, and no two briefs touch the
  same files.
briefs:
  - id: engine-once-flag
    title: Add `once:` guard to room on_enter effects
    kind: runtime
    goal: A room with `once: true` re-runs its on_enter effects at most once per visit, surviving /reload.
    scope: ["internal/machine/", "internal/app/imports.go"]
    depends_on: []
    acceptance:
      - "A reloaded room with once:true does not re-fire its on_enter oracle call"
      - "Existing rooms without once: behave unchanged"
    test_plan: "internal/machine/machine_test.go: new TestOnceGuard; existing flow fixtures stay green."
    agent_brief: >
      Implement an engine-level `once:` boolean on room on_enter effect lists...
      (self-contained — everything the implementer needs, no external context).
    risk: medium
```

`kind` ∈ `story | runtime | tui | tracing | test | docs`. `scope` globs are the
**write boundary** for the brief; new-file briefs legitimately point at paths
that don't exist yet (the validator bounds them rather than requiring a match).

## The loop

Run these phases in order. They mirror the proposal's rooms; each gate must
pass before the next.

### 1. Load the source

Resolve the source the user named (a `docs/proposals/<slug>.md`, or an epic).
Read the full body. **Detect epic vs. focused**: an epic has `**Kind:** epic`
and a **Slices** table over linked children — if so, read *every* linked child
proposal too, and warn (non-fatal) on any child file that's missing or renamed.
Confirm back to the user: the title, kind, and (for an epic) the child count, so
they know you pointed at the right thing.

### 2. Discovery — sharpen the scope (interactive)

Before slicing, **have a short conversation** to pin the constraints a static
read can't give you:

- What must be **sequential** vs. what can be **parallel**?
- The **test strategy** (flow fixtures? go tests? manual checks?).
- **Risk areas** and explicit **non-goals** (often already in the proposal —
  confirm, don't re-derive).
- Any files/subsystems that are **off-limits** or owned elsewhere.

Capture the answers in `.context/decompose-<slug>-scope.md` (transient working
notes, per CLAUDE.md). Don't skip this even when the proposal looks complete —
the slicing quality lives here. Stop when you can state the slicing rationale in
two sentences.

### 3. Decompose — emit the brief manifest

Produce the manifest. Slicing rules:

- A brief is **right-sized** when it has one coherent goal, fits one
  implementer's head, and could land alone or behind one named dependency. Cut
  along **kind boundaries first** (runtime substrate → story → tui, tracing
  where its events are produced), then along shippable units within a kind.
- `depends_on` is the real build order — be honest; a missing edge is what the
  reviewer and the board will trip on.
- **No file double-ownership**: two briefs' `scope` globs should not overlap on
  the same file. Overlap is a slicing bug.
- `agent_brief` is **self-contained** — the implementer agent gets only that
  text plus the ticket fields, not this conversation. Spell out the approach,
  the key files, and the done-condition.
- The `coverage_note` is your **completeness claim** — write it to be attacked:
  state explicitly how the briefs together cover every item in the proposal's
  "What changes", with nothing unowned.

Write it to `.artifacts/decompose/<slug>/decomposition.yaml` (generated
artifact, not committed — CLAUDE.md).

### 4. Validate — the deterministic gate

Run the bundled validator. Its **exit code is the gate** — do not proceed to
review on a non-zero exit:

```
python3 .agents/skills/work-decomposition/scripts/validate_decomposition.py \
    .artifacts/decompose/<slug>/decomposition.yaml --repo-root .
```

It checks schema shape, **unique ids**, **dangling `depends_on`**, an **acyclic
dependency DAG**, **scope paths bounded inside the repo** (parent dir exists),
and **non-empty acceptance + test_plan** per brief. On failure: fix the manifest
(go back to step 3 with the errors as the brief) and re-run until clean. This
gate has teeth independent of any LLM — trust it over your own read.

There is also a Starlark equivalent (`validate_decomposition.star`) that covers
the pure-logic checks (unique ids, acyclic DAG, acceptance/test_plan) and can be
called via `host.starlark.run` from a story room — pass the parsed manifest as
`inputs.manifest`. It does **not** cover JSON-schema shape or scope-path bounds
(those need filesystem access and the `jsonschema` library; use the Python script
for those).

### 5. Adversarial review — feasibility + completeness

Now switch hats and **attack your own manifest as a skeptic** (or, for a real
second opinion, spawn a sub-agent with the Agent tool to do it independently —
that's closer to the proposal's `decomp_adversary`):

- **Per brief:** is this actually buildable *as scoped*? Are its deps right and
  complete? Is anything impossible, hand-wavy, or secretly two briefs?
- **Across briefs:** do they *fully* cover the proposal — **attack the
  `coverage_note`**, name anything in "What changes" that no brief owns. Is
  there file overlap / double-ownership?
- **Default to `revise` when uncertain.** A confident "looks good" with no
  attempted refutation is not a review.

Emit a verdict: `{verdict: accept | revise, reason, questions[]}`. On `revise`,
fold `questions[]` back into step 3 and re-run validate + review. Budget the
loop (≈5 passes); if it won't converge, stop and tell the user *why* —
unconverged slicing is a signal the proposal isn't decomposable as written.

### 6. Hand back

On `accept`, present:

- the path to the validated `decomposition.yaml`,
- a one-line **board**: `status · id — title (deps)` per brief in dependency
  order, with the parallelizable briefs called out,
- the `coverage_note` and the review verdict + reason.

Note for the user that each brief is shaped to become a ticket the
`stories/implementation/` pipeline can build (`id/title/body = agent_brief +
acceptance + scope + test_plan`) — which is exactly what the proposal's
`dispatch` room will mint once the story ships.

## Quick reference

| Step | Mechanism | Gate |
|---|---|---|
| Load proposal/epic | read `docs/proposals/<slug>.md` (+ children) | right source confirmed |
| Discovery | short conversation → `.context/` scope note | slicing rationale in 2 sentences |
| Decompose | emit manifest → `.artifacts/decompose/<slug>/decomposition.yaml` | schema-shaped |
| **Validate** | `validate_decomposition.py` (CLI) or `host.starlark.run validate_decomposition.star` (story room) | **exit code 0 / ok: true** |
| **Review** | skeptic pass (or sub-agent) | **verdict: accept** |
| Hand back | board + coverage note + verdict | — |

## Maintenance

Codex discovers this skill directly. Refresh the project-local Claude Code
symlink after adding or moving skills:

```
make setup
```

When the `stories/decompose/` story from the proposal ships, the
`schemas/decomposition.json` here and `stories/decompose/schemas/decomposition.json`
should stay identical — they're the same contract; reconcile them rather than
letting them drift.
