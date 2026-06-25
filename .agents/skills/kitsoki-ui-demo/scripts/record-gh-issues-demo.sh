#!/usr/bin/env bash
# record-gh-issues-demo.sh — record the three-act, CROSS-SITE gh-issues demo and
# composite it into one MP4. The first kitsoki demo that drives a site other than
# kitsoki (GitHub) in a single video:
#
#   Act 1  report-bug-video         file a bug from the kitsoki web UI  (kitsoki)
#   Act 2  gh-issue-review-video    review the GitHub issue it opened   (GitHub)
#   Act 3  dev-story-bugfix-video   triage it back in the dev-story     (kitsoki)
#
# Each act records its own MP4 (kitsoki acts spawn a real `kitsoki web`; the
# GitHub act drives a deterministic static fixture over file://). concat-videos.sh
# stitches them with act title cards into:
#   .artifacts/gh-issues-demo/gh-issues-cross-site-demo.mp4
#
# Local dev/testing posture: the kitsoki acts run via `go run ./cmd/kitsoki`
# (KITSOKI_WEB_GO_RUN=1) so the server always tracks the working tree — no binary
# to build or keep fresh. Only the go:embed'd SPA must be staged (`make web`, no
# go build). For an actual client/CI capture, build a real binary instead and run
# with KITSOKI_WEB_GO_RUN=0.
#
# Usage: record-gh-issues-demo.sh [--no-stage] [--fast]
#   --no-stage  skip `make web` (reuse the already-staged SPA assets)
#   --fast      WEB_CHAT_PACE=0 — validation pass, no watch-speed dwells
set -euo pipefail

STAGE=1; PACE=1
for a in "$@"; do
  case "$a" in
    --no-stage) STAGE=0 ;;
    --fast) PACE=0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

ROOT="$(git rev-parse --show-toplevel)"
RS="$ROOT/tools/runstatus"
SCR="$ROOT/.agents/skills/kitsoki-ui-demo/scripts"
OUT_DIR="$ROOT/.artifacts/gh-issues-demo"
mkdir -p "$OUT_DIR"

if [ "$STAGE" -eq 1 ]; then
  echo "── staging the go:embed SPA (make web — no binary build) ──"
  ( cd "$ROOT" && make web )
fi

run_spec() { # <spec-basename>
  echo "── recording: $1 (PACE=$PACE, go run) ──"
  ( cd "$RS" && KITSOKI_WEB_GO_RUN=1 WEB_CHAT_PACE="$PACE" pnpm exec playwright test "$1" --project=chromium )
}
run_spec report-bug-video
run_spec gh-issue-review-video
run_spec dev-story-bugfix-video

ACT1="$ROOT/.artifacts/report-bug/report-bug-demo.mp4"
ACT2="$ROOT/.artifacts/gh-issue-review/gh-issue-review-demo.mp4"
ACT3="$ROOT/.artifacts/dev-story-bugfix/dev-story-bugfix-demo.mp4"
for f in "$ACT1" "$ACT2" "$ACT3"; do
  [ -f "$f" ] || { echo "missing act MP4: $f" >&2; exit 1; }
done

echo "── rendering act title cards ──"
C1="$OUT_DIR/card1.png"; C2="$OUT_DIR/card2.png"; C3="$OUT_DIR/card3.png"; C0="$OUT_DIR/card0.png"
card() { ( cd "$RS" && node "$SCR/make-title-card.mjs" "$@" ); }
card "$C0" "Bug → GitHub Issue → Triage" "One bug, three surfaces: the kitsoki web UI, GitHub, and back." "kitsoki demo" "#fbbf24"
card "$C1" "File a bug from the kitsoki web UI" "Capture screenshot + HAR + rrweb, review, and file." "Act 1 · kitsoki" "#38bdf8"
card "$C2" "Review the GitHub issue it opened" "host.gh.ticket.create on constructorfabric/Kitsoki — labels, evidence, metadata." "Act 2 · GitHub" "#a78bfa"
card "$C3" "Triage it back in kitsoki" "Pick the ticket and hand it to the autonomous bugfix pipeline." "Act 3 · kitsoki" "#34d399"

echo "── compositing ──"
OUT="$OUT_DIR/gh-issues-cross-site-demo.mp4"
"$SCR/concat-videos.sh" "$OUT" \
  "card:$C0:3" \
  "card:$C1:2.5" "video:$ACT1" \
  "card:$C2:2.5" "video:$ACT2" \
  "card:$C3:2.5" "video:$ACT3"

echo
echo "✅ cross-site demo: $OUT"
ls -lh "$OUT"
