#!/usr/bin/env bash
# Convert a Playwright-recorded .webm into a universally-playable H.264 MP4.
#
# .webm (VP8/VP9) is poorly supported in Keynote/PowerPoint, Slack previews and
# iMessage; an H.264 + yuv420p + faststart MP4 plays everywhere. Audio is
# dropped (UI demos are silent).
#
# Usage: webm-to-mp4.sh <input.webm> [output.mp4] [--fps N] [--width W]
#   --fps N     output frame rate (default 30)
#   --width W   scale to width W px, preserving aspect (default: keep source)
set -euo pipefail

in="${1:?usage: webm-to-mp4.sh <input.webm> [output.mp4] [--fps N] [--width W]}"
shift || true

out=""
fps=30
width=""
while [ $# -gt 0 ]; do
  case "$1" in
    --fps)   fps="$2"; shift 2 ;;
    --width) width="$2"; shift 2 ;;
    *)       out="$1"; shift ;;
  esac
done

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not on PATH (try: pnpm -C tools/runstatus playwright:install, or install ffmpeg)" >&2; exit 1; }
[ -f "$in" ] || { echo "no such file: $in" >&2; exit 1; }
[ -n "$out" ] || out="${in%.webm}.mp4"

# yuv420p is required for QuickTime/Safari; pad to even dims (libx264 needs it).
vf="fps=${fps}"
if [ -n "$width" ]; then
  vf="${vf},scale=${width}:-2:flags=lanczos"
else
  vf="${vf},scale=trunc(iw/2)*2:trunc(ih/2)*2"
fi

ffmpeg -y -loglevel error -i "$in" -vf "$vf" \
  -c:v libx264 -preset slow -crf 20 -pix_fmt yuv420p -movflags +faststart -an "$out"

echo "wrote $out ($(du -h "$out" | cut -f1))"
