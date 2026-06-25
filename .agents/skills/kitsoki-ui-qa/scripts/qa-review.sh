#!/usr/bin/env bash
# Ground a vision QA review of a UI demo against a feature description + usage
# scenarios. Spawns the local `claude` CLI (no API key, no per-call cost —
# see memory project_oracle_uses_claude_cli) as a READ-ONLY agent that reads the
# extracted frame PNGs with its Read tool and emits a structured verdict.json.
#
# Reliability is NOT from the model being deterministic — it comes from:
#   • a fixed, deterministic frame set (extract-frames.sh) as the ONLY evidence;
#   • every verdict MUST cite a frame filename + quote what is literally visible;
#     a claim with no citable frame is `unsupported`, never `pass`;
#   • an adversarial second pass (a skeptic that may only DOWNGRADE) re-checks
#     each `pass` against its cited frame (disable with --no-adversary).
#
# The adversarial pass follows the kitsoki split (interpretive vs. deterministic):
# the model emits ONLY a small set of downgrades (which step, to what, and why);
# this script APPLIES them deterministically — it can only lower a status, never
# raise one — then recomputes every scenario/overall/summary itself. That keeps
# the downgrade-only invariant honest in code and keeps the model output tiny, so
# the pass is robust (no re-emitting the whole multi-KB verdict, the failure mode
# that used to make it return unparseable text).
#
# This is an LLM-driven review tool by design. It is NOT a no-LLM flow test and
# must never be wired into the automated test suite (CLAUDE.md). The surrounding
# deterministic pieces (extract-frames.sh, report.sh) are testable without an LLM.
#
# Usage: qa-review.sh --frames <dir> --feature <file> --scenarios <file>
#                     --out <verdict.json> [--model M] [--no-adversary]
set -euo pipefail

frames="" feature="" scenarios="" out="" model="claude-opus-4-8" adversary=1
while [ $# -gt 0 ]; do
  case "$1" in
    --frames)       frames="$2"; shift 2 ;;
    --feature)      feature="$2"; shift 2 ;;
    --scenarios)    scenarios="$2"; shift 2 ;;
    --out)          out="$2"; shift 2 ;;
    --model)        model="$2"; shift 2 ;;
    --no-adversary) adversary=0; shift ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

