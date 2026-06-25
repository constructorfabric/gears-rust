---
name: kitsoki-ui-qa
description: Validate UI evidence (a screenshot for simple cases, a video for complex flows) against the bug or plan being verified plus usage scenarios — the inverse of kitsoki-ui-demo. Picks the evidence form by complexity, extracts deterministic frames, has a read-only `claude` vision agent judge each scenario against cited frames AND whether the evidence is complete for the stated bug/plan, adversarially re-checks every pass, and emits a gated qa-report.md + verdict.json. Use when asked to QA / review / validate / sign off on a demo, walkthrough, screenshot, or bug-fix proof, or to gate one in CI.
---

# Kitsoki UI demo QA

The **inverse** of [[kitsoki-ui-demo]]: that skill *produces* visual evidence;
this one *validates* it. Given **the bug or plan being verified**, a list of
**usage scenarios**, and the **evidence** (a screenshot for a simple case, a
video for a complex flow — or pre-extracted frames), it decides — with cited
evidence — whether the demo actually demonstrates each scenario, and exits
non-zero if a required one doesn't, so it can gate a release.

## Evidence is judged against the bug/plan — never in a vacuum

This skill **requires the bug or plan** as its `--feature` input (it is the
spec, not just background prose). The vision review answers two questions, not
one:

1. Does each scenario step appear, grounded in a cited frame? (the per-step
   verdict)
2. **Is the evidence complete and relevant for *this* bug/plan?** Evidence that
   is well-formed but doesn't actually exercise the changed behaviour — a video
   of an unrelated flow, a screenshot of the wrong state, a "before" that never
   shows the "after" the fix promises — is `unsupported`, even if every frame is
   crisp. A demo can be a perfectly good video and still be the wrong evidence.

So write the `--feature` file as the *actual* bug report or implementation plan
(what changed, what the user should now see), not a generic feature blurb. The
reviewer uses it to decide whether the screenshot/video proves the fix, not just
whether the UI rendered.

## Pick the evidence: screenshot vs video

Choose the *cheapest evidence that actually proves the change*, then QA that:

- **Simple, single-state cases → a Playwright screenshot.** If the bug/plan is
  fully verifiable from one (or a few) static frames — a badge label, a fixed
  layout, an element that should/shouldn't render, a color/spacing fix — capture
  a screenshot and QA it. No video needed; a screenshot is faster, smaller, and
  deterministic. Add a `page.screenshot({ path })` to a Playwright spec driving
  the relevant scene (the demo helpers in
  `tools/runstatus/tests/playwright/_helpers/demo.ts` stage scenes
  deterministically), or reuse the `NN-<scene>.png` captures the recorder
  already emits. Drop the PNG(s) into a dir and pass `--frames <dir>`.
- **Complex flows / multi-state transitions → a video.** If proving the
  bug/plan needs *motion* — a state badge advancing turn-by-turn, a streaming
  bubble appearing then resolving, a modal opening on an async event, an
  ordering/timing behaviour — a screenshot can't show it. Record a video with
  [[kitsoki-ui-demo]] and QA the video (frames are extracted deterministically).
  When in doubt about whether one frame can carry the claim, use a video.

**How to create the evidence** (and keep it relevant): see [[kitsoki-ui-demo]]
for the full recording pipeline (deterministic, no-LLM, MP4 + per-scene
`NN-<scene>.png`). Whichever form you pick, it **must exercise the feature being
implemented or the bug being fixed** — drive the specific room/intent/state the
bug/plan names, not a generic onboarding tour. Evidence that doesn't touch the
changed behaviour will be flagged `unsupported` by the review above.

> This is an **LLM-driven review tool by design** (it needs vision). It is *not*
> a no-LLM flow test and must never be wired into the automated test suite
> (CLAUDE.md, [[feedback_no_llm_tests]]). It uses the local `claude` CLI, so —
> like the engine's oracle — there's no API key and no per-call cost
> ([[project_oracle_uses_claude_cli]]). The two deterministic stages
> (`extract-frames.sh`, `report.sh`) are testable on their own without any LLM.

