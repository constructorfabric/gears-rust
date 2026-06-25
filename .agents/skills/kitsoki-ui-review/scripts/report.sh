#!/usr/bin/env bash
# kitsoki-ui-review · STAGE 3 (deterministic, no LLM): merge + gate + report.
#
# Combines the deterministic capture (audit.json: DOM-geometry + axe a11y, each
# carrying the element's selector/path/outerHTML/computed-styles/rect) with the
# multi-agent vision review (vision.json) into one verdict.json and a detailed,
# fixable review-report.md, then sets the GATE exit code from the merged
# severities — it does NOT trust any model's own verdict.
#
# The report is built to be ACTIONABLE: every finding records the DOM state it
# was observed in and a copy-pasteable reproduction recipe (server command +
# viewport + the tour step that reaches the surface), joined from audit.json.
#
#   exit 0  no blocking findings
#   exit 1  at least one blocking finding (error always; warn under --strict)
#   exit 2  pipeline error (bad inputs)
#
# Usage: report.sh --audit <audit.json> --vision <vision.json>
#                  --out <review-report.md> --verdict <verdict.json> [--strict]
set -euo pipefail

audit="" vision="" out="" verdict="" strict=0
while [ $# -gt 0 ]; do
  case "$1" in
    --audit)   audit="$2"; shift 2 ;;
    --vision)  vision="$2"; shift 2 ;;
    --out)     out="$2"; shift 2 ;;
    --verdict) verdict="$2"; shift 2 ;;
    --strict)  strict=1; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
command -v jq >/dev/null 2>&1 || { echo "jq not on PATH" >&2; exit 2; }
[ -f "$audit" ]  || { echo "no such audit.json: $audit" >&2; exit 2; }
[ -f "$vision" ] || { echo "no such vision.json: $vision" >&2; exit 2; }
[ -n "$out" ] && [ -n "$verdict" ] || { echo "--out and --verdict are required" >&2; exit 2; }
mkdir -p "$(dirname "$out")" "$(dirname "$verdict")"

