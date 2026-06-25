#!/usr/bin/env bash
# kitsoki-ui-review · STAGE 1 (deterministic, no LLM): capture the evidence.
#
# Rebuilds the SPA into bin/kitsoki (the binary serves the UI via go:embed, so a
# stale binary serves a stale UI), then runs the tour-review Playwright spec,
# which walks the tour manifest at each viewport and emits:
#
#   .artifacts/ui-review/frames/NN-<step>@<viewport>.png
#   .artifacts/ui-review/audit.json   (DOM-geometry + axe findings + step map)
#
# No LLM, no cost, reproducible. Override viewports with UI_REVIEW_VIEWPORTS
# (e.g. "desktop,mobile"); WEB_CHAT_PACE=0 collapses dwells for a fast run.
#
# Usage: capture.sh [--no-build] [--viewports a,b,c] [--fast]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"

build=1 fast=0
while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)  build=0; shift ;;
    --viewports) export UI_REVIEW_VIEWPORTS="$2"; shift 2 ;;
    --fast)      fast=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

# Reap any orphaned capture servers from an interrupted prior run — they hold the
# binary ("Text file busy" on cp) and the capture ports, which silently poisons
# the run (a stale server answers "no stories discovered" and the tour collapses).
pkill -f "kitsoki web --stories-dir" 2>/dev/null || true
sleep 1

if [ "$build" -eq 1 ]; then
  echo "▸ make build (embedding the current SPA)…" >&2
  ( cd "$repo" && make build >/dev/null && cp ./kitsoki bin/kitsoki )
fi
[ -x "$repo/bin/kitsoki" ] || { echo "bin/kitsoki missing — run without --no-build" >&2; exit 2; }

pace_env=()
[ "$fast" -eq 1 ] && pace_env=(WEB_CHAT_PACE=0)

echo "▸ capturing (playwright tour-review)…" >&2
( cd "$repo/tools/runstatus" && env "${pace_env[@]}" \
    pnpm exec playwright test tour-review --project=chromium )

echo "▸ capture complete → $repo/.artifacts/ui-review/" >&2
