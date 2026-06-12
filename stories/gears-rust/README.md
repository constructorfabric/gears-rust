# gears-rust — dev-story targeting an external project

This instance points the **dev-story** hub (kitsoki's flagship conversation-driven
development story) at *this* repo and drives its **PRD → Design** spec chain. It
is `kitsoki-dev` with **one thing changed**: a handful of doc-profile world keys
retarget *where* docs land and *what shape* they take. No engine or dev-story
room change is needed to retarget — the seam is configuration. It is the worked
example of the external-target profile (see the
[dev-story README → doc profile](https://github.com/constructorfabric/kitsoki/blob/main/stories/dev-story/README.md#doc-profile--targeting-an-external-project)
in the kitsoki repo).

This instance is **the gears team's own**, living in this (gears) repo under
`stories/gears-rust/`. It does not copy dev-story — it imports the kitsoki base
via `import: { source: "@kitsoki/dev-story" }` (which the kitsoki binary resolves
from its **embedded** story library) and extends it with the gears doc profile.
dev-story is a dependency; this repo owns and controls the instance. So this
repo runs `kitsoki web` / `kitsoki tour` with only the `kitsoki` binary present,
no kitsoki checkout. Discovery is **zero-config**: `kitsoki web` walks the
default `./stories` dir, so this instance under `stories/gears-rust/` is found
with no `.kitsoki.yaml`. The full migration story is in the kitsoki repo's
[`kitsoki-as-dependency.md`](https://github.com/constructorfabric/kitsoki/blob/main/docs/proposals/kitsoki-as-dependency.md)
epic (slice 3).

## What it proves (the POC)

Walking, from `core.main`:

```
prd  →  author a PRD  →  prd_published  →  continue  →  design pipeline  →  publish a DESIGN
```

lands two **gears-sdlc-shaped** docs in this checkout:

```
<repo_root>/gears/<gear>/docs/PRD.md      # fixed name, gears-sdlc PRD
<repo_root>/gears/<gear>/docs/DESIGN.md   # fixed name, gears-sdlc DESIGN
```

rather than kitsoki's own flat `docs/prd/<slug>.md` + `docs/proposals/<slug>.md`.
The design author reads this repo's own **gears-sdlc templates**
([`templates/`](templates/)) and **no kitsoki feature ticket** is minted —
gears-rust tracks work in GitHub issues (the `gh` ticket adapter is a
separate, deferred epic slice; the PRD → Design walk does not pick up a
ticket, so it is not needed here).

The target gear is **`notes-service`** — a fresh scratch gear that does not
exist in the real tree, so the POC never clobbers a real gear's docs.

## Quickstart

From the repo root (this gears checkout), with only the `kitsoki` binary on
your `PATH`:

```bash
kitsoki web
# Discovers stories/gears-rust/ via the default ./stories walk (zero-config);
# open the printed URL, pick "gears-rust", and type `prd` to start the walk.
```

`workdir` / `repo_root` default to `.` (this checkout), so the PRD and DESIGN
publish under `gears/notes-service/docs/` here. To author for a different gear
or point at a checkout elsewhere, edit the warp scenario
[`scenarios/gears-rust.yaml`](scenarios/gears-rust.yaml).

## The profile (the only thing that differs from kitsoki-dev)

All set in [`app.yaml`](app.yaml)'s instance `world:` and projected into the
`core` (dev-story) import via `world_in`. Every key has a dev-story default
that reproduces kitsoki's own behaviour — overriding them **is** the profile:

| World key | gears-rust value | Effect |
|---|---|---|
| `workdir` / `repo_root` | `.` | this checkout, where docs publish |
| `publish_durable_path` | `gears/notes-service/docs` | PRD home (relative to workdir) |
| `prd_doc_filename` | `PRD` | → `gears/notes-service/docs/PRD.md` (fixed, not slug-named) |
| `design_durable_path` | `gears/notes-service/docs` | DESIGN home |
| `design_doc_filename` | `DESIGN` | → `gears/notes-service/docs/DESIGN.md` |
| `design_template_dir` | `stories/gears-rust/templates` | the gears-sdlc templates the author reads |
| `design_ticket_dir` | `""` | skip the kitsoki feature ticket |

The world keys + the publish-path/`doc_filename` parameterization live in the
hub; see the dev-story README's
[doc-profile section](https://github.com/constructorfabric/kitsoki/blob/main/stories/dev-story/README.md#doc-profile--targeting-an-external-project).

## Flows (no-LLM validation)

```bash
kitsoki test flows stories/gears-rust/app.yaml   # 2/2
```

- [`flows/prd_to_design.yaml`](flows/prd_to_design.yaml) — the PRD half:
  `main → prd → … → prd_published → continue → design`, asserting the PRD
  publishes to `gears/notes-service/docs/PRD.md` and the path seeds the design
  intake. It also asserts the profile threads instance → core → **prd**
  (`core__prd__publish_durable_path`, `core__prd__prd_doc_filename`).
- [`flows/design_publishes_gears_design.yaml`](flows/design_publishes_gears_design.yaml)
  — the DESIGN half: the seeded design intake walked to publish, asserting
  `gears/notes-service/docs/DESIGN.md` and **no** feature ticket.

They are split because each carries one acceptance shape; a third flow,
[`flows/prd_to_design_full.yaml`](flows/prd_to_design_full.yaml), walks the
**whole** PRD → Design chain in one session (the prd author and design author
are disambiguated by the `id: prd_author` / `id: design_author` task ids) and
is the host-stub source the demo video drives.

## Demo video (no-LLM, tour-driven)

A tour-narrated walkthrough of the full PRD → Design conversation — driven
entirely through the chat UI against the no-LLM flow above — renders **from the
binary**, no Node/pnpm/Playwright:

```bash
kitsoki tour --feature gears-prd-design
# → .artifacts/gears-prd-design/gears-prd-design.mp4 (+ chapters + per-step PNGs)
```

The tour steps and their declarative `drive:` actions live in the feature
catalog [`features/gears-prd-design.yaml`](features/gears-prd-design.yaml).
(The `kitsoki tour` subcommand is slice 2 of the
[`kitsoki-as-dependency.md`](https://github.com/constructorfabric/kitsoki/blob/main/docs/proposals/kitsoki-as-dependency.md)
epic.)

## Copy this dir for a NEW external target

This instance **is** the template for a second target — no code change:

1. `cp -r stories/gears-rust stories/<target>`.
2. In `app.yaml`, swap the `host_bindings` if the target's ticket / vcs / ci
   providers differ, and repoint the doc-profile world keys (`workdir`,
   `*_durable_path` = `<scope>/docs`, the fixed filenames, `design_template_dir`).
3. Vendor that target's doc templates into `templates/`.
4. Copy the two flows, adjust the asserted paths.

A laxer target (flat docs, slug-named files, kitsoki templates) needs even
less — just `workdir`.
