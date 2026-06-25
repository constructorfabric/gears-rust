---
name: matrix-task-comparison
description: Run a controlled matrix comparison of approaches over a set of tasks — e.g. the kitsoki pipeline vs a naive single prompt, across a harness/model candidate matrix — scoring outcome, compliance, cost, and time, adjudicating implementation-coupled oracles, then regenerating a report + slidey deck offline with zero re-spend. Use when the user says "bake-off", "compare kitsoki vs single-prompt", "run a matrix comparison", "which model/harness is best at X", "kitsoki-vs-X study", "benchmark these approaches on these tasks", or wants an evidence-backed, cost-accounted comparison of contenders across a model grid. Generalises tools/bugfix-bakeoff (the reference implementation).
---

# Matrix task comparison

Compare approaches on the **same** tasks under **identical** conditions, score
each cell on outcome / compliance / cost / time, roll up, and deck it. The
reference implementation — the worked first instance — is
[`tools/bugfix-bakeoff/`](../../../tools/bugfix-bakeoff/) (kitsoki's `bugfix`
pipeline vs a naive single prompt, across an Opus/Sonnet/GLM/GPT model grid, on
real fixed bugs). This skill is the reusable *method*; cite those files, do not
re-derive them, and do **not** hardcode the bug9/12/14 specifics into a new
study.

**Read first (the reference impl):**
- [`tools/bugfix-bakeoff/README.md`](../../../tools/bugfix-bakeoff/README.md) — the runbook.
- [`tools/bugfix-bakeoff/bakeoff.yaml`](../../../tools/bugfix-bakeoff/bakeoff.yaml) — the manifest shape.
- [`tools/bugfix-bakeoff/results/SCHEMA.md`](../../../tools/bugfix-bakeoff/results/SCHEMA.md) — the cell/summary contract every tool honors.
- [`.context/bakeoff-learnings.md`](../../../.context/bakeoff-learnings.md) — the gotchas (mirrored in "Pitfalls" below).
- `prepare.sh` · `run_cell.sh` · `score.py` · `aggregate.py` — the four pieces.

## The design — three axes → cells

A **cell** = `(task × candidate × contender)`. Three axes:

- **Structure axis (the contenders).** The approaches under test, e.g. kitsoki
  pipeline vs naive single-prompt+guidance; or two stories; or pipeline-vs-pipeline.
  (`bakeoff.yaml: treatments` → `[kitsoki, single]`.)
- **Candidate axis (harness/model).** Each `{key, profile, model, effort,
  provider, invoker}` — e.g. GLM-5.2 / Opus-4.8 / Sonnet-4.6 / GPT-5.5, each a
  configured kitsoki profile. (`bakeoff.yaml: candidates`.) `invoker` decides
  *how* a control cell runs: `claude_p` → `claude -p`; `session` → studio-MCP.
- **Task axis.** The tasks/cases — for the bake-off, real fixed bugs, each with a
  `baseline_sha`, a hidden `oracle_test`, and `affected_test_pkgs`.

**One hermetic worktree per cell** — never shared.
`prepare.sh` cuts `.worktrees/bakeoff-<task>-<candidate>-<contender>` detached at
the task's `baseline_sha`. A shared checkout *is* concurrent-checkout bug #9 —
hard-isolate by `(task, candidate, contender)`. `prepare.sh` is idempotent (clean
worktree at the right SHA = no-op; dirty/off-baseline refused without `--force`).

## MANDATORY pre-flight — confirm each baseline is genuinely RED

The #1 money-waster. The baseline must actually **exhibit the condition** the task
tests (the bug present, the test RED, or noncompile) *before* you spend a dollar.
Several "fixes" in the marathon were a test/lint added **on top of** an
already-merged behavioural fix, so `<fix>^` was already GREEN → a degenerate cell
that proves nothing. (#1/#2/#8 were dropped for exactly this; only #9/#12/#14
reproduced.)

For each task, run the oracle at the baseline and confirm RED *before* scheduling:

```bash
cd .worktrees/bakeoff-<task>-<any>-<any>     # any prepared worktree at baseline
# go oracle: copy the oracle in from the fix, run it, expect FAIL/noncompile
git show <fix_sha>:<oracle_test> > <oracle_test>
go test -run '^TestXxx$' ./path/to/oracle/pkg ; echo "rc=$?"   # MUST be non-zero
git checkout -- <oracle_test> 2>/dev/null; rm -f <oracle_test>  # leave tree clean
```

A baseline that is GREEN is a study finding (note it), not a cell to run.

## The hidden oracle + adjudication

- **Hidden oracle.** Each task's oracle = the real fix's own regression test,
  kept **out of the candidate's tree**. `score.py` copies it in from the fix
  commit (`git show <fix_sha>:<oracle>`), runs it, and **removes** it so the tree
  is never polluted. The candidate must never see it (that would leak the answer).
