# Authoring gears-sdlc specs with kitsoki

[**kitsoki**](https://github.com/constructorfabric/kitsoki) is a deterministic
state-machine runtime for LLM workflows. One of its stories — **`gears-rust`**
— drives *this* repo's
**PRD → Design** spec chain as a guided, conversational walk: you describe an
idea in a chat UI, answer a few clarifying questions, and it authors two
[gears-sdlc](spec-templates/gears-sdlc/)-shaped documents straight into the
target gear's docs folder.

It is the worked example of kitsoki's *external-target profile*: the same
hub kitsoki uses to build itself, pointed at this repo purely through
configuration (where docs land, what shape they take). No kitsoki engine code
is specific to gears-rust.

> This is a **spec-authoring aid**, not a code generator. It produces PRD.md
> and DESIGN.md you review and edit — it never touches `src/`, `Cargo.toml`,
> or any gear implementation.

## What it produces

A full walk lands two fixed-name docs in the target gear, in the gears-sdlc
shape (the [`DESIGN`](spec-templates/gears-sdlc/DESIGN/) /
[`PRD`](spec-templates/gears-sdlc/PRD/) templates, with `cpt-…` IDs):

```
gears/<gear>/docs/PRD.md      # gears-sdlc PRD
gears/<gear>/docs/DESIGN.md   # gears-sdlc DESIGN
```

The default target gear is **`notes-service`** — a scratch gear used so the
demo never clobbers a real gear's docs. No GitHub issue / feature ticket is
filed (this repo tracks work in GitHub issues; that integration is a separate,
deferred piece — see [Improving the story](#improving-the-story)).

## Prerequisites

1. **A kitsoki checkout, as a sibling of this repo.** The story's default
   `workdir` is `../gears-rust`, so kitsoki expects this repo one level up
   from itself:

   ```
   code/
   ├── Kitsoki/        # the kitsoki checkout (clone it here)
   └── gears-rust/     # this repo
   ```

2. **Go toolchain** (to build the `kitsoki` binary) and **Python 3** (the
   publish glue). Node/pnpm only if you want the web UI's bundled assets
   rebuilt.

3. **An LLM, or a recorded cassette.** A live run dispatches `claude` agents
   to author the docs (cost + latency). The no-LLM flows and the demo run
   entirely against recorded fixtures — see [No-LLM validation](#no-llm-validation).

## Run it

All commands run **from the kitsoki checkout** (`cd ../Kitsoki`).

Build once:

```bash
make build && cp ./kitsoki bin/kitsoki
```

### Web UI (recommended)

```bash
# one line — a newline after --stories-dir splits the command and fails
./kitsoki web --stories-dir stories/gears-rust --addr 127.0.0.1:7780
```

Open `http://127.0.0.1:7780`, then drive the walk in the chat box.

### Terminal UI

```bash
./kitsoki run stories/gears-rust/app.yaml
```

### The walk

From the landing room (`core.main`):

```
prd                         # start the PRD pipeline (discovery chat opens)
<describe your gear idea>   # converse — the analyst reads this repo for context
prd__start                  # distil the idea → prior-art scan
prd__confirm                # no overlap → clarifying questions
<answer the questions>      # the last answer auto-advances
prd__confirm                # brief → reference curation
prd__confirm                # → draft the PRD
prd__accept                 # publish gears/<gear>/docs/PRD.md
continue                    # → design intake, seeded from the published PRD
<refine / accept>           # search → brief → draft → publish DESIGN.md
```

Each authoring step runs an agent **with this repo as its working directory**,
so it reads the real gear code, `docs/`, and `guidelines/` to ground the spec
— and writes the result through the vendored gears-sdlc template.

## Targeting a different gear or checkout

The target gear and checkout path are configuration, overridable per run via a
*warp scenario* without editing the story:

```bash
./kitsoki run stories/gears-rust/app.yaml --warp stories/gears-rust/scenarios/gears-rust.yaml
```

Edit that scenario (or the `world:` defaults in
`stories/gears-rust/app.yaml`) to change:

| Key | Default | Meaning |
|---|---|---|
| `workdir` / `repo_root` | `../gears-rust` | this repo's checkout path |
| `publish_durable_path` | `gears/notes-service/docs` | PRD home (relative to `workdir`) |
| `design_durable_path` | `gears/notes-service/docs` | DESIGN home |
| `prd_doc_filename` / `design_doc_filename` | `PRD` / `DESIGN` | fixed output filenames |
| `design_template_dir` | `stories/gears-rust/templates` | the gears-sdlc templates the author reads |
| `design_ticket_dir` | `""` | empty ⇒ skip filing a tracking ticket |

To author for, say, `chat-engine` instead of `notes-service`, set both
`*_durable_path` to `gears/chat-engine/docs`.

## No-LLM validation

The full walk is pinned by deterministic fixtures (recorded agent outputs, no
LLM call, no cost):

```bash
cd ../Kitsoki
./kitsoki test flows stories/gears-rust/app.yaml
```

These assert the resolved publish paths (`gears/notes-service/docs/PRD.md`,
`…/DESIGN.md`) and that no ticket is filed. There is also a tour-narrated demo
video driven through the chat UI against the same fixtures — see
[`stories/gears-rust/README.md`](https://github.com/constructorfabric/kitsoki/blob/main/stories/gears-rust/README.md)
in the kitsoki checkout.

## How the retargeting works

The story is `kitsoki-dev` (kitsoki building kitsoki) with one thing changed:
a handful of *doc-profile* world keys are set at the instance level and
projected into the shared `dev-story` hub. Every key has a default that
reproduces kitsoki's own behaviour; overriding them **is** the profile. The
two publish scripts (`prd_publish.py`, `publish_design.py`) take the durable
path / filename / ticket-dir as parameters, so the placement seam needs no
code change. Full detail lives in the [kitsoki
repo](https://github.com/constructorfabric/kitsoki):

- [`stories/gears-rust/README.md`](https://github.com/constructorfabric/kitsoki/blob/main/stories/gears-rust/README.md)
  — this instance, end to end.
- [`stories/dev-story/README.md`](https://github.com/constructorfabric/kitsoki/blob/main/stories/dev-story/README.md)
  → "Doc profile — targeting an external project" — the seam and every world key.

## Improving the story

The PRD → Design chain is the shipped slice. Natural next steps, roughly in
priority order:

1. **GitHub-issue ticket adapter (the big one).** Today the walk files no
   tracking ticket (`design_ticket_dir: ""`). A `gh`-backed adapter satisfying
   kitsoki's abstract `ticket` interface would, on publish, open/label a
   GitHub issue linking back to the DESIGN — closing the loop into this repo's
   actual workflow. This is glue (a script against `gh`), not a new runtime
   primitive; it is the one deferred slice of the external-targeting epic in
   the kitsoki repo. Once present, set `design_ticket_dir` / bind the adapter
   in `stories/gears-rust/app.yaml`'s `host_bindings`.

2. **Extend the chain to later gears-sdlc stages.** This repo's SDLC is
   PRD → DESIGN → **ADR → FEATURE → DECOMPOSITION** (templates and checklists
   already vendored under `docs/spec-templates/gears-sdlc/` and
   `docs/checklists/`). The story stops at DESIGN. Adding rooms that author
   ADRs (per architecturally-significant decision) and decompose a DESIGN into
   FEATUREs would mirror the existing design pipeline — each is another
   `intake → draft → publish` triple with its own template dir and fixed
   filename. The vendored templates in the story's `templates/ADR` and
   `templates/FEATURE` are already in place for this.

3. **Conventional-commit + DCO commit discipline.** A live run authors files
   but does not commit. Binding the `vcs` interface to a glue script that
   stages the new docs and commits them with a DCO sign-off + conventional
   message (`docs(<gear>): add PRD/DESIGN`) would make the walk land a
   review-ready branch, matching `CONTRIBUTING.md`.

4. **`make check` as the `ci` gate.** The hub declares a `ci` interface; wiring
   it to this repo's `make check` (fmt / clippy / lychee link-check on the new
   markdown) would let the story self-verify its output before handing back.

5. **Per-gear template / placement presets.** Targeting a different gear today
   means editing two `*_durable_path` keys. A small "pick a gear" room reading
   `gears/*/` could turn that into a menu, removing the manual path edit.

6. **Sharpen the spec prompts for gears vocabulary.** The author prompts are
   written to adapt to whichever template set they're handed; they could be
   tightened to actively use gears terms (GTS types, extension points, plugin
   vs adapter interfaces, the p1–p5 phase tags from `docs/GEARS.md`) so first
   drafts need less editing.

To make a *second* external target (another GitHub repo), copy
`stories/gears-rust/` in the kitsoki checkout, repoint the world keys, vendor
that target's templates, and adjust the two flows — no kitsoki code change.
The procedure is in `stories/gears-rust/README.md` → "Copy this dir for a NEW
external target".

## Troubleshooting

- **`flag needs an argument: --stories-dir` / `permission denied: stories/…`**
  — your shell split the command across a newline. Keep `--stories-dir
  <dir>` on one line (or end the line with `\`).

- **The spec talks about *kitsoki* instead of this repo.** The authoring agent
  must run with this repo as its working directory and the gears-sdlc
  templates as its template dir. Both are set by the profile; if you see
  kitsoki content, confirm you launched with `--stories-dir stories/gears-rust`
  (not the bare `kitsoki-dev` instance) and that `../gears-rust` resolves to
  this checkout. This requires a recent kitsoki build — earlier builds had a
  boot-time projection bug where the profile keys did not reach the hub.

- **Docs land in kitsoki's own `docs/proposals/` instead of here.** Same cause
  — the profile keys aren't resolving; check `workdir` / `*_durable_path` in
  `stories/gears-rust/app.yaml` and rebuild the binary.
