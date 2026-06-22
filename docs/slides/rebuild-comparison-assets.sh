#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SLIDES_DIR="$ROOT/docs/slides"
COMPARE_DIR="$SLIDES_DIR/compare"
TMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

if command -v marp >/dev/null 2>&1; then
  MARP=(marp)
else
  MARP=(npx -y @marp-team/marp-cli)
fi

mkdir -p "$COMPARE_DIR/old" "$COMPARE_DIR/new"
mkdir -p "$COMPARE_DIR/img"
rm -f "$COMPARE_DIR"/old/slide.*.png "$COMPARE_DIR"/new/*.png "$COMPARE_DIR"/old.[0-9]* "$COMPARE_DIR"/img/*.png

echo "==> Staging legacy Marp image inputs"
cp "$ROOT/docs/img/gear_architecture.drawio.png" "$COMPARE_DIR/img/gear_architecture.drawio.png"
cp "$ROOT/docs/img/architecture.drawio.png" "$COMPARE_DIR/img/architecture.drawio.png"
cp "$ROOT/docs/img/gears_categories.drawio.png" "$COMPARE_DIR/img/gears_categories.drawio.png"
cp "$ROOT/docs/img/request_sequence.png" "$COMPARE_DIR/img/request_sequence.png"

echo "==> Rendering Slidey diagram SVG inputs"
node "$SLIDES_DIR/diagrams/render-themed-drawio-svg.mjs"

echo "==> Rendering legacy Marp slides"
"${MARP[@]}" \
  "$COMPARE_DIR/source/1_OVERVIEW.old.md" \
  --theme-set "$COMPARE_DIR/source/slides.old.css" \
  --allow-local-files \
  --images png \
  -o "$TMP_DIR/slide.png"

i=1
while IFS= read -r file; do
  cp "$file" "$COMPARE_DIR/old/slide.$(printf '%03d' "$i").png"
  i=$((i + 1))
done < <(find "$TMP_DIR" -type f -name '*.png' | sort)

echo "==> Rendering migrated Slidey reveal steps"
slidey "$SLIDES_DIR/1_OVERVIEW.slidey.json" "$COMPARE_DIR/new"

echo "==> Rebuilding self-contained comparison deck"
slidey bundle "$SLIDES_DIR/1_OVERVIEW.compare.slidey.json" "$SLIDES_DIR/1_OVERVIEW.compare.html"

echo "==> Writing audit reports"
slidey "$SLIDES_DIR/1_OVERVIEW.slidey.json" --audit "$SLIDES_DIR/1_OVERVIEW.audit.json"
slidey "$SLIDES_DIR/1_OVERVIEW.compare.slidey.json" --audit "$COMPARE_DIR/audit.json"

echo "Done."
