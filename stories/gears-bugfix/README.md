# Fixing a gears-rust bug with kitsoki

[**kitsoki**](https://github.com/constructorfabric/kitsoki) is a deterministic
state-machine runtime for LLM workflows. This story — **`gears-bugfix`** —
points its **autonomous bug-fix pipeline** at *this* repo: hand it a gears-rust
bug and it reproduces, fixes, tests, reviews and validates the change, landing
it on a feature branch ready for a PR.

It is the bug-fix companion to [`stories/gears-rust/`](../gears-rust/README.md)
(PRD → Design): same idea — **kitsoki as a dependency, pointed at this repo via
config** — a different stage of the SDLC. The story imports the kitsoki base via
`source: "@kitsoki/bugfix"` (resolved from the kitsoki binary's embedded story
library — no kitsoki checkout needed) and binds it to this checkout + a ticket.

## The pipeline

```
reproduce → propose → implement → test → review → validate → done → (PR-ready)
```

Each room is a deterministic state-machine beat; each operator **accept** advances
exactly one room. The agent runs with **this checkout as its working directory**,
in an isolated `git worktree`, so it reads and edits real gear code; CI runs the
project's real **`cargo test`** (the story sets `test_cmd` / `build_cmd` to this
repo's `--features bootstrap` invocations in place of kitsoki's Go defaults).

> This **edits code and lands a branch** — it is not a spec-authoring aid. The
> default ticket is gears-rust issue **#4115** (gear config can't be overridden
> via env vars in Kubernetes — a dashed gear name vs the k8s C_IDENTIFIER rule).

## Watch the demo (no LLM, no cost)

A tour-narrated walk of the full pipeline against #4115, replayed **verbatim from
a real run** — deterministic, free, same frames every time:

```bash
cd stories/gears-bugfix
kitsoki tour --feature gears-bugfix
# → .artifacts/gears-bugfix/gears-bugfix.mp4 (+ chapters + per-step PNGs)
```

The content (the reproduction, the proposed fix, the applied diff, the 316/0
cargo log, the validation) is replayed from `flows/tour.yaml` +
`cassettes/tour.cassette.yaml`. Pin it as a no-LLM regression with:

```bash
kitsoki test flows stories/gears-bugfix/app.yaml
```

## Run it for real (dispatches LLM agents)

```bash
cd stories/gears-bugfix
kitsoki run app.yaml          # drive it in the TUI
# or: kitsoki web             # drive it in the browser
```

A live run dispatches `claude` agents to do the actual reproducing / fixing /
test-writing against this checkout (cost + latency). Drive it by `start`, then
review and `accept` (or `refine`) at each checkpoint.

## Fix a different bug

Point the story at another ticket without touching kitsoki — edit `app.yaml`'s
`world:` defaults (or override per run via a warp scenario):

| Key | Default | Meaning |
|---|---|---|
| `ticket_id` / `ticket_title` / `ticket_url` | `gh-4115` … | the bug to fix |
| `workdir` | `.worktrees/bf-gh-4115` | isolated worktree under this checkout |
| `base_branch` / `feature_branch` | `main` / `fix/gh-4115` | branch the fix off / land it on |
| `test_cmd` / `build_cmd` | `cargo … --features bootstrap` | this repo's CI commands |

To target a different gear/crate, set `test_cmd` / `build_cmd` to that crate's
`cargo test -p … / cargo build -p …` invocation. No kitsoki code change.

## How the retargeting works

The `bugfix` story is provider-neutral: it declares abstract `ticket` / `vcs` /
`ci` / `workspace` / `transport` interfaces and forwards `world.test_cmd` /
`world.build_cmd` to its local CI handler. This instance binds those interfaces
to git / git-worktree / local-CI and supplies the cargo commands — the seam is
config. Full detail lives in the
[kitsoki repo](https://github.com/constructorfabric/kitsoki):
`stories/bugfix/README.md` (the pipeline) and `docs/KITSOKI.md` here (the
embedded-base, bare-binary model).
