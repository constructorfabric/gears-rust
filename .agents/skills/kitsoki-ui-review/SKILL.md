---
name: kitsoki-ui-review
description: Heuristic UI layout & usability review of the kitsoki web UI. Walks the onboarding tour manifest at mobile/tablet/desktop, runs a deterministic DOM-geometry + axe-core audit per step, then fans the frames out across MULTIPLE read-only claude vision agents (one per surface, so no single agent holds every image) that critique each surface against a Nielsen-based heuristic catalog, adversarially re-checked, and emits a gated review-report.md + verdict.json with concrete fixes. Use when asked to review / critique / find layout, usability, responsive, or accessibility problems in the kitsoki web UI — distinct from kitsoki-ui-qa (which checks "does the demo SHOW scenario X").
---

# Kitsoki UI layout & usability review

Where [[kitsoki-ui-qa]] asks *"does the demo demonstrate scenario X?"*, this skill
asks *"is this UI well laid-out and usable?"* — a **heuristic critique** of the
live web UI that returns prioritised, frame-cited, actionable findings (with
fixes) to improve it efficiently.

The loop is **"we develop a tour, the agents review it."** The capture stage
reuses the onboarding **tour manifest** (`src/tour/manifest.ts`) as the script of
surfaces worth reviewing — so to put a new screen in front of the reviewers you
just add a tour step (it already drives the live overlay, the demo video, and
this review — [[project_onboarding_tour_generic]]).

> LLM-driven review **by design** (it needs vision). It is NOT a no-LLM flow test
> and must never be wired into the automated suite (CLAUDE.md,
> [[feedback_no_llm_tests]]). It uses the local `claude` CLI, so — like the
> engine's oracle — there's no API key and no per-call cost
> ([[project_oracle_uses_claude_cli]]). Stage 1 (capture) and stage 3 (report)
> are fully deterministic and testable without any LLM.

## Why it's reliable (read this first)

1. **Deterministic evidence + deterministic truth.** Stage 1 walks the tour at
   each viewport and, per step, captures a frame AND runs two no-LLM audits: a
   **DOM-geometry probe** (`tests/playwright/lib/ui-audit.ts` — page horizontal
   scroll, off-screen clipping, clipped/truncated content, tiny text, tiny tap
   targets, stray `{{template}}` tokens) and **axe-core** (WCAG contrast, labels,
   ARIA). These are measured from the real laid-out DOM and treated as ground
   truth — the vision pass never has to guess at them.
2. **Many small agents, not one big one.** Stage 2 shards the frames (default:
   one tour step = that surface across all viewports, ~3 frames) and fans them
   out across several **independent read-only `claude` vision agents** running in
   parallel. No single agent's context holds the whole frame set — each judges a
   coherent handful against the heuristic catalog, with that surface's audit
   findings handed in as already-known truth.
3. **Grounded + adversarially checked.** Every finding must cite a frame and
   quote what is literally visible; a per-shard skeptic then re-checks each
   agent's own frames and may **only downgrade/drop** (kills false positives,
   which vision models over-produce on layout).