- **Oracles are often wording/impl-coupled** → they false-fail a behaviourally
  correct fix done a different way (Opus refused with different wording; the
  kitsoki pipeline used a per-session-path approach where the oracle asserted a
  sentinel — both correct). Prefer **behavioural** oracles when authoring.
- **Adjudication step.** When the oracle fails (or noncompiles) but the behaviour
  is plausibly correct, an LLM/human judge decides `solved|partial|failed` on
  **behaviour**. Record the override; keep the raw `oracle_status` so the JSON
  never lies:

  ```bash
  python3 tools/bugfix-bakeoff/score.py --bug <task> --candidate <cand> \
    --treatment <contender> --worktree <wt> --transcript <t> \
    --adjudication solved \
    --adjudication-note "per-session-path fix; oracle asserts the sentinel impl"
  ```

  This sets `outcome.quality`, `adjudicated=true`, and the note; `oracle_status`
  stays the raw automated result. Rollups key on the (possibly adjudicated)
  `quality`.

## Metrics + scoring rubric

Per `results/SCHEMA.md`, every cell scores three families:

- **Outcome** → `quality ∈ {solved, partial, failed}`:
  `solved` = oracle pass ∧ build ok ∧ affected suite green; `partial` = oracle
  pass with a regression/build issue, **or** oracle noncompiles against a
  differing-but-plausible impl; `failed` = oracle fail/absent. May be
  adjudicator-overridden.
- **Compliance checklist** (five best-effort heuristics, mean = `rate`):
  `reproduced_red`, `added_regression_test`, `suite_green`, `in_scope`,
  `stage_order`. **Diff `baseline..HEAD` ∪ working tree**, not just `git status` —
  candidates often *commit* their fix+test, leaving status clean (`changed_files`
  in `score.py`).
- **Cost — one consistent basis.** This is the axle the whole study turns on:
  - **kitsoki traces** carry an authoritative per-call `payload.meta.cost_usd` —
    **sum the native figure** (`extract_kitsoki` prefers it; exact by construction).
  - **`claude -p` subscription transcripts carry NO cost** → price from
    `message.usage` via a **correct** rate table. The current rates: **Opus
    $5/$25** (cache 0.5 / 6.25 / 10), **Sonnet $3/$15**. `pricing.py`'s Opus row
    was historically stale at 15/75 — verify it before trusting USD. The corrected
    table reproduces kitsoki's native cost to ~0.4%.
  - **Tokens are the provider-neutral primary axis; USD second.** `cost_extract`
    only reads Claude Code `message.usage` (returns zero on kitsoki traces); so
    `score.py` *sniffs* format (top-level `kind` ⇒ kitsoki) and dispatches.
  Also: `wall_time_s` and `guidance_turns`.

## Running cells (the cost-bearing step — manual, never CI/auto)

`run_cell.sh` is the **only** cost-bearing piece. `prepare.sh`, `score.py`,
`aggregate.py` make no LLM calls. Per contender/invoker:

- **Naive single / `claude_p`** (Opus, Sonnet) — fully scripted:
  ```
  claude -p "$(cat prompts/<task>.md)" --output-format json --model <model> \
    --permission-mode acceptEdits \
    --allowedTools Bash Edit Write Read Glob Grep MultiEdit
  ```
  The scoped allowlist is mandatory — the classifier blocks
  `--dangerously-skip-permissions`; worktrees are disposable. Resume guidance
  turns with `claude -p --resume <sid> "<msg>"`.