# ── Build the merged verdict.json. Normalise both sources to one finding shape,
#    attach DOM context (deterministic only) and a reproduction recipe (joined
#    from audit.captures + audit.server by surface+viewport), then dedup. ──────
jq -n \
  --slurpfile a "$audit" \
  --slurpfile v "$vision" \
  --argjson strict "$strict" '
  ($a[0]) as $audit |
  ($v[0]) as $vision |
  ($audit.server // {}) as $server |
  ($audit.steps // []) as $steps |

  # repro recipe for a (surface,viewport): join to the capture that took the frame
  def repro($surface; $viewport):
    ( $audit.captures // [] | map(select(.step==$surface and .viewport==$viewport)) | .[0] ) as $cap |
    ( $steps | map(select(.id==$surface)) | .[0] ) as $st |
    {
      cmd:       ($server.cmd // ""),
      base:      ($server.base // ""),
      startTour: ($server.startTour // ""),
      viewport:  (if $cap then "\($cap.width)x\($cap.height)" else $viewport end),
      route:     ($cap.route // ($st.route // "any")),
      step:      $surface,
      stepTitle: ($st.title // $surface),
      url:       ($cap.url // "")
    };

  ([ $audit.findings[]? | {
      source: (if (.source=="a11y") then "a11y" else "geometry" end),
      check, severity, surface:.step, viewport, frame,
      detail, recommendation:"",
      selector:(.selector // ""), path:(.path // ""), html:(.html // ""),
      styles:(.styles // {}), rect:(.rect // null),
      target:(.target // ""), failureSummary:(.failureSummary // ""), helpUrl:(.helpUrl // ""),
      repro: repro(.step; .viewport)
    } ]) as $det |
  ([ $vision.findings[]? | {
      source:"vision", check, severity, surface:.shard, viewport,
      frame, detail:.observation, recommendation:(.recommendation // ""),
      selector:"", path:"", html:"", styles:{}, rect:null,
      target:"", failureSummary:"", helpUrl:"",
      repro: repro(.shard; .viewport)
    } ]) as $vis |

  ($det + $vis) as $raw |
  ([ $raw
     | group_by([.source, .check, .selector, .surface, .viewport])
     | .[]
     | (map(.frame) | map(select(. != "")) | unique) as $frames
     | (.[0] + { count: length, frames: $frames }) ]) as $all |
  ($all | map(select(.severity=="error"))) as $errs |
  ($all | map(select(.severity=="warn"))) as $warns |
  ($all | map(select(.severity=="info"))) as $infos |
  (($errs|length) + (if $strict==1 then ($warns|length) else 0 end)) as $blocking |
  {
    overall: (if $blocking>0 then "fail" else "pass" end),
    strict: ($strict==1),
    server: $server,
    summary: {
      error:($errs|length), warn:($warns|length), info:($infos|length),
      blocking:$blocking,
      by_source: {
        geometry: ($all|map(select(.source=="geometry"))|length),
        a11y:     ($all|map(select(.source=="a11y"))|length),
        vision:   ($all|map(select(.source=="vision"))|length)
      }
    },
    findings: ($all | sort_by(
      (if .severity=="error" then 0 elif .severity=="warn" then 1 else 2 end),
      .surface, .viewport))
  }' > "$verdict"

# ── Render review-report.md from the verdict. ───────────────────────────────
{
  overall="$(jq -r '.overall' "$verdict")"
  strictnote=""; [ "$strict" -eq 1 ] && strictnote=" *(strict: warnings block)*"
  echo "# UI layout & usability review"
  echo
  if [ "$overall" = "pass" ]; then echo "**Gate: ✅ PASS**$strictnote"; else echo "**Gate: ❌ FAIL**$strictnote"; fi
  echo
  jq -r '"- **\(.summary.error)** error · **\(.summary.warn)** warn · **\(.summary.info)** info " +
         "— \(.summary.blocking) blocking\n" +
         "- by source: \(.summary.by_source.geometry) geometry · " +
         "\(.summary.by_source.a11y) a11y · \(.summary.by_source.vision) vision"' "$verdict"
  echo
  echo "Each finding below records the **DOM state** it was seen in and a"
  echo "**reproduction recipe**. Frames are in \`frames/\`."
  echo

  # ---- Index tables (quick scan) ----
  for sev in error warn info; do
    cnt="$(jq --arg s "$sev" '[.findings[]|select(.severity==$s)]|length' "$verdict")"
    [ "$cnt" -eq 0 ] && continue
    case "$sev" in
      error) echo "## ❌ Errors ($cnt)";;
      warn)  echo "## ⚠️  Warnings ($cnt)";;
      info)  echo "## ℹ️  Info ($cnt)";;
    esac
    echo
    echo "| # | surface | viewport | source | check | observation | frame | seen |"
    echo "|---|---|---|---|---|---|---|---|"
    jq -r --arg s "$sev" '
      [ .findings[] | select(.severity==$s) ] | to_entries[] |
      .key as $i | .value as $f | $f |
      (.frames // []) as $fr |
      (if ($fr|length)>0 then $fr[0] else (.frame // "") end) as $cite |
      "| \($i+1) | \(.surface) | \(.viewport) | \(.source) | `\(.check)` | " +
      "\((.detail // "")|gsub("\\|";"\\\\|")|gsub("\n";" ")|.[0:90]) | " +
      "\(if $cite=="" then "—" else "`\($cite)`" end) | " +
      "\(if (.count // 1)>1 then "×\(.count)" else "1" end) |"' "$verdict"
    echo
  done

  # ---- Detailed, reproducible cards (errors + warnings) ----
  echo "---"
  echo
  echo "## Details — how to reproduce & fix"
  echo
  for sev in error warn; do
    has="$(jq --arg s "$sev" '[.findings[]|select(.severity==$s)]|length' "$verdict")"
    [ "$has" -eq 0 ] && continue
    label="Errors"; [ "$sev" = "warn" ] && label="Warnings"
    echo "### $label"
    echo
    jq -r --arg s "$sev" '
      .findings[] | select(.severity==$s) |
      (.frames // []) as $fr |
      (if ($fr|length)>0 then $fr[0] else (.frame // "") end) as $cite |
      (.styles // {} | to_entries
        | map(select(.value != "" and .value != null and .value != "none" and .value != "normal"))
        | map("`\(.key): \(.value)`") | join(" · ")) as $styles |
      "#### [\(.severity)] `\(.check)` — \(.surface) @ \(.viewport)\n" +
      "\n" +
      "- **Observed:** \(.detail // "")\n" +
      (if (.recommendation // "") != "" then "- **Fix:** \(.recommendation)\n" else "" end) +
      (if $cite != "" then "- **Frame:** `frames/\($cite)`" + (if (.count//1)>1 then " (seen on \(.count) frames)" else "" end) + "\n" else "" end) +
      "\n**DOM state**\n" +
      (if (.selector // "") != "" then "- selector: `\(.selector)`\n" else "" end) +
      (if (.path // "") != "" then "- path: `\(.path)`\n" else "" end) +
      (if .rect and (.rect.w != 0 or .rect.h != 0) then "- rect: x=\(.rect.x) y=\(.rect.y) · \(.rect.w)×\(.rect.h)px\n" else "" end) +
      (if $styles != "" then "- computed: \($styles)\n" else "" end) +
      (if (.failureSummary // "") != "" then "- axe: \(.failureSummary)\n" else "" end) +
      (if (.helpUrl // "") != "" then "- rule: \(.helpUrl)\n" else "" end) +
      (if (.html // "") != "" then "\n```html\n\(.html)\n```\n" else "" end) +
      "\n**Reproduce**\n" +
      "1. `\(.repro.cmd)`\n" +
      "2. Open \(.repro.base)/#/ in a **\(.repro.viewport)** viewport\n" +
      "3. Start the onboarding tour — \(.repro.startTour)\n" +
      "4. Advance to the **\(.repro.stepTitle)** step (surface `\(.repro.step)`, route `\(.repro.route)`)\n" +
      "5. Observe: \(.detail // "")\n" +
      "\n---\n"' "$verdict"
    echo
  done
} > "$out"

echo "wrote $verdict and $out" >&2
jq -r '"gate: " + (.overall|ascii_upcase) + " (" + (.summary.blocking|tostring) + " blocking)"' "$verdict" >&2

# Gate exit code.
blocking="$(jq '.summary.blocking' "$verdict")"
[ "$blocking" -eq 0 ] || exit 1
exit 0
