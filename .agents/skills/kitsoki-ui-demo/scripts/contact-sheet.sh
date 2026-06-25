#!/usr/bin/env bash
# Tile the numbered per-scene screenshots (NN-<scene>.png) a demo spec writes
# into one storyboard "contact sheet" image — a quick visual review of every
# scene, handy in a PR description.
#
# Each shot is letterboxed onto a uniform dark tile (fullPage screenshots vary
# in height), then arranged into a COLS-wide grid.
#
# Usage: contact-sheet.sh <artifact-dir> [output.png] [--cols N] [--tile-width W]
#   --cols N        columns in the grid (default 3)
#   --tile-width W  tile width px; height follows 1440:900 (default 480)
set -euo pipefail

dir="${1:?usage: contact-sheet.sh <artifact-dir> [output.png] [--cols N] [--tile-width W]}"
shift || true

out=""
cols=3
tilew=480
while [ $# -gt 0 ]; do
  case "$1" in
    --cols)       cols="$2"; shift 2 ;;
    --tile-width) tilew="$2"; shift 2 ;;
    *)            out="$1"; shift ;;
  esac
done

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not on PATH" >&2; exit 1; }
[ -d "$dir" ] || { echo "no such dir: $dir" >&2; exit 1; }
[ -n "$out" ] || out="${dir%/}/contact-sheet.png"

# Tile height tracks the 1440x900 capture aspect; background matches the SPA.
tileh=$(( tilew * 900 / 1440 ))
bg="0x0b0f17"

# Numbered scene PNGs, in order; skip any prior contact sheet.
mapfile -t files < <(find "$dir" -maxdepth 1 -name '[0-9]*.png' ! -name 'contact-sheet.png' | sort)
n=${#files[@]}
[ "$n" -gt 0 ] || { echo "no NN-*.png screenshots in $dir" >&2; exit 1; }
rows=$(( (n + cols - 1) / cols ))

# Build the ffmpeg input list + a filter that uniform-pads each shot then tiles.
inputs=()
filt=""
for ((j=0; j<n; j++)); do
  inputs+=( -i "${files[$j]}" )
  filt="${filt}[${j}:v]scale=${tilew}:${tileh}:force_original_aspect_ratio=decrease,"
  filt="${filt}pad=${tilew}:${tileh}:(ow-iw)/2:(oh-ih)/2:color=${bg},setsar=1[t${j}];"
done
for ((j=0; j<n; j++)); do filt="${filt}[t${j}]"; done
filt="${filt}concat=n=${n}:v=1:a=0[seq];[seq]tile=${cols}x${rows}:padding=10:margin=10:color=${bg}"

ffmpeg -y -loglevel error "${inputs[@]}" -filter_complex "$filt" -frames:v 1 "$out"

echo "wrote $out (${n} scenes, ${cols}x${rows} grid)"
