#!/usr/bin/env bash
# kitsoki-ui-review · STAGE 2 (LLM): multi-agent heuristic vision review.
#
# Reads the deterministic capture (audit.json + frames/) and fans the frames out
# across SEVERAL independent, read-only `claude` vision agents — one per shard
# (default: one tour step = that surface across all its viewports, ~3 frames).
# No single agent ever holds the whole frame set in its context; each judges only
# its own handful of frames against the heuristic catalog, with that surface's
# deterministic audit findings handed in as already-known truth. A per-shard
# adversarial skeptic (downgrade-only) then re-checks each agent's own frames.
#
# Uses the local `claude` CLI (no API key, no per-call cost — see memory
# project_oracle_uses_claude_cli). This is an LLM review by design; it is NOT a
# no-LLM flow test and must never be wired into the automated suite (CLAUDE.md).
#
# Output: vision.json = { shards:[...], findings:[ all per-shard findings ] }.
# report.sh merges this with audit.json into the gated verdict.
#
# Usage:
#   review.sh --audit <audit.json> --frames <dir> --heuristics <file>
#             --out <vision.json> [--design-intent <file>] [--model M]
#             [--jobs N] [--shard step|viewport] [--no-adversary]
set -euo pipefail

audit="" frames="" heuristics="" out="" intent="" model="claude-opus-4-8"
jobs=4 shard_by="step" adversary=1
while [ $# -gt 0 ]; do
  case "$1" in
    --audit)         audit="$2"; shift 2 ;;
    --frames)        frames="$2"; shift 2 ;;
    --heuristics)    heuristics="$2"; shift 2 ;;
    --out)           out="$2"; shift 2 ;;
    --design-intent) intent="$2"; shift 2 ;;
    --model)         model="$2"; shift 2 ;;
    --jobs)          jobs="$2"; shift 2 ;;
    --shard)         shard_by="$2"; shift 2 ;;
    --no-adversary)  adversary=0; shift ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

command -v claude >/dev/null 2>&1 || { echo "claude CLI not on PATH" >&2; exit 1; }
command -v jq     >/dev/null 2>&1 || { echo "jq not on PATH" >&2; exit 1; }
[ -f "$audit" ]      || { echo "no such audit.json: $audit" >&2; exit 1; }
[ -d "$frames" ]     || { echo "no such frames dir: $frames" >&2; exit 1; }
[ -f "$heuristics" ] || { echo "no such heuristics file: $heuristics" >&2; exit 1; }
[ -n "$out" ]        || { echo "--out is required" >&2; exit 1; }

frames="$(cd "$frames" && pwd)"
mkdir -p "$(dirname "$out")"
shard_dir="$(mktemp -d)"; trap 'rm -rf "$shard_dir"' EXIT

# Shard keys: a step id (default) or a viewport name. Each shard reviews a
# coherent, small set of frames so a single agent's context stays light.
if [ "$shard_by" = "viewport" ]; then
  mapfile -t shards < <(jq -r '[.captures[]|select(.captured)|.viewport]|unique|.[]' "$audit")
  sel_field=".viewport"
else
  mapfile -t shards < <(jq -r '[.captures[]|select(.captured)|.step]|unique|.[]' "$audit")
  sel_field=".step"
fi
[ "${#shards[@]}" -gt 0 ] || { echo "no captured frames in $audit" >&2; exit 1; }

intent_block=""
[ -n "$intent" ] && [ -f "$intent" ] && intent_block="$intent"

# ---- one read-only claude call over a shard's frames; echoes validated JSON ---
call_claude() { # <promptfile> <result-out>
  local pf="$1" rf="$2" raw result json
  raw="$(claude -p \
          --output-format json \
          --model "$model" \
          --permission-mode bypassPermissions \
          --allowedTools "Read" \
          --add-dir "$frames" \
          < "$pf" 2>/dev/null)" || { echo '{"findings":[]}' > "$rf"; return 0; }
  result="$(printf '%s' "$raw" | jq -r '.result // .text // empty')"
  [ -n "$result" ] || result="$raw"
  json="$(printf '%s\n' "$result" | sed '/^```/d')"
  if printf '%s' "$json" | jq -e . >/dev/null 2>&1; then
    printf '%s' "$json" | jq . > "$rf"
  else
    echo '{"findings":[]}' > "$rf"     # never let one bad shard fail the run
  fi
}

