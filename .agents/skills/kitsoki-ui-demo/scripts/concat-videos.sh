#!/usr/bin/env bash
# concat-videos.sh — stitch demo MP4s (and optional title-card PNGs) into one
# shareable MP4, the reusable compositor for MULTI-ACT / CROSS-SITE demos (the
# first being the gh-issues bug→review→triage demo, whose GitHub act is recorded
# separately from the two kitsoki acts and composited here).
#
# Each segment is normalised to a common size/fps (letterboxed on the brand
# colour, never stretched) and muxed through an mpegts intermediate so the final
# concat is a clean stream copy — robust across inputs that differ slightly in
# resolution or frame timing (Playwright recordings vs. looped still cards).
#
# Usage:
#   concat-videos.sh <out.mp4> <segment> [<segment> ...] [--size WxH] [--fps N]
#
#   segment forms:
#     video:/path/clip.mp4            a recorded act
#     card:/path/card.png[:seconds]   a title card (default 2.5s), looped
#
# Example:
#   concat-videos.sh out.mp4 \
#     card:act1.png:2.5 video:report-bug-demo.mp4 \
#     card:act2.png:2.5 video:gh-issue-review-demo.mp4 \
#     card:act3.png:2.5 video:dev-story-bugfix-demo.mp4
set -euo pipefail

SIZE="1600x900"
FPS="30"
BG="0x070d1a"   # the demo brand backdrop (matches the recording curtain)

OUT=""
SEGMENTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --size) SIZE="$2"; shift 2 ;;
    --fps)  FPS="$2"; shift 2 ;;
    video:*|card:*) SEGMENTS+=("$1"); shift ;;
    *)
      if [ -z "$OUT" ]; then OUT="$1"; shift
      else echo "concat-videos: unexpected arg: $1" >&2; exit 2; fi ;;
  esac
done

if [ -z "$OUT" ] || [ "${#SEGMENTS[@]}" -eq 0 ]; then
  echo "usage: concat-videos.sh <out.mp4> <video:clip.mp4|card:img.png[:sec]> ..." >&2
  exit 2
fi
command -v ffmpeg >/dev/null || { echo "concat-videos: ffmpeg not on PATH" >&2; exit 2; }

W="${SIZE%x*}"; H="${SIZE#*x}"
VF="scale=${W}:${H}:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2:color=${BG},fps=${FPS},setsar=1,format=yuv420p"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
parts=()
i=0
for seg in "${SEGMENTS[@]}"; do
  ts="$TMP/part_$(printf '%03d' "$i").ts"
  kind="${seg%%:*}"
  rest="${seg#*:}"
  case "$kind" in
    video)
      [ -f "$rest" ] || { echo "concat-videos: missing video $rest" >&2; exit 2; }
      ffmpeg -y -loglevel error -i "$rest" \
        -vf "$VF" -c:v libx264 -preset veryfast -crf 20 -an -f mpegts "$ts"
      ;;
    card)
      img="${rest%:*}"; sec="${rest##*:}"
      [ "$sec" = "$rest" ] && sec="2.5"   # no :seconds suffix
      [ -f "$img" ] || { echo "concat-videos: missing card $img" >&2; exit 2; }
      ffmpeg -y -loglevel error -loop 1 -t "$sec" -i "$img" \
        -vf "$VF" -c:v libx264 -preset veryfast -crf 20 -an -f mpegts "$ts"
      ;;
    *) echo "concat-videos: bad segment $seg" >&2; exit 2 ;;
  esac
  parts+=("$ts")
  i=$((i+1))
done

list="$(IFS='|'; echo "${parts[*]}")"
ffmpeg -y -loglevel error -i "concat:${list}" -c copy -movflags +faststart "$OUT"
echo "concat-videos: wrote $OUT ($(du -h "$OUT" | cut -f1), ${#SEGMENTS[@]} segments)"