command -v claude >/dev/null 2>&1 || { echo "claude CLI not on PATH" >&2; exit 1; }
command -v jq     >/dev/null 2>&1 || { echo "jq not on PATH" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 not on PATH" >&2; exit 1; }
[ -d "$frames" ]      || { echo "no such frames dir: $frames" >&2; exit 1; }
[ -f "$feature" ]     || { echo "no such feature file: $feature" >&2; exit 1; }
[ -f "$scenarios" ]   || { echo "no such scenarios file: $scenarios" >&2; exit 1; }
[ -n "$out" ]         || { echo "--out is required" >&2; exit 1; }

frames="$(cd "$frames" && pwd)"           # absolute, for --add-dir
mkdir -p "$(dirname "$out")"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

frame_list="$(cd "$frames" && ls -1 [0-9]*.png 2>/dev/null | sort || ls -1 *.png | sort)"
[ -n "$frame_list" ] || { echo "no PNG frames in $frames" >&2; exit 1; }

# extract_json: read arbitrary model text on stdin, print the first BALANCED
# top-level JSON object found in it. Tolerates surrounding prose, ``` fences
# anywhere, and a trailing explanation — the failure mode `sed '/^```/d'` could
# not handle. Exits non-zero if no valid JSON object is present.
#
# The extractor is written to a temp FILE and invoked as `python3 file` (reading
# the pipe via sys.stdin). It must NOT be `python3 - <<'EOF'`: there the heredoc
# IS python's stdin (the program source), so the piped model text would never
# reach sys.stdin.read() — the bug that made every extraction "find no JSON".
cat > "$tmp/extract_json.py" <<'PY'
import sys, json
s = sys.stdin.read()
def first_balanced(text, opener, closer):
    start = text.find(opener)
    while start != -1:
        depth = 0; instr = False; esc = False
        for i in range(start, len(text)):
            c = text[i]
            if instr:
                if esc: esc = False
                elif c == '\\': esc = True
                elif c == '"': instr = False
            else:
                if c == '"': instr = True
                elif c == opener: depth += 1
                elif c == closer:
                    depth -= 1
                    if depth == 0:
                        cand = text[start:i+1]
                        try:
                            json.loads(cand); return cand
                        except Exception:
                            break
        start = text.find(opener, start + 1)
    return None
obj = first_balanced(s, '{', '}')
if obj is None:
    sys.exit(1)
sys.stdout.write(obj)
PY
extract_json() { python3 "$tmp/extract_json.py"; }

# call_claude_json: run one read-only claude call from a prompt file and print
# the extracted verdict JSON. Retries once on a transient non-JSON / invocation
# blip before giving up (exit 2). label is for diagnostics.
call_claude_json() { # <promptfile> <label>
  local pf="$1" label="$2" attempt raw result json
  for attempt in 1 2; do
    raw="$(claude -p \
            --output-format json \
            --model "$model" \
            --permission-mode bypassPermissions \
            --allowedTools "Read" \
            --add-dir "$frames" \
            < "$pf" 2>/dev/null)" || { echo "  ($label) claude invocation failed (attempt $attempt)" >&2; continue; }
    result="$(printf '%s' "$raw" | jq -r '.result // .text // empty')"
    [ -n "$result" ] || result="$raw"          # tolerate a bare-JSON CLI build
    if json="$(printf '%s' "$result" | extract_json)"; then
      printf '%s' "$json"
      return 0
    fi
    echo "  ($label) no parseable JSON in model output (attempt $attempt); retrying…" >&2
    printf '%s' "$result" > "${out%.json}.${label}.raw.txt" 2>/dev/null || true
  done
  return 2
}

# ---------- pass 1: grounded review ----------
review_prompt="$tmp/review.txt"
{
  cat <<'HEAD'
You are a meticulous UI QA reviewer. You are given screenshots ("frames") — a
single captured screenshot for a simple case, or frames sampled from a demo
video for a complex flow — plus the BUG OR PLAN being verified (the "feature"
file) and a list of usage scenarios. Decide, for each scenario step, whether the
evidence actually demonstrates it, AND whether the evidence as a whole is
relevant and complete for the stated bug/plan (evidence that never exercises the
changed behaviour cannot prove it — its steps are `unsupported`, not `pass`).

EVIDENCE RULES (these make the review trustworthy — follow them exactly):
1. The frame PNG files are the ONLY admissible evidence. Use the Read tool to
   open the specific frames you need. Read enough frames to judge every step.
2. For every step you mark `pass`, you MUST cite at least one frame filename and
   quote what is LITERALLY visible in it that demonstrates the step (visible text,
   a button, a state badge, a list, etc.). Do not infer beyond the pixels.
3. If no frame shows the step, its status is `unsupported` (the demo neither
   proves nor disproves it) — NOT `pass`. If a frame actively contradicts it
   (wrong text, error state, missing element), its status is `fail`.
4. Never invent UI that you did not see in a frame. When unsure, prefer
   `unsupported` over `pass`.
5. VISUAL INTEGRITY — a broken or blank render is a FAILURE, not a pass. If a
   frame shows a large blank/uniform/placeholder region (an all-white or
   all-black box, an empty pane, a broken-image glyph) WHERE the feature is
   meant to show visual content — a screenshot, image, replay, preview, thumbnail,
   chart, map, avatar, or video — then any step that claims that visual is `fail`:
   cite the frame and describe the empty area (e.g. "the Session replay pane is a
   solid white rectangle — no UI rendered"). A visual step passes ONLY if the
   expected content is ACTUALLY rendered in the pixels. (An explicit empty-state
   message like "No data" is acceptable only if the scenario expects an empty
   state; a silent blank where content belongs is a render bug — flag it.)
   Proactively scan EVERY frame for such dead regions even when no scenario step
   names them, and report each in a top-level "visual_issues" array.
6. ANNOTATION CONSISTENCY — a demo must use ONE narration mechanism throughout.
   These videos narrate with EITHER tour popovers (a titled card with a step
   counter like "Step 3 of 9" and Back / Next / Skip buttons, usually anchored to
   a spotlight ring) OR banner/caption overlays (a flat title+subtitle strip,
   typically along an edge, with no Next affordance). EITHER style is fine on its
   own. MIXING them — tour-popover cards in some frames AND banner/caption
   overlays in others within the SAME video — is a defect: the video drifts
   between two annotation styles. Scan EVERY frame and classify the narration
   style you see (if any). If across the whole frame set you see BOTH a
   tour-popover style AND a banner/caption style, report each offending frame in a
   top-level "annotation_issues" array: for each frame name the styles_seen and
   describe the inconsistency. A video that uses a single consistent style (or a
   frame with no narration at all) contributes NOTHING to this array — leave it
   empty in that case. Do NOT flag a video merely for being caption-narrated or
   merely for being tour-narrated; only flag the MIX of the two.
7. STUCK PLACEHOLDERS — a transient placeholder that NEVER resolves is a bug, even
   though any single frame of it looks "fine". Read the frames as a TIMELINE: if a
   panel shows a loading placeholder — "Loading…", a spinner, a skeleton, an empty
   "…" pane — across MULTIPLE frames spanning a meaningful stretch of the demo (it
   should have rendered content or an empty/start state by then), that panel is
   stuck (a classic cause: a `loading` flag the code never lowers). Treat it as a
   `fail` for any step claiming that panel rendered, AND add a `visual_issues` entry
   citing the first and last frame the placeholder persists in (e.g. "the Trace
   panel shows 'Loading…' from 0002 through 0009 — never resolves"). A placeholder
   in just one transitional frame is fine; persistence across many is the bug.
7. CONVERSATION LEGIBILITY — when the demo shows a chat/conversation (the operator
   talking to the agent), a viewer must be able to FOLLOW the human usage. Reading
   the frames as a TIMELINE, verify:
   (a) every operator INPUT the demo sends appears somewhere as legible text (a
       chat message/bubble you can actually read) — an input that is never visible
       in any frame (it flashed by too fast, or was sent off-camera) is a failure.
       This includes WHILE TYPING: the composer must show the operator's FULL
       message as it is entered. The classic bug is a single-line input that
       horizontally scrolls, so a long message's START scrolls off the left edge
       and is never readable in full (e.g. the field shows "…require tenant
       isolation" with the leading "add a non-goals section and " gone). If any
       frame shows the text being typed but its beginning has scrolled out of the
       input (you cannot read the whole message the operator is composing), that
       is a failure — the composer should wrap/grow to keep the full text visible;
   (b) enough of each agent RESPONSE is visible to understand what happened — a
       long response MAY be truncated, but a reply clipped to nothing (or never
       shown at all) is a failure;
   (c) each agent RESPONSE is REVEALED FROM ITS START, not just its tail. A well
       made conversation video SCROLLS THROUGH each message: across the timeline
       you should see a long reply's OPENING lines in some frame(s) and later
       lines in others, as the camera eases down through it. The classic bug is
       the transcript snapping to the BOTTOM the instant a reply arrives, so only
       its last lines are ever on-screen and its opening is never shown — if a
       reply taller than the viewport only ever appears scrolled to its end (its
       first lines never visible in any frame), that is a failure: it is not
       followable and the viewer cannot pause to read it from the start;
   (d) the chat transcript is NOT covered, pushed off-screen, or collapsed to
       unreadability by another panel — a classic bug is a file/PRD/diff editor
       opening OVER the conversation so the messages are no longer visible;
   (e) NO raw internal intent name is visible as a label. The UI humanises intent
       labels (buttons, the state-diagram "via …" breadcrumb), so a visible
       machine slug with double underscores (e.g. "core__prd__start",
       "bf__accept") is a failure — it means a stale embed or an un-humanised
       surface leaked the identifier to the operator.
   For any of these, add a "visual_issues" entry naming the frame(s) and the
   missing/obscured content (e.g. "the chat is fully covered by the 004-prd.md
   editor in 0005-0008 — the user inputs and replies are not visible"; or "the
   reply in 0006-0009 is only ever shown scrolled to its bottom — its opening
   lines never appear"; or "a button reads 'core__prd__start' in 0004"), and
   `fail` any step that depends on seeing the conversation.
8. OCCLUSION / OVERLAPPING OVERLAYS — a floating overlay (a tour coachmark,
   popover, tooltip, toast, or spotlight) that OVERLAPS and obscures the primary
   content — especially the chat — is a failure, not decoration. If a tour label
   sits on top of the conversation so messages/inputs are hidden behind it, add a
   "visual_issues" entry naming the frame and the obscured content.
9. PROGRESS LEGIBILITY ON THE RIGHT SURFACE — a demo of a feature USED by a human
   (an agent doing work, a loop running, a flow progressing) must let the viewer
   FOLLOW that work on the product's CONVERSATION surface — the chat transcript of
   messages/bubbles where the operator and the agent/machine speak. EVERY
   conversation must provide meaningful feedback AS IT PROGRESSES, EVEN WHEN NO
   OPERATOR INPUT IS REQUIRED: an autonomous / self-driving run (one that advances
   with no human turn) must still narrate its progress as readable conversation
   messages — each step (what it is doing, a result, a verdict, a transition)
   appearing as a legible bubble — not merely advance silently. A demo that shows
   the run ONLY through the developer-facing TRACE/OBSERVER — a state diagram plus
   an event/timeline list of rows like `host.run`, `world.update`, `machine.say`,
   turn counters — while the conversation surface stays empty (or is never shown),
   is the WRONG SURFACE for proving human usage: the auditor's trace is not the
   product experience. Reading the frames as a TIMELINE, if the scenarios/feature
   describe usage/a-conversation/a-loop progressing but across the whole demo you
   only ever see the trace/observer (state-diagram + event rows) and never a
   conversation of readable progress messages, add a blocking "visual_issues"
   entry (e.g. "the run is shown only as a trace/state-diagram + event timeline in
   0003–0009; the conversation surface is empty — no progress messages, so the
   feature is not shown being used") and mark every usage/progress/conversation
   scenario `fail`. EXCEPTION — do NOT flag this when the FEATURE ITSELF IS the
   trace/observer/state-diagram (e.g. the run viewer, the timeline, the diagram is
   literally what the bug/plan is about); there the trace IS the correct, expected
   surface. Decide which case you are in from the feature file, not from a default.

Compute each scenario's status as the worst of its steps (fail < unsupported <
pass). Copy each scenario's `id`, `title`, and `required` exactly from the YAML
(default `required` to true if absent). `overall` is `pass` only if every
required scenario is `pass`, else `fail`.

OUTPUT: print ONLY a single raw JSON object (no prose, no ``` fences) of shape:
{
  "overall": "pass|fail",
  "summary": {"scenarios_total":0,"passed":0,"failed":0,"unsupported":0},
  "frames_reviewed": ["0001-0ms.png"],
  "visual_issues": [
    {"frame":"0003-1200ms.png","region":"<where on screen>","issue":"<blank/broken render observed where content was expected>"}
  ],
  "annotation_issues": [
    {"frame":"0007-5200ms.png","styles_seen":["tour-popover","banner-caption"],"issue":"<the mixed narration styles observed across the video>"}
  ],
  "scenarios": [
    {"id":"...","title":"...","required":true,"status":"pass|fail|unsupported",
     "steps":[
       {"text":"...","status":"pass|fail|unsupported",
        "evidence":[{"frame":"0003-1200ms.png","observation":"<literal, what is visible>"}],
        "confidence":0.0}
     ]}
  ]
}
HEAD
  echo; echo "## FEATURE DESCRIPTION"; echo; cat "$feature"
  echo; echo "## USAGE SCENARIOS (YAML)"; echo; echo '```yaml'; cat "$scenarios"; echo '```'
  echo; echo "## AVAILABLE FRAMES"
  echo "Located in: $frames (Read them by filename)."
  echo "$frame_list" | sed 's/^/  - /'
} > "$review_prompt"

echo "▸ grounded review ($model, $(echo "$frame_list" | wc -l | tr -d ' ') frames)…" >&2
# Write atomically (temp on the SAME filesystem as $out, then mv) so a concurrent
# reader — e.g. report.sh, or a watcher — never observes a truncated or partial
# verdict, only the previous file or the complete new one.
out_tmp="$(mktemp "$(dirname "$out")/.verdict.XXXXXX")"
if ! call_claude_json "$review_prompt" review > "$out_tmp"; then
  rm -f "$out_tmp"
  echo "grounded review did not produce parseable JSON after retries" >&2
  exit 2
fi
mv -f "$out_tmp" "$out"

# ---------- pass 2: adversarial verification (downgrade-only, delta output) ----------
if [ "$adversary" -eq 1 ]; then
  adv_prompt="$tmp/adversary.txt"
  {
    cat <<'HEAD'
You are an adversarial verifier. Below is a prior QA verdict (JSON) for a demo
video. Your ONLY job is to catch OVER-CLAIMS in the steps currently marked
`pass`.

For each `pass` step: Read its cited frame(s) with the Read tool and confirm the
quoted observation is ACTUALLY, LITERALLY visible there. If the cited frame does
not clearly show it — wrong frame, the element is absent, the text differs, or it
was inferred beyond the pixels — it must be downgraded:
  • `fail`        — the frame actively contradicts the claim.
  • `unsupported` — the frame simply doesn't show it.

Pay special attention to steps that claim a VISUAL is shown (a screenshot,
image, replay, preview, thumbnail, chart, map, avatar, video). If the cited
frame's supposed-content region is actually a large blank/uniform box (all-white
or all-black), a placeholder, or a broken-image glyph, the visual is NOT rendered
— downgrade that step to `fail` and describe the empty region. A passed visual
step backed by a blank frame is the exact over-claim you exist to catch.

Do NOT re-emit the whole verdict. Output ONLY the downgrades you are confident
about. You may ONLY downgrade a `pass`; never touch `fail`/`unsupported` steps
and never upgrade anything (the harness enforces this — an upgrade is ignored).
If every `pass` step holds up, output an empty list.

Reference each step by the scenario `id` and its zero-based `step_index` within
that scenario's `steps` array.

OUTPUT: print ONLY a single raw JSON object (no prose, no ``` fences):
{
  "downgrades": [
    {"scenario_id":"<id>","step_index":0,"new_status":"fail|unsupported",
     "observation":"<what the cited frame REALLY shows>"}
  ]
}
HEAD
    echo; echo "## PRIOR VERDICT"; echo; echo '```json'; cat "$out"; echo '```'
    echo; echo "## AVAILABLE FRAMES"
    echo "Located in: $frames (Read them by filename)."
    echo "$frame_list" | sed 's/^/  - /'
  } > "$adv_prompt"

  echo "▸ adversarial verification (downgrade-only)…" >&2
  if call_claude_json "$adv_prompt" adversary > "$tmp/downgrades.json"; then
    # Apply the downgrades deterministically: lower-only, then recompute every
    # scenario status (worst of steps), overall (all required pass), and counts.
    python3 - "$out" "$tmp/downgrades.json" <<'PY'
import sys, json
verdict_path, deltas_path = sys.argv[1], sys.argv[2]
with open(verdict_path) as f: v = json.load(f)
with open(deltas_path) as f: deltas = (json.load(f) or {}).get("downgrades", []) or []
RANK = {"fail": 0, "unsupported": 1, "pass": 2}
NAME = {0: "fail", 1: "unsupported", 2: "pass"}
by_id = {s.get("id"): s for s in v.get("scenarios", [])}
applied = []
for d in deltas:
    sc = by_id.get(d.get("scenario_id"))
    if not sc: continue
    steps = sc.get("steps", []) or []
    idx = d.get("step_index")
    if not isinstance(idx, int) or idx < 0 or idx >= len(steps): continue
    st = steps[idx]
    cur = RANK.get(st.get("status"), 2)
    new = min(cur, RANK.get(d.get("new_status"), cur))   # ONLY ever downgrade
    if new != cur:
        st["status"] = NAME[new]
        if d.get("observation"):
            st["observation_adversary"] = d["observation"]
        applied.append({"scenario_id": d.get("scenario_id"), "step_index": idx,
                        "from": NAME[cur], "to": NAME[new], "observation": d.get("observation", "")})
counts = {"pass": 0, "fail": 0, "unsupported": 0}
all_required_pass = True
for sc in v.get("scenarios", []):
    steps = sc.get("steps", []) or []
    worst = min((RANK.get(s.get("status"), 2) for s in steps), default=RANK.get(sc.get("status"), 2))
    sc["status"] = NAME[worst]
    counts[sc["status"]] += 1
    if sc.get("required", True) and sc["status"] != "pass":
        all_required_pass = False
v["overall"] = "pass" if all_required_pass else "fail"
v["summary"] = {"scenarios_total": len(v.get("scenarios", [])),
                "passed": counts["pass"], "failed": counts["fail"], "unsupported": counts["unsupported"]}
v["adversary"] = {"status": "ok", "downgrades_applied": applied}
# Atomic rewrite: write a sibling temp then os.replace, so a concurrent reader
# never sees a verdict whose summary and per-scenario statuses disagree (the
# truncate-then-write window). os.replace is atomic on the same filesystem.
import os, tempfile
d = os.path.dirname(verdict_path) or "."
fd, tmp_path = tempfile.mkstemp(dir=d, prefix=".verdict.")
with os.fdopen(fd, "w") as f: json.dump(v, f, indent=2)
os.replace(tmp_path, verdict_path)
print(f"  adversary applied {len(applied)} downgrade(s)", file=sys.stderr)
PY
  else
    # The adversary couldn't produce parseable deltas even after a retry. Do NOT
    # silently pass: record the failure on the verdict and recompute. The gate
    # (report.sh, --strict) can then treat an un-run adversary as it sees fit;
    # the grounded review (with cited evidence) still stands as the verdict.
    echo "  adversary pass did not return parseable JSON after retries — verdict left as the grounded review, flagged adversary.status=error" >&2
    python3 - "$out" <<'PY'
import sys, json
p = sys.argv[1]
with open(p) as f: v = json.load(f)
v["adversary"] = {"status": "error", "downgrades_applied": []}
with open(p, "w") as f: json.dump(v, f, indent=2)
PY
  fi
fi

echo "wrote $out" >&2