# ---- review one shard: grounded pass, then optional adversarial downgrade -----
review_shard() { # <shard-key>
  local key="$1" safe rev_prompt rev_json adv_prompt
  safe="$(printf '%s' "$key" | tr -c 'a-zA-Z0-9_.-' '_')"
  rev_prompt="$shard_dir/$safe.review.txt"
  rev_json="$shard_dir/$safe.json"

  local frame_list audit_findings
  frame_list="$(jq -r --arg k "$key" ".captures[]|select(.captured and ($sel_field==\$k))|.frame" "$audit")"
  audit_findings="$(jq -c --arg k "$key" "[.findings[]|select($sel_field==\$k)]" "$audit")"
  [ -n "$frame_list" ] || { echo '{"findings":[]}' > "$rev_json"; return 0; }

  {
    cat <<'HEAD'
You are a senior product designer doing a LAYOUT & USABILITY review of a few
screenshots ("frames") of one surface of a web UI, captured at one or more
viewport widths. Judge how well-designed and usable each frame is. Apply the
heuristic catalog below.

EVIDENCE RULES (these make the review trustworthy — follow them exactly):
1. The frame PNG files are the ONLY admissible evidence. Use the Read tool to
   open every frame listed below and look closely before judging.
2. Every finding MUST cite the frame filename and quote what is LITERALLY
   visible that constitutes the defect. No frame, no finding. Do not infer
   beyond the pixels or invent UI you did not see.
3. Honour each check's `not_this` guard — do NOT report a false positive. When
   unsure whether something is a real defect, DROP it. A short, high-precision
   list beats a long speculative one.
4. The "ALREADY-KNOWN AUDIT FINDINGS" below were measured deterministically from
   the live DOM (overflow, off-screen, tiny text/targets, stray tokens, WCAG
   contrast/labels). Treat them as TRUE — do not re-report them. You may
   reference one for context, but your job is the JUDGEMENT calls metrics can't
   make (hierarchy, balance, affordance, responsive quality, empty states).
5. Use the check `id` and `severity` from the catalog. Pick the viewport the
   frame was captured at (it is in the filename after "@").

OUTPUT: print ONLY a single raw JSON object (no prose, no ``` fences):
{ "findings": [
    { "check":"<catalog id>", "severity":"error|warn|info",
      "frame":"02-home-story-cards@mobile.png", "viewport":"mobile",
      "observation":"<literal, what is visible that is wrong>",
      "recommendation":"<one concrete fix>", "confidence":0.0 }
] }
If the surface looks good, return {"findings":[]}.
HEAD
    echo; echo "## HEURISTIC CATALOG"; echo; echo '```yaml'; cat "$heuristics"; echo '```'
    if [ -n "$intent_block" ]; then
      echo; echo "## DESIGN INTENT (context — what this UI is for)"; echo; cat "$intent_block"
    fi
    echo; echo "## ALREADY-KNOWN AUDIT FINDINGS (measured, treat as true, do NOT re-report)"
    echo; echo '```json'; echo "$audit_findings"; echo '```'
    echo; echo "## FRAMES TO REVIEW (in $frames — Read each by filename)"
    echo "$frame_list" | sed 's/^/  - /'
  } > "$rev_prompt"

  call_claude "$rev_prompt" "$rev_json"

  if [ "$adversary" -eq 1 ]; then
    local cur; cur="$(cat "$rev_json")"
    # Only bother with the skeptic if there is something to challenge.
    if printf '%s' "$cur" | jq -e '.findings | length > 0' >/dev/null 2>&1; then
      adv_prompt="$shard_dir/$safe.adv.txt"
      {
        cat <<'HEAD'
You are an adversarial verifier guarding against false positives in a UI review.
Below is a prior set of findings for a few frames, plus the frames themselves.
For EACH finding: Read its cited frame and confirm the defect is ACTUALLY,
clearly visible there and is not excused by the catalog's `not_this` guard.

You may ONLY remove findings or LOWER their severity (error→warn→info). Never add
a finding and never raise a severity. If a finding is real and well-cited, keep
it unchanged. Delete any finding whose frame doesn't clearly show it, that
restates an already-known measured audit finding, or that the `not_this` guard
excuses.

OUTPUT: print ONLY the revised object {"findings":[...]} (same shape, raw JSON,
no prose, no ``` fences).
HEAD
        echo; echo "## HEURISTIC CATALOG (for the not_this guards)"; echo; echo '```yaml'; cat "$heuristics"; echo '```'
        echo; echo "## PRIOR FINDINGS"; echo; echo '```json'; printf '%s' "$cur"; echo; echo '```'
        echo; echo "## FRAMES (in $frames — Read each cited one)"
        echo "$frame_list" | sed 's/^/  - /'
      } > "$adv_prompt"
      call_claude "$adv_prompt" "$shard_dir/$safe.verified.json"
      mv "$shard_dir/$safe.verified.json" "$rev_json"
    fi
  fi
  # Tag every finding with its shard for traceability.
  jq --arg s "$key" '{shard:$s, findings:(.findings // [] | map(. + {shard:$s}))}' "$rev_json" \
    > "$rev_json.tagged" && mv "$rev_json.tagged" "$rev_json"
}

echo "▸ reviewing ${#shards[@]} shards (by $shard_by) · model $model · up to $jobs parallel agents…" >&2

# Fan out with a simple concurrency throttle.
pids=()
for key in "${shards[@]}"; do
  review_shard "$key" &
  pids+=("$!")
  while [ "$(jobs -rp | wc -l)" -ge "$jobs" ]; do wait -n 2>/dev/null || true; done
done
wait

# Merge all shard verdicts into one vision.json.
jq -s '{
  shards:   [ .[].shard ],
  findings: [ .[].findings[]? ]
}' "$shard_dir"/*.json > "$out"

n="$(jq '.findings | length' "$out")"
echo "▸ ${#shards[@]} shards reviewed → $n vision findings → $out" >&2
