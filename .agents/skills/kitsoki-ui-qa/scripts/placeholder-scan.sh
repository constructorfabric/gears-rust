#!/usr/bin/env bash
# placeholder-scan.sh — DETERMINISTIC (no-LLM) stuck-placeholder detector.
#
# Catches the failure where a panel sits on a TRANSIENT placeholder forever — a
# "Loading…" spinner that never resolves because the code forgot to lower its
# loading flag, an empty "…" pane, a perpetual skeleton. A placeholder is fine for
# a beat; it is a BUG when it dominates the demo. blank-scan.sh can't see this: a
# "Loading…" pane is mostly themed background with a few words of low-contrast text,
# so it isn't a high-contrast solid block. This reads the actual TEXT (OCR) and
# flags a placeholder string that persists across too many frames.
#
# How it works (pure ffmpeg + tesseract — no PIL): sample the video to frames (or
# take a frames dir / image), OCR each, and count frames whose text matches the
# placeholder pattern (default: the word "loading", case-insensitive — also matches
# "Loading…"). A transient placeholder shows in ~0–1 sampled frames; a STUCK one
# shows for a long CONTIGUOUS run (the panel never resolved) and/or across a large
# fraction of the whole demo. Flag when the longest run >= --min-run OR the fraction
# >= --min-fraction. The run catch is the strong one: ~8s of unbroken "Loading…" is
# stuck even if the rest of the demo is clean.
#
# OCR is best-effort: when `tesseract` is absent the scan SKIPS (advisory, exit 0)
# with a loud note rather than breaking an OCR-less env — the LLM review in
# qa-review.sh remains the always-on catch. Where tesseract exists (this repo's dev
# env) it is a cheap, reproducible gate: same frames in → same finding out.
#
# Usage:
#   placeholder-scan.sh <frames-dir|image|video> [--out scan.json]
#       [--pattern REGEX] [--min-fraction F] [--min-run N] [--fail-on-find]
# Defaults: --pattern '(?i)\bloading\b' --min-fraction 0.34 --min-run 4
# (\b word-boundaries so "uploading"/"reloading"/"downloading" don't match; at
#  ~1 frame/2s a run of 4 ≈ 8s of unbroken placeholder.)
# Exit: 0 = scanned OK (no flag, or flag but advisory, or OCR unavailable);
#       3 = flagged AND --fail-on-find; 2 = usage/IO error.
set -euo pipefail

command -v ffmpeg >/dev/null 2>&1 || { echo "ffmpeg not on PATH" >&2; exit 2; }

src="${1:-}"; shift || true
[ -n "$src" ] || { echo "usage: placeholder-scan.sh <frames-dir|image|video> [opts]" >&2; exit 2; }

out="" pattern='(?i)\bloading\b' min_fraction="0.34" min_run=4 fail_on_find=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out)          out="$2"; shift 2 ;;
    --pattern)      pattern="$2"; shift 2 ;;
    --min-fraction) min_fraction="$2"; shift 2 ;;
    --min-run)      min_run="$2"; shift 2 ;;
    --fail-on-find) fail_on_find=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# OCR is the whole mechanism — if it's missing, skip loudly rather than break.
if ! command -v tesseract >/dev/null 2>&1; then
  echo "placeholder-scan: tesseract not on PATH — skipping OCR placeholder check (advisory)." >&2
  [ -n "$out" ] && printf '{"skipped":true,"reason":"tesseract not on PATH"}\n' > "$out"
  exit 0
fi

# Collect frames — mirrors blank-scan.sh: a video is sampled to frames first.
frames=()
tmp_extracted=""
case "$src" in
  *.mp4|*.webm|*.mov|*.MP4|*.WEBM|*.MOV)
    tmp_extracted="$(mktemp -d)"
    ffmpeg -loglevel error -i "$src" -vf "fps=1/2" "$tmp_extracted/f%04d.png" \
      || { echo "ffmpeg failed to extract frames from $src" >&2; exit 2; }
    while IFS= read -r f; do frames+=("$f"); done < <(find "$tmp_extracted" -type f -name '*.png' | sort)
    ;;
  *)
    if [ -d "$src" ]; then
      while IFS= read -r f; do frames+=("$f"); done < <(find "$src" -maxdepth 1 -type f -name '*.png' | sort)
    else
      frames+=("$src")
    fi
    ;;
esac
[ "${#frames[@]}" -gt 0 ] || { echo "no frames to scan under $src" >&2; exit 2; }
cleanup() { ec=$?; [ -n "$tmp_extracted" ] && rm -rf "$tmp_extracted"; exit "$ec"; }
trap cleanup EXIT

python3 - "$pattern" "$min_fraction" "$min_run" "$fail_on_find" "$out" "${frames[@]}" <<'PY'
import sys, json, re, subprocess, os
pattern = re.compile(sys.argv[1])
min_fraction = float(sys.argv[2])
min_run = int(sys.argv[3])
fail_on_find = sys.argv[4] == "1"
out = sys.argv[5]
frames = sys.argv[6:]

def ocr(path):
    try:
        r = subprocess.run(
            ["tesseract", path, "stdout", "--psm", "6", "-l", "eng"],
            capture_output=True, text=True, timeout=30,
        )
        return r.stdout or ""
    except Exception:
        return ""

matched = []           # frames whose OCR text hit the placeholder pattern
run = longest_run = 0  # longest contiguous run of matched frames (≈ stuck duration)
for f in frames:
    hit = bool(pattern.search(ocr(f)))
    if hit:
        matched.append(os.path.basename(f))
        run += 1
        longest_run = max(longest_run, run)
    else:
        run = 0

total = len(frames)
fraction = (len(matched) / total) if total else 0.0
flagged = total > 0 and (longest_run >= min_run or fraction >= min_fraction)

rec = {
    "frames_total": total,
    "frames_matched": len(matched),
    "fraction": round(fraction, 4),
    "longest_run_frames": longest_run,
    "min_fraction": min_fraction,
    "min_run": min_run,
    "pattern": sys.argv[1],
    "flagged": flagged,
    "matched_frames": matched,
}
if out:
    with open(out, "w") as fh:
        json.dump(rec, fh, indent=2)
        fh.write("\n")

if flagged:
    pct = round(fraction * 100)
    print(
        f"placeholder-scan: a placeholder matching /{sys.argv[1]}/ persists across "
        f"{len(matched)}/{total} frames ({pct}%, longest unbroken run {longest_run} "
        f">= {min_run}) — likely a panel STUCK on a transient placeholder (e.g. a "
        f"'Loading…' flag never lowered).",
        file=sys.stderr,
    )
    for m in matched[:8]:
        print(f"  {m}", file=sys.stderr)
else:
    print(
        f"placeholder-scan: ok — /{sys.argv[1]}/ in {len(matched)}/{total} frames "
        f"({round(fraction*100)}%, longest run {longest_run}); under min-run {min_run} "
        f"and min-fraction {min_fraction}.",
        file=sys.stderr,
    )

sys.exit(3 if (flagged and fail_on_find) else 0)
PY