## Why it's reliable (read this first)

Video QA is unreliable when a model free-associates about UI it never saw. The
pipeline removes that failure mode structurally, not by hoping the model behaves:

1. **Deterministic evidence.** `extract-frames.sh` is pure ffmpeg — scene-change
   detection (the meaningful moments in a UI demo are state transitions) plus a
   periodic floor so static dwells aren't missed. Same video + flags → same
   frames + same `frames.json`. The frames are the *only* admissible evidence.
2. **Grounded verdicts.** Every `pass` MUST cite a frame filename and quote what
   is **literally visible**. A claim with no citable frame is `unsupported`
   (never silently `pass`); a frame that contradicts it is `fail`.
3. **Adversarial re-check (interpretive ÷ deterministic).** A second `claude`
   pass plays skeptic: it re-reads each `pass` step's cited frame and emits only a
   small **list of downgrades** (which step, to `fail`/`unsupported`, and what the
   frame really shows). `qa-review.sh` then **applies them deterministically** — it
   can only *lower* a status, never raise one (the downgrade-only invariant is
   enforced in code, not by trusting the model), and recomputes every
   scenario/`overall`/summary itself. The tiny delta output (vs. re-emitting the
   whole multi-KB verdict) is what makes this pass robust. Model output is parsed
   with a brace-matching JSON extractor that tolerates stray prose / ``` fences.
   The result is recorded as `adversary: {status, downgrades_applied}` on the
   verdict. (`--no-adversary` to skip.)
4. **Authoritative gate.** `report.sh` recomputes pass/fail from the per-scenario
   status in `verdict.json` (it does *not* trust the model's own `overall`) and
   sets the exit code. Under `--strict` it additionally blocks if the adversarial
   pass was supposed to run but did not complete (`adversary.status != "ok"`), so
   a silent adversary flake can never pass a strict gate.
5. **Visual-integrity check.** A demo can render a feature's frame as a large
   blank/uniform box (an all-white screenshot pane, a broken-image glyph, an empty
   preview) and still "show the right region." The review prompt treats a blank
   where visual content is expected as `fail` (not `pass`), the adversary
   specifically re-checks visual `pass` steps against their frame, and any
   `visual_issues[]` the model reports (proactively, even when no scenario names
   the region) **block the gate at every effort level** — surfaced in a "Visual
   issues" table in `qa-report.md`. Backing this is a **deterministic** layer:
   `blank-scan.sh` (pure ffmpeg + python, no LLM) flags any frame with a large
   contiguous flat block of a single colour — *any* colour, not just white/black,
   excluding the page background — or a near-empty frame. It runs on every
   `qa.sh` invocation; flags are advisory (a flat block can be a legit empty
   panel) unless `--blank-strict`. Together they stop a silently-broken image
   from passing QA — the LLM catches context, the scan catches what a model
   might gloss over.
6. **Annotation-consistency check.** A demo must narrate with ONE mechanism
   throughout — *either* tour popovers (a titled card with a "Step N of M"
   counter and Back/Next/Skip, anchored to a spotlight ring) *or* banner/caption
   overlays (a flat title+subtitle strip, no Next affordance). Both styles are
   legitimate on their own; the invariant is CONSISTENCY, not "must be tour". A
   single video that MIXES the two — a tour-popover intro that then hands off to
   banner captions for the feature payload — is the defect (it reads as two demos
   stitched together, and a scenario set written to bless the captions will never
   flag it). The review prompt (EVIDENCE RULE 6) classifies the narration style in
   every frame and, if BOTH styles appear across the video, emits a top-level
   `annotation_issues[]` (`{frame, styles_seen, issue}`); a single consistent
   style → empty array. Any `annotation_issues` **block the gate at every effort
   level** (same treatment as `visual_issues`) and are surfaced in an "Annotation
   issues" table in `qa-report.md`. A required `annotation-consistent` scenario in
   the scenario file pins the expectation in the per-step verdict as well.
7. **Pacing check (deterministic, no LLM).** A demo is only good if each narrated
   moment stays on screen long enough to read — and the *vision* pass is blind to
   this: every individual frame still looks correct, so a tour where every popover
   flashes by in 70ms passes clean (the classic footgun: recording with
   `WEB_CHAT_PACE=0`, the fast-validation posture, collapses every
   `dwell(step.dwellMs)` to ~0, yielding a 12-second blur instead of a readable
   ~80-second walk). `pacing-scan.sh` (pure `jq`) reads the recorder's **chapter
   sidecar** (`<video>.chapters.json`, emitted by `ChapterRecorder`/`writeChapters`
   — see [[kitsoki-ui-demo]]), where each tour step carries the `[start_ms,end_ms]`
   window it actually occupied in the final MP4, and flags any chapter whose
   duration is below the readable floor (`--pacing-min`, default 1500ms; also a
   total-span floor). Same video in → same flags out. `qa.sh` auto-detects the
   sidecar beside the MP4 and runs it on every invocation; flags are **advisory**
   (surfaced in a "Pacing warnings" table, never block) unless `--pacing-strict`,
   which promotes them to a blocking gate — the same advisory/strict shape as
   `blank-scan`. This is the deterministic catch for "the pacing is terrible" that
   no amount of frame-by-frame vision review can see.
7b. **Embedded-rrweb pacing check (deterministic, no LLM).** A tour can be embedded
   as a native **rrweb** clip (a slidey `video` scene replaying a `.rrweb.json`
   log) rather than an MP4 — and then BOTH prior pacing defenses are blind: there
   is no chapter sidecar for `pacing-scan.sh` to read, and the frame sampler sees
   each end-state frame looking correct. The defect this misses: a captured
   conversation plays fine for most of its length, then the **last few messages /
   the final artifact all flush in under a second** — "the last 3-5 messages are
   super-rushed." That is invisible to interpretation; it lives only in the rrweb
   **event timeline**. `rrweb-pacing-scan.mjs` (pure structural parse, no LLM, no
   ffmpeg) reads the clip directly: a *content reveal* is an incremental DOM
   mutation whose `adds` introduce a substantial block (clears `--sig-min-adds` /
   `--sig-min-text`); reveals within `--coalesce` ms are one logical render; each
   reveal must hold for `--min-dwell` ms (default 1200) before the next, and a
   burst inside the final `--tail-window` ms is reported as the rushed *tail*. Pass
   `--rrweb <clip.rrweb.json | dir>` to `qa.sh`; flags are **advisory** (a
   "rrweb-pacing warnings" table) unless `--rrweb-strict` promotes them to a
   blocking gate — same advisory/strict shape as `blank-scan`/`pacing-scan`. The
   fix when it fires: give the capture an end-of-conversation dwell, or re-pace the
   clip deterministically with `slidey rrweb-repace <in> <out>` (which mirrors this
   scan's significance definition, so a re-paced clip clears the gate).
8. **Stuck-placeholder check.** A panel can sit on a *transient* placeholder
   forever — a "Loading…" spinner whose loading flag is never lowered — and every
   single frame of it still looks "fine," so blank-scan (mostly themed bg) and a
   frame-at-a-time reviewer both miss it. The review prompt now reads the frames as
   a **timeline** and fails a panel whose placeholder persists across many frames
   (rule 7), and a **deterministic** `placeholder-scan.sh` (OCR) flags a placeholder
   that runs unbroken for `--min-run` frames or covers `--min-fraction` of the demo.
   This is what catches "the three panels just say Loading… for a long time."
7. **Conversation-legibility + occlusion check.** A demo of human usage must let a
   viewer FOLLOW the conversation. The review prompt (rules 7–8) reads the frames as
   a timeline and emits a blocking `visual_issues[]` entry when: an operator INPUT
   is never visible as legible text (it flashed by / was sent off-camera); an agent
   RESPONSE is clipped to nothing or never shown (long replies MAY truncate); a
   long RESPONSE is only ever shown **scrolled to its bottom** so its opening lines
   never appear (the transcript snapped to the end instead of scrolling THROUGH the
   message — the #1 "jumpy" cause); a **raw internal intent name** with double
   underscores (`core__prd__start`) is visible as a label (stale embed / un-humanised
   surface); the chat transcript is **covered or pushed off-screen** by another
   panel (a file/PRD/diff editor opening OVER the conversation); or a floating
   overlay (tour coachmark / popover / tooltip) overlaps and obscures the chat. This
   is what catches "the video is jumpy, you can't see the user inputs, and the
   editor hides the chat." Because `visual_issues` always block the gate, such a
   demo can never pass — author it so each message SCROLLS THROUGH (use the
   `revealTurn` helper, never fixed dwells over the native auto-scroll), the chat
   stays visible beside the editor, labels are humanised, and every input is
   readable (see `kitsoki-ui-demo`).
8. **Right-surface + progress-legibility check.** A demo of a feature *used by a
   human* must prove it on the product's **conversation** surface — and **every
   conversation must provide meaningful feedback as it progresses, even when no
   operator input is required**. An autonomous / self-driving run (one that
   advances with no human turn — e.g. a loop that cascades to terminal on entry)
   must still narrate each step as readable conversation messages, not advance
   silently. The footgun this catches: a demo that shows the run ONLY through the
   developer-facing **trace/observer** (the state diagram + an event timeline of
   `host.run` / `world.update` / `machine.say` rows) while the conversation pane
   stays empty. The auditor's trace is not the product experience; proving "the
   feature works" on the trace alone is the WRONG SURFACE. The review prompt
   (EVIDENCE RULE 9) reads the frames as a timeline and, when the feature/scenarios
   describe usage/a-loop/a-conversation but only the trace ever appears, emits a
   blocking `visual_issues` entry and fails every usage scenario. The one
   exception is a feature that genuinely *is* the trace/observer/diagram (the run
   viewer itself) — there the trace is the correct surface, decided from the
   feature file, not a default. (Runtime side: a self-driving kitsoki run surfaces
   its `say:` breadcrumbs as conversation bubbles so this feedback exists to film —
   see the demo-video-loop story.)

## Prerequisites

`ffmpeg`, `jq`, and the `claude` CLI on PATH (all already present in this repo's
dev env). No `make build` needed — this consumes an existing video/frames.

## The loop

1. **Give it the bug/plan + what the evidence should show.** Copy the templates
   and edit — `--feature` is the *actual bug report or implementation plan*, not
   a generic blurb (see "judged against the bug/plan" above):
   ```bash
   D=.agents/skills/kitsoki-ui-qa
   cp $D/templates/feature.example.md   .context/qa-feature.md   # ← the bug or plan
   cp $D/templates/scenarios.example.yaml .context/qa-scenarios.yaml
   ```
   Scenarios are **observable claims** ("the state badge advances", "three story
   cards are listed") — not internal behaviour the camera can't see. Mark a
   scenario `required: false` to keep it non-blocking.

   Then pick the evidence form (see "Pick the evidence" above): a **screenshot**
   for a simple, single-state case; a **video** for a complex/multi-state flow.

2. **Run the QA gate** (one shot: extract → contact sheet → review → report).
   For a **screenshot** (or any pre-captured PNG set), pass the frames dir
   directly — the positional path is only used to name the output dir:
   ```bash
   .agents/skills/kitsoki-ui-qa/scripts/qa.sh .artifacts/fix-badge/badge.png \
     --frames   .artifacts/fix-badge \
     --feature   .context/qa-feature.md \
     --scenarios .context/qa-scenarios.yaml --strict
   ```
   For a **video**:
   ```bash
   .agents/skills/kitsoki-ui-qa/scripts/qa.sh \
     .artifacts/multi-story/multi-story.mp4 \
     --feature   .context/qa-feature.md \
     --scenarios .context/qa-scenarios.yaml
   echo "gate exit: $?"          # 0 pass · 1 blocking failure · 2 pipeline error
   ```
   Artifacts land in `.artifacts/ui-qa/<video-stem>/`
   ([[feedback_artifacts_dir]]): `frames/`, `frames.json`, `contact-sheet.png`,
   `verdict.json`, `qa-report.md`.

3. **Prefer ground-truth frames when you have them.** The recorder already emits
   labeled per-scene `NN-<scene>.png`. Point `--frames` at that dir to QA those
   exact captures instead of re-extracting (highest fidelity, skips ffmpeg):
   ```bash
   .agents/skills/kitsoki-ui-qa/scripts/qa.sh .artifacts/multi-story/multi-story.mp4 \
     --frames .artifacts/multi-story --feature .context/qa-feature.md \
     --scenarios .context/qa-scenarios.yaml --strict
   ```

4. **Read `qa-report.md`.** Per-scenario table with the cited evidence frame for
   each step. Open the cited PNGs (or `contact-sheet.png`) to confirm. If a
   scenario is `unsupported`, the demo didn't cover it — usually a gap in the
   *demo*, occasionally a vague scenario step to sharpen.

## Full-editor (VS Code) evidence

QA'ing a **full-editor** video (the kitsoki UI embedded in a VS Code window —
see [[kitsoki-ui-demo]] → "Full-editor (VS Code) mode") differs from a browser
video: a larger 1400×900 frame, a big **dark editor pane** / empty welcome
region, and VS Code chrome (Activity Bar, panels) with grey edge strips. Two
deterministic stages need the right invocation so the gate stays trustworthy:

- **Prefer the labeled ground-truth frames** the recorder emits
  (`.artifacts/vscode-tour/NN-<beat>.png`, one per beat). Pass them via
  `--frames` — highest fidelity, no scene-extraction artifacts, and they pass
  `blank-scan.sh` at the **default** `--min-coverage 0.10` (proven: 0 flags on
  the 7 vscode-tour beats). **This is the recommended path.**
  ```bash
  docs/skills/kitsoki-ui-qa/scripts/qa.sh \
    .artifacts/vscode-tour/vscode-tour.mp4 \
    --frames    .artifacts/vscode-tour \
    --feature   .context/qa-vscode-feature.md \
    --scenarios .context/qa-vscode-scenarios.yaml --strict
  # → overall: pass, 6/6 scenarios, 0 visual issues, 0 blank-scan flags
  ```

- **If you must scene-extract** (no labeled frames), `extract-frames.sh` at the
  default `--scene 0.30` surfaces the editor's meaningful transitions fine
  (≈18 frames on the 72s vscode-tour). But the full-window frames carry a
  legitimate **grey editor-chrome edge strip** (~12.5% of the frame) that trips
  `blank-scan.sh` at the default `--min-coverage 0.10` as a false "solid block".
  Raise the threshold for editor videos via the `qa.sh` passthrough:
  ```bash
  docs/skills/kitsoki-ui-qa/scripts/qa.sh \
    .artifacts/vscode-tour/vscode-tour.mp4 \
    --feature .context/qa-vscode-feature.md \
    --scenarios .context/qa-vscode-scenarios.yaml \
    --blank-min-coverage 0.15        # editor-chrome strips no longer false-flag
  # → blank-scan: 0 flags on the 18 extracted frames
  ```
  `--scene TH` is also a passthrough if you want denser/sparser extraction.

The legitimately **dark** editor pane / empty welcome region does **not**
false-flag at any setting: `blank-scan.sh`'s contrast gate treats the most-common
dark bucket as the page background, so a large dark region is correctly ignored
(only a *high-contrast* solid block flags). The grey edge strip was the only
editor-specific false positive, and `--blank-min-coverage 0.15` clears it while
still catching a genuine blank pane.

A ready-to-edit feature/scenarios pair for a VS Code embed lives at
`templates/vscode-feature.md` + `templates/vscode-scenarios.yaml` (the worked
example behind the `vscode-tour` gate above).

## The tools (`scripts/`)

| Script | Does | LLM? |
|---|---|---|
| `qa.sh <video> --feature F --scenarios S [--frames D] [--out D] [--model M] [--max-frames N] [--scene TH] [--blank-min-coverage F] [--chapters F] [--pacing-min N] [--rrweb CLIP\|DIR] [--rrweb-min-dwell N] [--no-adversary] [--strict] [--blank-strict] [--pacing-strict] [--rrweb-strict]` | One-shot wrapper; exit code is the gate. `--scene` / `--blank-min-coverage` pass through to extract-frames / blank-scan (tune for full-editor videos — see above); `--rrweb` runs the embedded-tour pacing scan on the clip(s) | via review |
| `extract-frames.sh <video> <out-dir> [--scene TH] [--interval S] [--dedup MS] [--max N] [--width W]` | Deterministic scene-change + periodic-floor frames + `frames.json` | no |
| `blank-scan.sh <frames-dir\|image> [--out scan.json] [--grid WxH] [--quant N] [--min-coverage F] [--empty-coverage F] [--fail-on-find]` | Deterministic monochrome-region detector → `blank-scan.json` (flags any large flat block of one colour, or a near-empty frame) | no |
| `pacing-scan.sh <chapters.json> [--out scan.json] [--min-ms N] [--min-total-ms N] [--fail-on-find]` | Deterministic chapter-duration detector → `pacing-scan.json` (flags narrated moments that flash by below the readable-window floor) | no |
| `rrweb-pacing-scan.mjs <clip.rrweb.json\|dir> [--out scan.json] [--min-dwell N] [--coalesce N] [--sig-min-adds N] [--sig-min-text N] [--tail-window N] [--fail-on-find]` | Deterministic embedded-rrweb timeline scan → `rrweb-pacing-scan.json` (flags content reveals crammed below the readable dwell — the rushed-last-messages defect a frame sampler / chapter scan can't see) | no |
| `placeholder-scan.sh <frames-dir\|image\|video> [--out scan.json] [--pattern RE] [--min-fraction F] [--min-run N] [--fail-on-find]` | Deterministic OCR stuck-placeholder detector → flags a placeholder (default `\bloading\b`) that persists across a long unbroken run / large fraction of frames — a "Loading…" that never resolves. Skips (advisory) if `tesseract` is absent | no (OCR) |
| `qa-review.sh --frames D --feature F --scenarios S --out V [--model M] [--no-adversary]` | Read-only vision agent → evidence-cited `verdict.json` + adversarial re-check | **yes** |
| `report.sh <verdict.json> [--out report.md] [--strict] [--blank-scan scan.json] [--blank-strict] [--pacing-scan scan.json] [--pacing-strict] [--rrweb-scan scan.json] [--rrweb-strict]` | `verdict.json` (+ optional scans) → `qa-report.md`; recomputes the gate exit code | no |

Defaults: review model `claude-opus-4-8` (override `--model claude-sonnet-4-6`
for faster/cheaper); `--max-frames 48`; `--strict` makes every scenario blocking.
`qa.sh` always runs `blank-scan.sh` over the frames, and `pacing-scan.sh` over the
chapter sidecar when one is present beside the MP4 (auto-detected, or `--chapters`);
both scans' flags are **advisory** (surfaced in the report, never block) unless you
pass `--blank-strict` / `--pacing-strict`. The LLM `visual_issues` and
`annotation_issues` checks (context-aware) always block regardless.

## verdict.json shape

```json
{ "overall":"pass|fail",
  "summary":{"scenarios_total":0,"passed":0,"failed":0,"unsupported":0},
  "frames_reviewed":["0001-0ms.png"],
  "scenarios":[
    {"id":"drive","title":"…","required":true,"status":"pass|fail|unsupported",
     "steps":[{"text":"…","status":"pass|fail|unsupported",
               "evidence":[{"frame":"0007-5200ms.png","observation":"<literal>"}],
               "confidence":0.0}]}]}
```

## Pointers

- The recorder this inverts: [[kitsoki-ui-demo]] (`.agents/skills/kitsoki-ui-demo/`)
  — its `NN-<scene>.png` output is the ideal `--frames` input here, and its
  `contact-sheet.sh` is reused for the storyboard.
- Oracle = local `claude` CLI: `internal/host/oracle_runner.go`.

## Maintenance

Codex discovers this skill directly. Refresh the project-local Claude Code
symlink after adding or moving skills:

```
make setup
```
