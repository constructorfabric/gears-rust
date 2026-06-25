#!/usr/bin/env bash
# Extract a deterministic, reviewable set of frames from a UI demo video — the
# evidence substrate the QA reviewer grounds every verdict in (see SKILL.md).
#
# Two ffmpeg passes are merged so coverage is reliable on any clip:
#   • scene-change  — captures every visual transition (the meaningful moments
#                     in a UI demo are state changes), via select='gt(scene,TH)'
#                     plus the very first frame.
#   • periodic floor— one frame every --interval seconds so long static dwells
#                     are never missed even when nothing "changes".
# Frames from both passes are merged by timestamp, near-duplicates within
# --dedup ms dropped, capped at --max (subsampled evenly), then renumbered
#   NNNN-<ms>ms.png
# — digit-leading on purpose so the kitsoki-ui-demo skill's contact-sheet.sh
# tiles them as-is. A frames.json manifest (index → timestamp) is written too.
#
# Deterministic: same video + same flags → same frames + same manifest. No LLM.
#
# Already have labeled ground-truth frames (the kitsoki-ui-demo skill's
# NN-<scene>.png)? Skip this — point qa.sh --frames at that dir instead.
#
# Usage: extract-frames.sh <video> <out-dir>
#          [--scene TH] [--interval S] [--dedup MS] [--max N] [--width W]
#   --scene TH    scene-change sensitivity 0..1, lower = more frames (default 0.30)
#   --interval S  periodic-floor seconds between samples (default 4)
#   --dedup MS    drop a frame within MS of the previous kept one (default 700)
#   --max N       hard cap on frames; subsampled evenly if exceeded (default 48)
#   --width W     downscale width px, height auto, lanczos (default 1280)
set -euo pipefail

video="${1:?usage: extract-frames.sh <video> <out-dir> [opts]}"
outdir="${2:?usage: extract-frames.sh <video> <out-dir> [opts]}"
shift 2 || true

scene=0.30 interval=4 dedup=700 max=48 width=1280
while [ $# -gt 0 ]; do
  case "$1" in
    --scene)    scene="$2"; shift 2 ;;
    --interval) interval="$2"; shift 2 ;;
    --dedup)    dedup="$2"; shift 2 ;;
    --max)      max="$2"; shift 2 ;;
    --width)    width="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not on PATH" >&2; exit 1; }
command -v jq     >/dev/null 2>&1 || { echo "jq not on PATH" >&2; exit 1; }
[ -f "$video" ] || { echo "no such file: $video" >&2; exit 1; }

mkdir -p "$outdir"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scene" "$tmp/floor"

# showinfo logs one `pts_time:<sec>` line per frame it forwards, in output order,
# so the i-th pts_time pairs with the i-th %05d.png. We harvest both passes that
# way, then merge purely on timestamp.
emit() { # <subdir> <select-or-fps filter>
  ffmpeg -y -loglevel info -i "$video" \
    -vf "$2,showinfo,scale=${width}:-1:flags=lanczos" -vsync vfr \
    "$tmp/$1/%05d.png" 2> "$tmp/$1/log" || true
}
emit scene "select='gt(scene,${scene})+eq(n,0)'"
emit floor "fps=1/${interval}"

# Build a single "pts<TAB>srcpath" list across both passes.
pair_list="$tmp/pairs.tsv"; : > "$pair_list"
collect() { # <subdir>
  local d="$tmp/$1" i=0 f
  mapfile -t pts < <(grep -o 'pts_time:[0-9.]*' "$d/log" | sed 's/pts_time://')
  for f in "$d"/*.png; do
    [ -e "$f" ] || continue
    printf '%s\t%s\n' "${pts[$i]:-0}" "$f" >> "$pair_list"
    i=$((i+1))
  done
}
collect scene
collect floor

[ -s "$pair_list" ] || { echo "no frames extracted from $video" >&2; exit 1; }

# Sort by timestamp, then drop any frame within --dedup ms of the last kept one.
kept="$tmp/kept.tsv"
sort -n "$pair_list" | awk -v dd="$dedup" '
  BEGIN { last = -1e9 }
  { if (($1*1000) - last >= dd) { print; last = $1*1000 } }
' > "$kept"

# Cap: if over --max, keep an evenly-spaced subsample (always incl. first/last).
total=$(wc -l < "$kept")
final="$tmp/final.tsv"
if [ "$total" -gt "$max" ]; then
  # Pick $max evenly-spaced line numbers, always including the first and last.
  awk -v n="$total" -v m="$max" '
    BEGIN { for (k=0; k<m; k++) pick[int(k*(n-1)/(m-1)+0.5)+1]=1 }
    pick[FNR]
  ' "$kept" > "$final"
  echo "capped $total → $(wc -l < "$final") frames (--max $max)" >&2
else
  cp "$kept" "$final"
fi

# Renumber, copy, and build the manifest.
idx=0; manifest="$tmp/manifest.ndjson"; : > "$manifest"
while IFS=$'\t' read -r t src; do
  idx=$((idx+1))
  ms=$(awk -v t="$t" 'BEGIN{printf "%d", t*1000}')
  name=$(printf '%04d-%dms.png' "$idx" "$ms")
  cp "$src" "$outdir/$name"
  jq -nc --argjson i "$idx" --arg f "$name" --argjson ms "$ms" \
     --arg ts "$(awk -v t="$t" 'BEGIN{printf "%.2fs", t}')" \
     '{index:$i, file:$f, t_ms:$ms, t:$ts}' >> "$manifest"
done < "$final"

jq -s --arg v "$video" '{video:$v, count:length, frames:.}' "$manifest" \
  > "$outdir/frames.json"

echo "wrote $idx frames + frames.json → $outdir"
