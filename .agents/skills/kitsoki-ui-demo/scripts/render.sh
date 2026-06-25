#!/usr/bin/env bash
# One-shot post-production for a recorded demo. The recording spec now emits the
# canonical MP4 directly (see _helpers/server.ts → saveVideoAsMp4 — we ALWAYS
# produce MP4, never .webm, because MP4 plays inline in VS Code / Keynote / Slack
# and .webm does not). So point this at the `<name>-demo.mp4`; it adds a GIF and
# a contact sheet of the sibling NN-*.png scene screenshots.
#
# A legacy `<name>-demo.webm` is still accepted and transcoded to MP4 first.
#
# This does NOT run Playwright — record first (see SKILL.md), then point this at
# the resulting *-demo.mp4.
#
# Usage: render.sh <demo.(mp4|webm)> [--gif-width W] [--no-gif]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
vid="${1:?usage: render.sh <demo.(mp4|webm)> [--gif-width W] [--no-gif]}"
shift || true

gif_width=900
make_gif=1
while [ $# -gt 0 ]; do
  case "$1" in
    --gif-width) gif_width="$2"; shift 2 ;;
    --no-gif)    make_gif=0; shift ;;
    *)           echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

[ -f "$vid" ] || { echo "no such file: $vid" >&2; exit 1; }
dir="$(dirname "$vid")"

# Normalise to MP4 — the canonical share artifact. The spec already emits MP4, so
# this is a no-op in the common path; it only kicks in for a legacy .webm.
case "$vid" in
  *.webm)
    echo "▸ MP4 (transcoding legacy webm)"
    mp4="${vid%.webm}.mp4"
    "$here/webm-to-mp4.sh" "$vid" "$mp4"
    vid="$mp4"
    ;;
  *.mp4) echo "▸ MP4 already present ($vid)" ;;
  *)     echo "unsupported input (need .mp4 or .webm): $vid" >&2; exit 1 ;;
esac

if [ "$make_gif" -eq 1 ]; then
  echo "▸ GIF"
  "$here/webm-to-gif.sh" "$vid" --width "$gif_width"
fi
echo "▸ contact sheet"
"$here/contact-sheet.sh" "$dir" || echo "  (skipped — no NN-*.png screenshots)"

echo "done → $dir"
