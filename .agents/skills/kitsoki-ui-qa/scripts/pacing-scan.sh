#!/usr/bin/env bash
# pacing-scan.sh — DETERMINISTIC (no-LLM) demo pacing detector.
#
# A tour/demo video is well-paced only if each narrated moment stays on screen
# long enough to read. The recorder already emits a producer-agnostic CHAPTER
# SIDECAR next to the MP4 (`<video>.chapters.json`) mapping every tour step to
# the [start_ms,end_ms] window it actually occupied in the final video (see the
# kitsoki-ui-demo skill's ChapterRecorder / writeChapters). That sidecar is the
# ground truth for pacing — no vision needed, no flakiness: same video in → same
# windows out.
#
# The classic pacing defect this catches: a demo recorded in the FAST-VALIDATION
# posture (WEB_CHAT_PACE=0) collapses every `dwell(step.dwellMs)` to ~0, so the
# popovers flash by in tens of milliseconds — a 12-second blur instead of a
# readable ~80-second walk. The vision QA gate can't see this (each individual
# frame still looks correct); only the per-chapter DURATION reveals it.
#
# How it works (pure jq — no LLM, no ffmpeg):
#   • read the chapter array, compute window_ms = end_ms - start_ms per chapter;
#   • flag any chapter whose window is below --min-ms (a viewer can't read a
#     titled popover card in under ~1.5s);
#   • flag the whole video if its total span is below --min-total-ms (a sanity
#     floor — a multi-step narrated tour that runs in a couple seconds is a blur
#     regardless of any single window).
#
# Usage:
#   pacing-scan.sh <chapters.json> [--out scan.json]
#                  [--min-ms N] [--min-total-ms N] [--fail-on-find]
# Defaults: --min-ms 1500 --min-total-ms 0  (total floor off unless set)
# Exit: 0 = scanned OK (no flags, or flags but advisory);
#       3 = flags found AND --fail-on-find; 2 = usage/tool error.
#
# This is an ADVISORY nudge by default (surfaced in qa-report.md, never blocks);
# report.sh promotes it to a hard gate under --pacing-strict.
set -euo pipefail

command -v jq >/dev/null 2>&1 || { echo "jq not on PATH" >&2; exit 2; }

src="${1:-}"; shift || true
[ -n "$src" ] || { echo "usage: pacing-scan.sh <chapters.json> [opts]" >&2; exit 2; }
[ -f "$src" ] || { echo "no such chapters sidecar: $src" >&2; exit 2; }
jq -e 'type=="array"' "$src" >/dev/null 2>&1 || {
  echo "chapters sidecar is not a JSON array: $src" >&2; exit 2; }

out="" min_ms=1500 min_total_ms=0 fail_on_find=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out)           out="$2"; shift 2 ;;
    --min-ms)        min_ms="$2"; shift 2 ;;
    --min-total-ms)  min_total_ms="$2"; shift 2 ;;
    --fail-on-find)  fail_on_find=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

report="$(jq --argjson min "$min_ms" --argjson mintot "$min_total_ms" '
  ( [ .[] | { id: (.id // .step_id // "?"),
              label: (.label // ""),
              window_ms: (((.end_ms // 0) - (.start_ms // 0)) | floor) } ] ) as $ch |
  ( [ $ch[].window_ms ] ) as $w |
  ( if ($w|length) > 0 then ($w | add) else 0 end ) as $total |
  ( [ $ch[] | select(.window_ms < $min)
              | . + { issue: "on screen \(.window_ms)ms < \($min)ms minimum readable window — popover flashes by, unreadable" } ] ) as $short |
  ( if ($total < $mintot) and ($mintot > 0)
      then [ { id: "__total__", label: "whole video",
               window_ms: $total,
               issue: "total narrated span \($total)ms < \($mintot)ms — the whole walk is too fast to follow" } ]
      else [] end ) as $tot_flag |
  { min_ms: $min,
    min_total_ms: $mintot,
    chapters_total: ($ch|length),
    total_ms: $total,
    median_ms: ( if ($w|length) > 0
                 then ($w | sort | .[ (length/2|floor) ]) else 0 end ),
    shortest_ms: ( if ($w|length) > 0 then ($w|min) else 0 end ),
    flagged: ($short + $tot_flag),
    chapters: $ch }
' "$src")"

if [ -n "$out" ]; then
  printf '%s\n' "$report" > "$out"
else
  printf '%s\n' "$report"
fi

n_flagged="$(printf '%s' "$report" | jq '(.flagged // []) | length')"
if [ "$n_flagged" -gt 0 ]; then
  echo "pacing-scan: $n_flagged chapter(s) below the readable-window floor — review:" >&2
  printf '%s' "$report" | jq -r '.flagged[] | "  \(.id): \(.issue)"' >&2
else
  total="$(printf '%s' "$report" | jq '.total_ms')"
  echo "pacing-scan: all chapters comfortably paced (total ${total}ms)" >&2
fi

[ "$n_flagged" -gt 0 ] && [ "$fail_on_find" -eq 1 ] && exit 3
exit 0
