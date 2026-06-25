#!/usr/bin/env bash
# Convert a recorded demo video (.mp4 — the canonical artifact — or a legacy
# .webm) into a high-quality looping GIF for embedding in PRs / markdown / chat.
# Uses the two-pass palettegen+paletteuse pipeline (a naive single-pass GIF
# looks muddy and banded).
#
# GIFs are large — keep --width modest (<=900) and --fps low (10-15). For
# anything long or detailed, prefer the MP4.
#
# Usage: webm-to-gif.sh <input.(mp4|webm)> [output.gif] [--fps N] [--width W]
#   --fps N     frame rate (default 12)
#   --width W   output width px, aspect preserved (default 900)
set -euo pipefail

in="${1:?usage: webm-to-gif.sh <input.(mp4|webm)> [output.gif] [--fps N] [--width W]}"
shift || true

out=""
fps=12
width=900
while [ $# -gt 0 ]; do
  case "$1" in
    --fps)   fps="$2"; shift 2 ;;
    --width) width="$2"; shift 2 ;;
    *)       out="$1"; shift ;;
  esac
done

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not on PATH" >&2; exit 1; }
[ -f "$in" ] || { echo "no such file: $in" >&2; exit 1; }
[ -n "$out" ] || out="${in%.*}.gif"

pal="$(mktemp -t webm-to-gif-pal).png"
trap 'rm -f "$pal"' EXIT

scale="fps=${fps},scale=${width}:-1:flags=lanczos"
ffmpeg -y -loglevel error -i "$in" -vf "${scale},palettegen=stats_mode=diff" "$pal"
ffmpeg -y -loglevel error -i "$in" -i "$pal" \
  -lavfi "${scale}[x];[x][1:v]paletteuse=dither=sierra2_4a" "$out"

echo "wrote $out ($(du -h "$out" | cut -f1))"
