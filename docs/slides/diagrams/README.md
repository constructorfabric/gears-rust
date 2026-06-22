# Slide diagrams

This directory keeps the editable diagram sources used by the Slidey overview deck.

## Source of truth

The original Draw.io PNGs embed their `mxfile` source. The extracted reference files live in `reference/` and preserve the original coordinates, labels, colors, and grouping:

- `reference/gear_architecture.drawio.xml`
- `reference/planned_gears_map.drawio.xml`
- `reference/gear_categories.drawio.xml`

The themed SVG files used by `docs/slides/1_OVERVIEW.slidey.json` are generated from those references:

```sh
node docs/slides/diagrams/render-themed-drawio-svg.mjs
```

The wrapper calls the reusable Slidey command:

```sh
slidey drawio docs/slides/diagrams/reference/gear_architecture.drawio.xml --out-dir docs/slides/diagrams/themed-svg --label "Gear architecture"
```

To re-extract source XML from an embedded Draw.io PNG and generate a themed SVG in one step:

```sh
slidey drawio docs/img/gear_architecture.drawio.png --extract-dir docs/slides/diagrams/reference --out-dir docs/slides/diagrams/themed-svg
```

## Mermaid sketches

- `gear_architecture.mmd` reconstructs `docs/img/gear_architecture.drawio.png`.
- `planned_gears_map.mmd` reconstructs `docs/img/architecture.drawio.png`.
- `gear_categories.mmd` reconstructs `docs/img/gears_categories.drawio.png`.
- `request_lifecycle.mmd` reconstructs `docs/img/request_sequence.png`.

These are non-authoritative sketches. Use the Draw.io XML references when correcting layout drift because Mermaid does not support arbitrary fixed XY coordinates for these flowcharts.
