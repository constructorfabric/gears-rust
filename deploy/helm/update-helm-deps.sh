#!/usr/bin/env bash
set -euo pipefail

helm_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$helm_root/../.." && pwd)"
platform_chart="$helm_root/toolkit-platform"

# Auto-discover every chart that depends on the toolkit-common library and resolve
# it, then resolve the umbrella last (its subcharts must be built first). New gear
# charts are picked up automatically.
while IFS= read -r chart_yaml; do
  helm dependency update "$(dirname "$chart_yaml")"
done < <(grep -rlE --include=Chart.yaml '^[[:space:]]*-[[:space:]]*name:[[:space:]]*toolkit-common' \
  "$repo_root/apps" "$repo_root/examples" "$repo_root/gears" | grep -v '/charts/')

helm dependency update "$platform_chart"