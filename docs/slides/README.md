# SLIDES

This folder contains the Constructor Fabric Gears overview deck as a Slidey JSON spec and a generated self-contained HTML deck.

## Build the overview deck

Install or link the Slidey CLI, then run from the repository root:

```sh
make slides
```

Equivalent direct command:

```sh
node docs/slides/diagrams/render-themed-drawio-svg.mjs
slidey bundle docs/slides/1_OVERVIEW.slidey.json docs/slides/1_OVERVIEW.html
```

The generated HTML is self-contained and can be opened directly from disk. The source deck is `docs/slides/1_OVERVIEW.slidey.json`.

## Rebuild comparison review assets

The migration review deck is kept as source plus a regeneration script, not as checked-in PNG screenshots. To recreate the legacy-vs-Slidey comparison images and HTML:

```sh
docs/slides/rebuild-comparison-assets.sh
```

That script renders `compare/source/1_OVERVIEW.old.md` with Marp, renders the Slidey deck reveal steps, and rebuilds `docs/slides/1_OVERVIEW.compare.html`. The comparison HTML is self-contained once rebuilt, but `docs/slides/1_OVERVIEW.compare.slidey.json` needs the generated `compare/old/` and `compare/new/` images if you open or bundle it again.
