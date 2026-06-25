#!/usr/bin/env bash
# One-shot UI-demo QA — the inverse of the kitsoki-ui-demo recording pipeline.
#
#   extract frames → contact sheet → grounded vision review → gated report
#
# Reliability comes from a deterministic frame set, an evidence-cited verdict,
# and an adversarial downgrade-only pass (see SKILL.md). Exit code is the gate:
# 0 = pass, 1 = a blocking scenario failed, 2 = pipeline error.
#
# Usage: qa.sh <video> --feature <file> --scenarios <file>
#          [--frames <dir>] [--out <dir>] [--model M]
#          [--max-frames N] [--scene TH] [--blank-min-coverage F]
#          [--no-adversary] [--strict] [--blank-strict]
#
#   --frames <dir>  use existing labeled frames (e.g. the kitsoki-ui-demo skill's
#                   NN-<scene>.png) as ground truth instead of extracting. Highest
#                   fidelity when available — and, for full-editor (VS Code)
#                   videos, the most reliable input (no scene-extraction artifacts;
#                   see SKILL.md → "Full-editor (VS Code) evidence").
#   --scene TH      scene-change sensitivity passed to extract-frames.sh (default
#                   0.30); ignored when --frames is supplied.
#   --blank-min-coverage F
#                   min fraction a flat block must cover to be flagged by
#                   blank-scan.sh (default 0.10). Raise to ~0.15 for full-editor
#                   videos, whose legitimate dark editor-chrome edge strips would
#                   otherwise trip the default — see SKILL.md.
#   --out <dir>     artifact dir (default .artifacts/ui-qa/<video-stem>)
#   --chapters <f>  chapter sidecar for the deterministic pacing scan (default:
#                   auto-detect <video>.chapters.json next to the MP4)
#   --pacing-min N  minimum readable on-screen window per chapter, ms (default 1500)
#   --pacing-strict promote pacing flags from advisory to a blocking gate
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
demo_scripts="$here/../../kitsoki-ui-demo/scripts"   # reuse the recorder's contact sheet

video="${1:?usage: qa.sh <video> --feature <f> --scenarios <f> [opts]}"
shift || true

feature="" scenarios="" frames="" outdir="" model="" max=48 chapters="" pacing_min="" scene="" blank_min_cov=""
rrweb="" rrweb_min_dwell=""
adv_flag="" strict_flag="" blank_strict_flag="" pacing_strict_flag="" rrweb_strict_flag=""
while [ $# -gt 0 ]; do
  case "$1" in
    --feature)     feature="$2"; shift 2 ;;
    --scenarios)   scenarios="$2"; shift 2 ;;
    --frames)      frames="$2"; shift 2 ;;
    --out)         outdir="$2"; shift 2 ;;
    --model)       model="$2"; shift 2 ;;
    --max-frames)  max="$2"; shift 2 ;;
    --chapters)    chapters="$2"; shift 2 ;;
    --pacing-min)  pacing_min="$2"; shift 2 ;;
    --scene)       scene="$2"; shift 2 ;;
    --blank-min-coverage) blank_min_cov="$2"; shift 2 ;;
    --no-adversary) adv_flag="--no-adversary"; shift ;;
    --strict)      strict_flag="--strict"; shift ;;
    --blank-strict) blank_strict_flag="--blank-strict"; shift ;;
    --pacing-strict) pacing_strict_flag="--pacing-strict"; shift ;;
    --rrweb)         rrweb="$2"; shift 2 ;;
    --rrweb-min-dwell) rrweb_min_dwell="$2"; shift 2 ;;
    --rrweb-strict)  rrweb_strict_flag="--rrweb-strict"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

[ -f "$feature" ]   || { echo "--feature file required" >&2; exit 2; }
[ -f "$scenarios" ] || { echo "--scenarios file required" >&2; exit 2; }
if [ -z "$frames" ]; then
  [ -f "$video" ] || { echo "no such video: $video" >&2; exit 2; }
fi

stem="$(basename "${video%.*}")"
[ -n "$outdir" ] || outdir=".artifacts/ui-qa/$stem"
mkdir -p "$outdir"
frames_dir="$outdir/frames"