4. **Authoritative gate.** Stage 3 merges the deterministic + vision findings and
   recomputes pass/fail from severities (it does *not* trust any model's verdict):
   `error` always blocks, `warn` blocks under `--strict`, `info` never blocks.

## Prerequisites

`pnpm`, `ffmpeg`-free (no video here), `jq`, and the `claude` CLI on PATH. The
`@axe-core/playwright` dev dep is installed in `tools/runstatus`. Stage 1 runs
`make build` to embed the current SPA (override with `--no-build`).

## The loop

1. **(Optional) write the design intent.** Grounds the agents in what the UI is
   *for* so they don't apply a generic aesthetic. Copy and edit:
   ```bash
   cp .agents/skills/kitsoki-ui-review/templates/design-intent.example.md .context/ui-intent.md
   ```

2. **Run the review** (capture → multi-agent review → gated report):
   ```bash
   .agents/skills/kitsoki-ui-review/scripts/ui-review.sh \
     --design-intent .context/ui-intent.md
   echo "gate exit: $?"        # 0 pass · 1 blocking finding · 2 pipeline error
   ```
   Artifacts land in `.artifacts/ui-review/` ([[feedback_artifacts_dir]]):
   `frames/`, `audit.json`, `vision.json`, `verdict.json`, `review-report.md`.

3. **Read `review-report.md`.** Findings grouped error → warn → info, each with
   the surface, viewport, source (`geometry` / `a11y` / `vision`), the check,
   a literal observation, a concrete fix, and the cited frame. Open the cited
   `frames/NN-<step>@<viewport>.png` to confirm before acting.

4. **Iterate.** Fix the UI, re-run. To review a NEW surface, add a step to the
   tour manifest. To teach the review a new defect class, add a check to
   `heuristics.yaml` (the extensible surface) — or, for a *measurable* rule, add
   it to `lib/ui-audit.ts` (deterministic + free).

### Faster / scoped runs

```bash
S=.agents/skills/kitsoki-ui-review/scripts
$S/ui-review.sh --viewports desktop                 # one viewport (fast)
$S/ui-review.sh --no-build --no-capture --strict    # re-review existing frames
$S/ui-review.sh --model claude-sonnet-4-6 --jobs 6   # cheaper/faster, more parallel
$S/capture.sh --fast --viewports desktop             # capture only (no LLM)
```

## The tools (`scripts/`)

| Script | Stage | Does | LLM? |
|---|---|---|---|
| `ui-review.sh [--viewports …] [--design-intent F] [--model M] [--jobs N] [--shard step\|viewport] [--no-adversary] [--strict] [--no-build] [--no-capture]` | all | One-shot wrapper; exit code is the gate | via review |
| `capture.sh [--no-build] [--viewports a,b,c] [--fast]` | 1 | `make build` + run the `tour-review` spec → `frames/` + `audit.json` | no |
| `review.sh --audit A --frames D --heuristics H --out vision.json [--design-intent F] [--model M] [--jobs N] [--shard step\|viewport] [--no-adversary]` | 2 | Shard frames → parallel read-only vision agents → adversarial re-check → merged `vision.json` | **yes** |
| `report.sh --audit A --vision V --out report.md --verdict verdict.json [--strict]` | 3 | Merge deterministic + vision findings → report + recompute gate exit | no |

Defaults: model `claude-opus-4-8`; `--jobs 4`; shard by `step`; adversary on;
viewports `desktop,tablet,mobile`.

## Capture spec & audit (the deterministic half)

- `tools/runstatus/tests/playwright/tour-review.spec.ts` — the generalized tour
  driver. Walks `TOUR_STEPS` at each viewport; the **primary** (first) viewport
  keeps the strict anti-drift title assertion, secondary viewports run
  best-effort so a tour that *stalls* at a narrow width becomes a `tour-stalled`
  finding instead of crashing the capture. Reuses `_helpers/server.ts`
  (no-LLM `--flow` posture) — same harness as [[kitsoki-ui-demo]].
- `tools/runstatus/tests/playwright/lib/ui-audit.ts` — the in-browser geometry
  probe (high-precision on purpose; see its header for each check + FP guard).

## verdict.json shape

Every finding carries the **DOM state** it was seen in and a **reproduction
recipe**, so a fix can be made (by a human or a fix-it agent) without re-running:

```json
{ "overall":"pass|fail", "strict":false,
  "server":{"cmd":"bin/kitsoki web --stories-dir … --flow …","base":"http://127.0.0.1:7746"},
  "summary":{"error":0,"warn":0,"info":0,"blocking":0,
             "by_source":{"geometry":0,"a11y":0,"vision":0}},
  "findings":[
    {"source":"vision|geometry|a11y","check":"a11y:color-contrast","severity":"error",
     "surface":"home-welcome","viewport":"desktop",
     "frame":"01-home-welcome@desktop.png", "count":1, "frames":["…"],
     "detail":"<literal observation>","recommendation":"<fix>",
     // DOM context (geometry + a11y; empty for vision, which only sees pixels):
     "selector":"[data-testid=\"session-filter-active\"]",
     "path":"div#app › div[home-view] › … › button[session-filter-active]",
     "rect":{"x":339,"y":253,"w":63,"h":21},
     "styles":{"fontSize":"12px","color":"rgb(100,116,139)","backgroundColor":"…", "...":"…"},
     "html":"<button class=\"home__filter-chip\" …>Active</button>",
     // a11y-only (from axe): the exact failing measurement + the rule doc:
     "failureSummary":"insufficient contrast 3.75 (fg #64748b / bg #0f172a) — expected 4.5:1",
     "helpUrl":"https://dequeuniversity.com/rules/axe/…",
     // how to get back to it:
     "repro":{"cmd":"…","base":"…","viewport":"1440x900","route":"home",
              "step":"home-welcome","stepTitle":"Welcome to kitsoki","url":"…"}}]}
```

`review-report.md` mirrors this: a quick-scan index table per severity, then a
**Details — how to reproduce & fix** section with one card per error/warn finding
(its DOM state, computed styles, outerHTML, and the numbered reproduction recipe).
Info-level findings (e.g. the flood-prone tiny-text/tiny-tap-target) stay in the
JSON with full context but are not expanded as cards, to keep the report skimmable.

## Pointers

- The isolated driver: the `kitsoki-ui-review` agent (`.claude/agents/`) runs this
  pipeline in its own context and returns only the gated verdict, so the frames
  never enter the calling session.
- **Found problems? Drive `stories/ui-fix`** to review findings into root-cause
  groups, fix each group with a scoped agent, prove it cleared with a re-audit,
  and record a before/after media artifact per group. See
  [`docs/stories/ui-fix.md`](../../stories/ui-fix.md).
- Sibling skills: [[kitsoki-ui-demo]] (records a video), [[kitsoki-ui-qa]]
  (scenario validation), `kitsoki-web-debug` (when the UI is actually broken/500ing).
- The vision agent is the local `claude` CLI: `internal/host/oracle_runner.go`.

## Maintenance

Codex discovers this skill directly. Refresh the project-local Claude Code
symlink after adding or moving skills:

```
make setup
```
