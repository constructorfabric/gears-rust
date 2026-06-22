# SLIDES

This folder contains the Constructor Fabric Gears overview deck as a Slidey JSON spec and a generated self-contained HTML deck.

## Build the overview deck

Install or link the Slidey CLI, then run from the repository root:

```sh
make slides
```

Equivalent direct command:

```sh
slidey bundle docs/slides/1_OVERVIEW.slidey.json docs/slides/1_OVERVIEW.html
```

The generated HTML is self-contained and can be opened directly from disk. The source deck is `docs/slides/1_OVERVIEW.slidey.json`.
