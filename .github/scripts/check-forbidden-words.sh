#!/usr/bin/env bash
#
# Scans the lines added by a pull request for forbidden words/phrases.
#
# Only newly added lines (those prefixed with '+' in the diff) are checked.
# Rules are defined in .github/forbidden-words.json.
#
# Usage:
#   check-forbidden-words.sh <base_sha> <head_sha>

set -euo pipefail

BASE_SHA="${1:?base SHA required}"
HEAD_SHA="${2:?head SHA required}"
MERGE_BASE="$(git merge-base "$BASE_SHA" "$HEAD_SHA")" || { echo "::error::Unable to compute merge-base"; exit 2; }
CONFIG_FILE=".github/forbidden-words.json"

[[ -f "$CONFIG_FILE" ]] || { echo "::notice::No forbidden-words.json; skipping check"; exit 0; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq is required"; exit 2; }

rule_count=$(jq '(.rules // []) | length' "$CONFIG_FILE")
[[ "$rule_count" -gt 0 ]] || { echo "No rules defined; nothing to check."; exit 0; }

mapfile -t GLOBAL_EXCLUDES < <(jq -r '.excludePaths // [] | .[]' "$CONFIG_FILE")

matches_any_glob() {
  local path="$1"; shift
  local glob
  for glob in "$@"; do
    [[ -z "$glob" ]] && continue
    if [[ "$path" == $glob ]]; then
      return 0
    fi
  done
  return 1
}

violations=0

mapfile -t CHANGED_FILES < <(git diff --name-only --diff-filter=ACMR "$MERGE_BASE" "$HEAD_SHA")

for file in "${CHANGED_FILES[@]}"; do
  [[ -z "$file" ]] && continue
  matches_any_glob "$file" "${GLOBAL_EXCLUDES[@]}" && continue

  diff_output=$(git diff --unified=0 "$MERGE_BASE" "$HEAD_SHA" -- "$file") || true
  [[ -z "$diff_output" ]] && continue

  added_lines=$(awk '
    /^@@/ {
      for (i = 1; i <= NF; i++) {
        if (substr($i, 1, 1) == "+") {
          s = substr($i, 2)
          split(s, parts, ",")
          line = parts[1] + 0
          break
        }
      }
      next
    }
    /^\+\+\+/ { next }
    /^\+/ {
      content = substr($0, 2)
      printf "%d:%s\n", line, content
      line++
    }
  ' <<< "$diff_output")

  [[ -z "$added_lines" ]] && continue

  for ((r = 0; r < rule_count; r++)); do
    pattern=$(jq -r ".rules[$r].pattern" "$CONFIG_FILE")
    message=$(jq -r ".rules[$r].message // \"\"" "$CONFIG_FILE")
    case_sensitive=$(jq -r ".rules[$r].caseSensitive // false" "$CONFIG_FILE")

    mapfile -t RULE_EXCLUDES < <(jq -r ".rules[$r].excludePaths // [] | .[]" "$CONFIG_FILE")
    if ((${#RULE_EXCLUDES[@]} > 0)) && matches_any_glob "$file" "${RULE_EXCLUDES[@]}"; then
      continue
    fi

    grep_opts=(-P)
    [[ "$case_sensitive" != "true" ]] && grep_opts+=(-i)

    while IFS= read -r entry; do
      [[ -z "$entry" ]] && continue
      lineno="${entry%%:*}"
      content="${entry#*:}"
      if printf '%s' "$content" | grep -q "${grep_opts[@]}" -- "$pattern" 2>/dev/null; then
        printf '::error file=%s,line=%s::Forbidden phrase: %s\n' "$file" "$lineno" "$message"
        violations=$((violations + 1))
      fi
    done <<< "$added_lines"
  done
done

echo
if [[ "$violations" -gt 0 ]]; then
  echo "❌ Found $violations forbidden phrase occurrence(s) in added lines."
  echo "   Update the wording or adjust .github/forbidden-words.json if a rule is wrong."
  exit 1
fi

echo "✅ No forbidden phrases found in added lines."