- **kitsoki / MCP cells** (and single/`session` for GLM, GPT) — studio-MCP
  `session_new` under the candidate's **profile** (the profile/agent-def controls
  the maker model — see Pitfalls). Pass:
  - `profile: <candidate.profile>`, `harness: "live"`,
  - an explicit **`trace:`** (otherwise the trace goes to a random temp file —
    the filed P1: `session_new` uses `os.CreateTemp`),
  - `initial_world.base_branch` / `base` = the **baseline SHA**, so the pipeline
    cuts from the buggy parent, not main (where the bug is already fixed),
  - a **scoped `test_cmd`** = the changed-area packages (a repo with pre-existing
    unrelated reds bounces every fix forever; the authoritative grade is your own
    oracle, not the pipeline's internal CI).
- **Guidance turns** are operator-driven, **oracle-gated**, and **counted** (cap
  e.g. 5). Give fair behavioural feedback (what a reviewer running the scenario
  sees) *without* revealing the hidden oracle. The worktree is **reused** on a
  resumed turn — do not re-prepare.

## Aggregation + offline regeneration (zero re-spend)

The committed `summary.json` (+ per-task `agenteval` reports) makes the study
fully reproducible — the report/deck regenerate with **no LLM calls**:

```bash
cd tools/bugfix-bakeoff
python3 aggregate.py --generated-at 2026-06-24T00:00:00Z   # cells/*.json -> summary.json
python3 aggregate.py --generated-at 2026-06-24T00:00:00Z --emit-agenteval  # + agenteval/<task>/latest.json
python3 ../session-mining/eval_pilot_report.py \
  --summary results/report.json --markdown results/report.md --deck results/deck.html
```

(`--generated-at` is required — the build bans implicit wall-clock timestamps;
`BAKEOFF_GENERATED_AT` works too. `--markdown`/`--deck`/`--summary` each take a
path.) `summary.json` and the `agenteval.Report` files are the durable artifacts.

## Runbook (step by step)

1. **Setup.** Pick the structure × candidate × task axes. Confirm each candidate
   profile exists (`.kitsoki.local.yaml`) and the model alias prices in
   `pricing.py`.
2. **Manifest.** Author `bakeoff.yaml`: `bugs`/tasks (`baseline_sha` = `<fix>^`,
   hidden `oracle_test`, `affected_test_pkgs`), `candidates`, `treatments`,
   `run.max_guidance_turns`. Copy the reference manifest's shape.
3. **Pre-flight.** For every task, prove the baseline is RED (above). Drop
   degenerate (GREEN-at-baseline) tasks; record the drop as a finding.
4. **Run cells** (COST, manual). `prepare.sh` → `run_cell.sh` per cell; iterate
   oracle-gated guidance turns up to the cap.
5. **Score.** `score.py` per cell → `results/cells/<task>-<cand>-<contender>.json`.
6. **Adjudicate.** Re-score with `--adjudication`/`--adjudication-note` where a
   coupled oracle false-failed a behaviourally-correct fix.
7. **Aggregate.** `aggregate.py [--emit-agenteval]` → `summary.json`.
8. **Report + deck.** `eval_pilot_report.py --markdown --deck` (offline).

## How this connects to the `task-bakeoff` story

A kitsoki story `stories/task-bakeoff/` (being built in parallel) wraps this
method into a **drivable workflow** that produces the slidey report directly:
the rooms encode Setup → manifest → pre-flight → run cells → score → adjudicate →
aggregate, calling the same `tools/bugfix-bakeoff/*` scripts as host steps, and
the final room renders the deck (the `--deck` HTML / a slidey spec). When that
story lands, drive it with `kitsoki-mcp-driver`; until then run this skill's
manual runbook. Keep the scripts the single source of truth — the story orchestrates
them, it does not reimplement scoring/pricing.

## Common pitfalls (from the learnings doc)

1. **Degenerate baseline** (#1 above) — GREEN-at-baseline tasks prove nothing.
2. **Shared checkout** — one worktree per cell, always; sharing is the bug.
3. **Status-only compliance** — candidates commit their work; diff
   `baseline..HEAD` ∪ working tree.
4. **Wrong/stale price table** — verify Opus $5/$25, Sonnet $3/$15 before trusting USD.
5. **cost_extract returns zero on kitsoki traces** — sniff format; prefer native
   `meta.cost_usd`.
6. **Maker model lives on the profile / story agent-def**, not magic — the
   `session_new profile:` supersedes a story `model:`; pin a sonnet profile for
   the sonnet cell. Relative imports read disk.
7. **No explicit `trace:`** → trace lost to a temp file (filed P1).
8. **Driving on main, not the baseline** → nothing to reproduce.
9. **Unscoped CI `test_cmd`** → pre-existing reds bounce every fix forever; your
   oracle is the authoritative grade.
10. **claude -p needs the scoped allowlist** (`--permission-mode acceptEdits
    --allowedTools "Bash Edit Write Read Glob Grep MultiEdit"`); the classifier
    blocks `--dangerously-skip-permissions`.
11. **Oracle leakage** — never let a candidate see the hidden oracle; copy it in
    only at scoring, then remove.
12. **Coupled oracle false-fails** — adjudicate on behaviour; keep raw `oracle_status`.
13. **MCP-driven sessions may not journal a discoverable local trace** —
    `run_cell.sh --locate` reports `trace_found=false`; that absence is a finding,
    not a script bug.

## No-LLM testing rule (load-bearing — [AGENTS.md](../../../AGENTS.md))

`run_cell.sh` is the only cost-bearing piece and is run **manually**, never in CI
or automatically. `prepare.sh`/`score.py`/`aggregate.py` are deterministic and
free; the reference impl ships ~22 offline tests against fixture transcripts/
worktrees (oracle runner + cost extractor are dependency-injected). The committed
`summary.json` lets the whole study re-derive its report/deck with zero spend.

## Maintenance

Codex discovers this skill directly. After adding/moving it, re-link into Claude
Code's `.claude/skills/`:

```
make setup
```