# 1. Frames — prefer caller-supplied labeled set, else extract deterministically.
if [ -n "$frames" ]; then
  [ -d "$frames" ] || { echo "no such --frames dir: $frames" >&2; exit 2; }
  echo "▸ using labeled frames from $frames"
  frames_dir="$frames"
else
  echo "▸ extracting frames → $frames_dir"
  extract_args=( "$video" "$frames_dir" --max "$max" )
  [ -n "$scene" ] && extract_args+=( --scene "$scene" )
  "$here/extract-frames.sh" "${extract_args[@]}"
fi

# 2. Contact sheet (best-effort; reuses the recorder's tiler). Non-fatal.
if [ -x "$demo_scripts/contact-sheet.sh" ]; then
  "$demo_scripts/contact-sheet.sh" "$frames_dir" "$outdir/contact-sheet.png" \
    && echo "▸ contact sheet → $outdir/contact-sheet.png" \
    || echo "  (contact sheet skipped)"
fi

# 2b. Deterministic blank/solid-region scan (no LLM). Advisory by default —
#     surfaces frames with a large solid white/black block for human review;
#     --blank-strict promotes them to blocking. Never aborts the run itself.
blank_scan="$outdir/blank-scan.json"
blank_args=( "$frames_dir" --out "$blank_scan" )
[ -n "$blank_min_cov" ] && blank_args+=( --min-coverage "$blank_min_cov" )
"$here/blank-scan.sh" "${blank_args[@]}" || true

# 2c. Deterministic pacing scan (no LLM) over the chapter sidecar — flags
#     narrated moments that flash by too fast to read (the WEB_CHAT_PACE=0
#     fast-validation footgun). Auto-detects <video>.chapters.json beside the MP4
#     unless --chapters is given. Advisory by default; --pacing-strict blocks.
pacing_scan=""
if [ -z "$chapters" ] && [ -f "${video}.chapters.json" ]; then
  chapters="${video}.chapters.json"
fi
if [ -n "$chapters" ] && [ -f "$chapters" ]; then
  pacing_scan="$outdir/pacing-scan.json"
  pacing_args=( "$chapters" --out "$pacing_scan" )
  [ -n "$pacing_min" ] && pacing_args+=( --min-ms "$pacing_min" )
  "$here/pacing-scan.sh" "${pacing_args[@]}" || true
else
  echo "  (no chapter sidecar — pacing scan skipped; pass --chapters to enable)"
fi

# 2d. Deterministic rrweb-pacing scan (no LLM) over the embedded tour clip(s) —
#     flags content reveals crammed below the readable dwell (the "last messages
#     are super-rushed" defect), which neither the frame sampler nor the vision
#     review can see. Pass --rrweb <clip.rrweb.json | dir>. Advisory by default;
#     --rrweb-strict blocks.
rrweb_scan=""
if [ -n "$rrweb" ]; then
  rrweb_scan="$outdir/rrweb-pacing-scan.json"
  rrweb_args=( "$rrweb" --out "$rrweb_scan" )
  [ -n "$rrweb_min_dwell" ] && rrweb_args+=( --min-dwell "$rrweb_min_dwell" )
  node "$here/rrweb-pacing-scan.mjs" "${rrweb_args[@]}" || true
fi

# 3. Grounded, adversarially-verified vision review → verdict.json
verdict="$outdir/verdict.json"
review_args=( --frames "$frames_dir" --feature "$feature" \
              --scenarios "$scenarios" --out "$verdict" )
[ -n "$model" ]    && review_args+=( --model "$model" )
[ -n "$adv_flag" ] && review_args+=( "$adv_flag" )
"$here/qa-review.sh" "${review_args[@]}"

# 4. Gated report — exit code propagates as the QA gate.
echo
report_args=( "$verdict" --out "$outdir/qa-report.md" $strict_flag \
  --blank-scan "$blank_scan" $blank_strict_flag )
[ -n "$pacing_scan" ] && report_args+=( --pacing-scan "$pacing_scan" $pacing_strict_flag )
[ -n "$rrweb_scan" ] && report_args+=( --rrweb-scan "$rrweb_scan" $rrweb_strict_flag )
"$here/report.sh" "${report_args[@]}"
rc=$?
echo
echo "QA artifacts in $outdir/ : verdict.json, qa-report.md, contact-sheet.png, frames/"
exit $rc
