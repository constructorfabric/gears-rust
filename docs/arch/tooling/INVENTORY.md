# Repository Inventory — what `gears-rust` holds, and how each part can become a reference

Companion to [VISION.md](VISION.md). It applies section 6 ("versioned references, not copies") to the actual
contents of this repository, item by item, and reports what the residue turns out to be.

## Table of Contents

- [How to read this](#how-to-read-this)
- [A. Build system](#a-build-system)
- [B. CI](#b-ci)
- [C. Rust toolchain and lint configuration](#c-rust-toolchain-and-lint-configuration)
- [D. SDLC and Studio](#d-sdlc-and-studio)
- [E. AI agent configuration](#e-ai-agent-configuration)
- [F. Guidelines and platform docs](#f-guidelines-and-platform-docs)
- [G. Organizational boilerplate](#g-organizational-boilerplate)
- [H. Product content, out of scope](#h-product-content-out-of-scope)
- [Findings](#findings)

---

## How to read this

Each item gets the highest order from VISION.md section 6 that it can reach:

| Order | Meaning |
|---|---|
| **R** | Reference — a versioned external artifact plus a pin here. |
| **G** | Generate — produced from a manifest or a source of truth, reproducibly, never hand-edited. |
| **C** | Copy — the residue; replaced as a whole file on upgrade, never merged. |
| **—** | Out of scope; product content, not a convention. |

"Today" records what the repository does now, so the delta is visible.

---

## A. Build system

| Item | Size | Today | Target | Mechanism / blocker |
|---|---|---|---|---|
| `Makefile` | 12 lines | copied | **C** | The bootstrap that makes R possible for everything else. Declares `.DEFAULT_GOAL`, includes the project file, then the shared one. Stable; a genuine three-line stub. |
| `.gears-dna/gears.mk` | 37 KB, v1.1.0, 32 targets | vendored by hand; its own header forbids editing the copy | **R** | Already behaves like a reference — it has a version, a declared source and a no-edit rule — but resolution is a human running a copy. Needs a pinned resolution step. A git submodule would do it (precedent: `guidelines/DNA`), as would a fetch-by-version into a cache directory with a checksum. `include` requires the file to exist before make parses it, so the bootstrap rule is the real design work. |
| `gears-rust.mk` | 64 KB, 74 targets | owned | **C** (owned) | Settings for the shared file plus this project's own targets — per-project by design. But it currently mixes genuinely project-specific targets with ones that belong upstream in `gears.mk`; the split is a prerequisite, not a consequence. |

## B. CI

| Item | Count | Today | Target | Mechanism / blocker |
|---|---|---|---|---|
| `.github/workflows/*.yml` | 30 | copied | **R** | Bodies move to a versioned repository, each project keeps a thin caller pinned by tag. Three files here already declare `workflow_call` (`cfs.yml`, `publish-artifacts.yml`, `test-suite.yml`), so the pattern is established in-repo, not hypothetical. Residue per workflow: the `on:` triggers, path filters and secret wiring — a handful of lines each. |
| `.github/actions/*` | 4 (`cargo-gears`, `nightly-issue`, `sccache`, `sccache-stats`) | copied | **R** | Composite actions are referable cross-repository as `owner/repo/path@tag` with no copy at all. The cheapest win in this document. |
| `tools/scripts/*.py` | 24 (`ci.py`, `coverage.py`, `junit_enrich.py`, `run_make_matrix.py`, `rustc_annotate.py`, `validate_gear_names.py`, `resolve_release.py`, …) | copied | **R** | This is the pipeline's real logic, invisible in the workflow files. Published as a pinned package or folded into `cargo gears` / `xtask` as subcommands. Leaving it in order 3 would make every other CI gain cosmetic. |
| `.github/scripts/check-forbidden-words.sh` | 1 | copied | **R** | Script is shared logic. |
| `.github/forbidden-words.json` | 1 | copied | **C** | The word list is project policy, not shared logic. |
| `.clusterfuzzlite/` | 3 | copied | **R** + small **C** | `build.sh` is shared; `project.yaml` names the project. |
| `.github/dependabot.yml`, `CODEOWNERS`, `pull_request_template.md`, `copilot-instructions.md` | 4 | copied | **C** | Name teams, owners and per-project policy. Template once, owned after. |
| `.codecov.yml`, `.codacy.yml`, `.prs/config.yaml` | 3 | copied | **C** | Third-party services with no external configuration inheritance. Small and stable. |

## C. Rust toolchain and lint configuration

This is the section that justifies the tool existing. Rust has no `extends`, no parent manifest and
no remote configuration inheritance, so order 1 is unavailable for almost everything here.

| Item | Today | Target | Mechanism / blocker |
|---|---|---|---|
| `clippy.toml` | copied | **C** | No inheritance mechanism in clippy. |
| `rustfmt.toml` | copied | **C** | No inheritance mechanism in rustfmt. |
| `deny.toml`, `deny-fips.toml` | copied | **C** | `cargo-deny` reads a local file; no external source. |
| `rust-toolchain.toml` | copied | **C** | Pin only, no inheritance. |
| `.config/nextest.toml` | copied | **C** | Profiles are local; no external source. |
| `.cargo/config.toml` | copied | **C** | Cargo merges config up the *directory* tree, which stops at the repository. The closest thing to inheritance Rust offers, and it does not cross a repository boundary. |
| `dylint.toml` + the dylint rule set | rules pulled as a crate | **R — already works** | The exception that proves the rule. The lints themselves live in `cargo-gears-lints`, a versioned crate resolved by `cargo gears lint`; only the per-project skip list in `Gears.toml` is owned. Order 1 works here **because the rules are code in a crate, not configuration in a file.** |
| `Cargo.toml` → `[workspace.lints.rust]`, `[workspace.lints.clippy]` | ~125 lint settings, copied | **G** | Inheritable within a workspace, which ends at the repository. Generatable from a shared source and checkable. |
| `Cargo.toml` → `[workspace.dependencies]` | 256 entries, copied | **G** | Cross-repository version alignment has no Cargo mechanism at all — this is the Maven BOM that Rust does not have. The largest unsolved order-1 gap after lint config. |
| `Cargo.toml` → `[profile.*]`, `[workspace.package]` | copied | **C** | Small, stable. |
| `.editorconfig`, `.gitattributes`, `.dockerignore` | copied | **C** | Small, stable. |
| `.gitignore` | 593 lines | **G** | An aggregate of per-tool sections. The textbook case for generation from a declared tool list. |
| `Cargo.lock` | — | **—** | The project's own. |

## D. SDLC and Studio

| Item | Today | Target | Mechanism / blocker |
|---|---|---|---|
| `studio-kit-gears/` + `.cf-studio-kit.toml` + `.cf-studio/` | kit with version, lockfile and install mode | **R — already done** | The working model the rest of this document is trying to reach. Note the local caveat: `gears-rust` registers its own kit in place (`install_mode = "register"`, `path = ".."`), so it is the *publisher*; a downstream project consumes it by version. |
| `.cf-studio/config/artifacts.toml` | Studio-owned, user-editable, ~700 lines | **G** | A map of this repository's systems and their artifact roots. Studio's skills own it; the `[[ignore]]` entries are the part a person adds, one suppression at a time. |
| `.cf-studio/config/rules/*.md` | 8 files, via kit | **R** | Already kit-delivered. |
| `docs/spec-templates/`, `docs/checklists/`, `docs/pr-review/` | stubs + READMEs | **R — already done** | Contents moved into the kit by PR #4930; what remains here is pointers. |

## E. AI agent configuration

| Item | Today | Target | Mechanism / blocker |
|---|---|---|---|
| `AGENTS.md`, `CLAUDE.md` | mixed: an `@cf:root-agents` marked block plus hand-written prose | **G** (block) + **C** (prose) | Managed regions with explicit markers already exist in this repository. The precedent for mixed ownership in one file is set and works. |
| `.claude/` (settings, 7 agents, 3 skills), `.codex/prompts/`, `.cursor/commands/`, `.devin/workflows/`, `.windsurf/workflows/` | Studio manages its own agent files; everything else here is hand-kept | **G** | The non-Studio part is the problem. The `toolkit-pr-review` prompt exists five times, once per agent vendor, and four of the copies have already diverged. One source, five renderings, generated. This is order 2 by construction — those files have no business being authored. |

## F. Guidelines and platform docs

| Item | Today | Target | Mechanism / blocker |
|---|---|---|---|
| `guidelines/DNA` | **git submodule** | **R — already done** | The in-repo precedent for order 1. Caveat: it is not fetched by default, and `cfs` crashes on an uninitialised submodule — PR #5326 adds `ensure-dna.py` to fail fast. A reference mechanism needs a bootstrap check, or it is worse than a copy. |
| `guidelines/*.md` (`DEPENDENCIES`, `DESIGN_REVIEW`, `GTS`, `SECURITY`, `README`) | copied | **R** | Platform-wide content with nothing project-specific in it. |
| `docs/gdl.md` | 18-line pointer to `constructorfabric/gearbox` | **R — already done** | The purest existing example of the principle: the document that would have been copied was replaced by a reference, with the reasons stated in the file itself. |
| `docs/GEARBOX.md` | explanatory, this repo's corpus | **C/—** | Explains what `gear.gdl` files are for; partly platform-wide, partly about this corpus. |

## G. Organizational boilerplate

| Item | Size | Target | Mechanism / blocker |
|---|---|---|---|
| `CONTRIBUTING.md` | 22 KB | **R** + **C** preamble | Mostly platform-wide process; the project-specific part is small. |
| `LICENSE`, `NOTICE` | — | **C** | Per-project by law, written once. |
| `README.md`, `SETUP.md`, `MAINTAINERS.md`, `SECURITY.md` | — | **C** | Templated once, owned after. |
| `CHANGELOG.md` | 437 KB | **G** | Produced by `release-plz`; already generated. |
| `release-plz.toml`, `lychee.toml` | — | **C** | Tool configuration with no external inheritance. |

## H. Product content, out of scope

`gears/` (20), `libs/` (37), `apps/` (2), `examples/` (3), `testing/`, `deploy/`, `proto/`, `config/`
(21 runtime configs), and `docs/{arch,adrs,api,toolkit_unified_system,web-docs,security,slides,img}`.

The CLI has no claim on any of it. Its only interest is **layout** — that a gear sits where
gears sit and carries the documents a gear carries — and layout is a checkable rule, not a file to
install. It belongs with `check`, not with `init`.

---

## Findings

**1. The principle is already in use here, three times, and nobody generalised it.** The `guidelines/DNA`
submodule, the Studio kit with its version and lockfile, and `docs/gdl.md` as an eighteen-line
pointer to the upstream reference are all order 1, arrived at independently. Alongside them,
`gears.mk` carries a version, a declared source and a no-edit rule — everything a reference needs
except the resolution step. The work is less invention than finishing something already started.

**2. Order 1 works in Rust exactly where the rule is code rather than configuration.** The dylint
rule set is referenced, versioned and upgraded by bumping a crate, while `clippy.toml` beside it must
be copied. The difference is not importance, it is form: a crate has a version and a resolver, a TOML
file has neither. This points at a strategy for shrinking the residue further — anything expressible
as a check in a crate escapes order 3 — and at its limit, since formatting and dependency-audit
policy are not expressible that way.

**3. CI is the largest win and is already half-started.** Thirty workflows, four composite actions and
twenty-four Python scripts are all order 1 with no Rust obstacle whatsoever, and three workflows here
already declare `workflow_call`. The composite actions need no copy at all. The hidden item is
`tools/scripts/` — moving the workflow files while leaving the scripts behind would make the whole
exercise cosmetic.

**4. The residue is about twenty small files, and that is the real result.** `clippy.toml`,
`rustfmt.toml`, `deny.toml`, `deny-fips.toml`, `rust-toolchain.toml`, `.config/nextest.toml`,
`.cargo/config.toml`, `.editorconfig`, `.gitattributes`, `.dockerignore`, the `Makefile` driver,
`CODEOWNERS`, `dependabot.yml`, the PR template, three service configs, and the legal and readme
files. All small, all stable, none structured enough to be worth merging. Whole-file replacement
under a version check is sufficient for every one of them, which is what makes the "no merge engine"
non-goal in VISION.md section 8 possible, not only desirable.

**5. Two gaps are big enough to be their own work.** `[workspace.dependencies]` holds 256 entries with
no cross-repository alignment mechanism in Cargo — the BOM that Rust lacks — and `[workspace.lints]`
holds roughly 125 settings inheritable only within a workspace. Both are generation problems, both
need a source of truth that does not exist yet, and neither is solved by any scaffolding tool on the
market.

**6. A reference without a bootstrap check is worse than a copy.** `guidelines/DNA` is the warning:
the submodule is a correct order-1 mechanism, and an uninitialised one crashes `cfs` at a point far
from the cause, which is why PR #5326 has to add a fail-fast guard. Every reference introduced here
needs its missing-resolution failure designed at the same time, or the principle trades visible
staleness for invisible breakage.
