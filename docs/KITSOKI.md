# Working on gears-rust with Kitsoki

[Kitsoki](https://github.com/constructorfabric/kitsoki) runs deterministic
state-machine workflows for software work. This repo owns a single local
instance, `gears-rust-dev`, that imports Kitsoki's general `@kitsoki/dev-story`
hub and extends it with gears-rust configuration.

The boundary is deliberate:

- Kitsoki's dev-story defines the reusable workbench, PRD/design flow, bugfix
  pipeline, git/worktree helpers, and validation gates.
- gears-rust owns the project profile: command defaults, doc placement,
  gears-sdlc templates, local MCP registration, and any repo-specific story
  extensions.
- Kitsoki engine/runtime code does not know about gears-rust.

## Run

From the gears-rust repo root:

```sh
kitsoki web
```

`.kitsoki.yaml` points discovery at `./.kitsoki/stories` and sets
`.kitsoki/stories/gears-rust-dev/app.yaml` as the default story. The terminal equivalent
is:

```sh
kitsoki run .kitsoki/stories/gears-rust-dev/app.yaml
```

## Project Commands

The project profile lives at `.kitsoki/project-profile.yaml`. Current defaults:

```sh
make dev
make test
make build
make check
```

`.kitsoki/stories/gears-rust-dev/app.yaml` projects `build_cmd` and `test_cmd` into the
imported dev-story/bugfix pipeline, so validation uses the Rust workspace's
own Make/Cargo gates instead of Kitsoki's Go defaults.

## Story Shape

The local story is intentionally thin:

```yaml
imports:
  core:
    source: "@kitsoki/dev-story"
    entry: landing
```

gears-rust extends that imported hub through world defaults:

- PRDs publish to `gears/notes-service/docs/PRD.md`.
- Designs publish to `gears/notes-service/docs/DESIGN.md`.
- Design authoring reads `docs/spec-templates/gears-sdlc`.
- Feature-ticket minting is disabled for the design publish step.
- Bugfix build/test gates use `make build` and `make test`.

Change those values in `.kitsoki/stories/gears-rust-dev/app.yaml` when targeting a
different gear or when the repo's command conventions change.

## Local Tooling

Project initialization also installs the Kitsoki agent toolkit into `.agents/`
and links it into `.claude/`, plus registers the Kitsoki studio MCP in
`.mcp.json`. These are project-local integration files so Claude Code and other
MCP-capable clients can drive the same Kitsoki story surface.

Generated runtime data stays local and ignored:

- `.kitsoki.local.yaml`
- `.kitsoki/sessions/`
- `.context/`
- `.artifacts/`
- `.worktrees/`

## Validation

The shared dev-story fixtures live in the Kitsoki repo and are no-LLM. A
project-local flow fixture is not generated for this instance yet; add one under
`.kitsoki/stories/gears-rust-dev/flows/` when gears-rust needs assertions beyond the
general imported hub behavior.
