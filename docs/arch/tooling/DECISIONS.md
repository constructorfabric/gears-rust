<!-- Related: ./VISION.md, ./INVENTORY.md -->

# Gears CLI — Decision Register

[VISION.md](VISION.md) gives the direction and the limits of the scope. It does not make design
decisions. This register holds the decisions that `DESIGN.md` and the ADRs after it must make.

Each row uses the terms of [VISION.md section 2](VISION.md#2-terms).

## Table of Contents

- [Status vocabulary](#status-vocabulary)
- [Open decisions](#open-decisions)
- [Decisions owned elsewhere](#decisions-owned-elsewhere)
- [Detail](#detail)

## Status vocabulary

| Status | Meaning |
|---|---|
| open | Nobody has made this decision. `DESIGN.md` or an ADR must make it. |
| deferred | Not needed now. The row names the event that reopens it. |
| elsewhere | Another team owns this decision. The row states how it affects this tool. |
| decided | Made. The row names the document that records it. |

## Open decisions

| ID | Area | Decision to make | Evidence | Impact |
|---|---|---|---|---|
| C-01 | Upstream location and form | Where the referenceable conventions live, and in which form a repository pins them | `gears.mk` header; `guidelines/DNA` submodule; `constructorfabric/DNA` repository | **BLOCKER** |
| C-02 | Copied files | How the tool records the installed version of a copied file, detects a local edit, and handles a legitimate local change on upgrade | VISION section 6, option 3; INVENTORY section C | **BLOCKER** |
| C-03 | Convention menu | What a maintainer selects, how large one unit is, which units depend on others, and whether a recommended baseline exists | VISION section 5, conformance validation | **HIGH** |
| C-04 | Extension mechanism | How a project declares an extension and pins its version, and what an extension may contribute | VISION section 5.1 | **HIGH** |
| C-05 | Tool boundary | One binary, or `gears` as a wrapper over `cargo gears`, `cfs` and Gearbox | VISION section 5.1; three repositories, three release cycles | **MEDIUM** |
| C-06 | Facades on PR #5326 | Whether the kit facades and this vision combine, or one replaces the other | PR #5326 (open); `scripts/ci-commands.toml`, `scripts/project-paths.toml`, `cf-gears-init` | **HIGH**, time-critical |

| ID | Area | Decision to make | Evidence | Status |
|---|---|---|---|---|
| C-07 | CI portability | The reduced CI baseline for a project outside this organization | 30 workflows; self-hosted runners; Codacy, Codecov, ClusterFuzzLite | deferred — research task, reopens when C-01 and C-03 need a scope |
| C-08 | AI layer | Which tool triggers the generation of agent configuration | VISION section 7, "Aggregate. Do not re-implement."; `cfs` owns the prompts | deferred — reopens at first release of the setup command |

## Decisions owned elsewhere

| ID | Area | Decision | Owner | Effect on this tool |
|---|---|---|---|---|
| C-09 | Two application generators | Whether `cargo gears` (from `Gears.toml`) and Gearbox (from `gear.gdl` with `product.gdl`) converge, divide by scope, or one is withdrawn | `cargo-gears` and `gearbox` maintainers | The tool writes the manifests that the selected conventions require. The answer changes which manifests appear, not how setup works. |
| C-10 | Descriptor backfill | Whether any tool may draft the declared part of a `gear.gdl` for a gear that already exists | `gearbox` maintainers | Gearbox refuses this today. The tool reports a missing descriptor. It does not write one. |

---

## Detail

### C-01 — Upstream location and form

- **Current state**: `.gears-dna/gears.mk` names `gears-rust` as its source and forbids edits to a
  copy. A person copies it by hand. `guidelines/DNA` is a git submodule and points at
  `constructorfabric/DNA`, a public repository that holds "Development Norms & Architecture". Three
  workflows in `.github/workflows` already declare `workflow_call`.
- **Problem**: VISION section 6 has no effect until a pin can select a convention. No such place
  exists for the build rules or for CI.
- **Options**: (a) a new versioned repository that publishes `gears.mk` under tags and the CI
  pipeline as reusable workflows; (b) the same content in `constructorfabric/DNA`, which already
  holds the human-readable conventions; (c) a pinned fetch into a cache directory with a checksum.
- **Recommendation**: (a) or (b) over (c), and a git submodule over a fetch. Dependabot covers a
  submodule through its `gitsubmodule` ecosystem and covers a `uses:` reference through its
  `github-actions` ecosystem. It covers a bare version string in a file through nothing. Prefer the
  form that an existing updater already understands.
- **Note**: `constructorfabric/DNA` and `.gears-dna/` are two different things today. The first is a
  repository of human-readable rules. The second is machine-readable make rules published from
  `gears-rust`. Option (b) merges them, which is a separate decision.

### C-02 — Copied files

- **Current state**: `gears-rust` holds about twenty small files that Rust gives no alternative for.
  INVENTORY section C lists them. Nothing records which version a repository installed.
- **Problem**: the update loop replaces a copied file as a whole file under a version check. Both the
  version and the check need a mechanism.
- **Questions**: Where does the tool record the installed version? How does it detect that a person
  edited a managed file? What does the tool do on upgrade when a project changed the file for a good
  reason?
- **Constraint**: VISION section 8 excludes a three-way merge. A project that must keep a local
  change therefore needs a different answer, such as an explicit opt-out of that managed file.
- **Related**: Projen marks a generated file with a header and makes it read-only. `AGENTS.md` in
  this repository already uses an `@cf:root-agents` marker for a managed region.

### C-03 — Convention menu

- **Current state**: VISION section 5 states that conventions are a menu and that the tool judges a
  repository against its declaration only. The units of that menu do not exist yet.
- **Questions**: What is one unit — "document-first development", or "a PRD exists for each gear"?
  Which units depend on other units? Does a recommended baseline exist, and does the tool report a
  project as below that baseline without a failure?
- **Risk**: units that are too large make the menu unusable. Units that are too small make the report
  unreadable.

### C-04 — Extension mechanism

- **Current state**: VISION section 5.1 requires the tool to be open along four directions, and
  leaves the seams to this decision: the commands, the checks, the menu of conventions, and the
  places a reference resolves from. A renderer that changes how a report is displayed is a candidate
  fifth seam; the content and the schema of a report are not extensible.
- **Constraint**: an extension is a declared and pinned reference. The tool must not find an
  extension in the environment, because the bot contract depends on deterministic behaviour.
- **Questions**: How does a project declare an extension? How does the tool resolve and pin it? What
  may an extension contribute, and what stays in the core?

### C-05 — Tool boundary

- **Current state**: the commands that fit this vision are in three tools, in three repositories,
  with different release cycles and different owners.
- **Note**: VISION section 5.1 reduces the cost of this decision. A tool that can take a capability
  as an extension lets the boundary move later. The decision is therefore not a blocker.

### C-06 — Facades on PR #5326

- **Current state**: PR #5326 is open against `main`. It adds `scripts/ci-commands.toml` and
  `scripts/project-paths.toml`, with a `cf-gears-init` workflow that discovers the build commands and
  paths of a project. The kit then adapts to the project.
- **Problem**: this is the opposite direction. The facades adapt the kit to the build system that a
  project already has. This vision gives a project the build system that it should have.
- **Why it is time-critical**: both sides become harder to change after they are released.
- **Options**: (a) the facades stay, as the path for a project that cannot take the conventions; (b)
  this vision replaces them; (c) the facades become one convention in the C-03 menu.
