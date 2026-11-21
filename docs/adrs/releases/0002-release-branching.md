---
description: "How gear crates get bug fixes for earlier published release lines while new features go to main: trunk-based development with per-gear release branches cut on demand and fixes cherry-picked from main."
status: proposed
date: 2026-11-21
decision-makers: Constructor Fabric Steering Committee
---

# Fix earlier release lines on per-gear release branches, cut on demand, fixes merged to main first


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Policy](#policy)
  - [Release automation](#release-automation)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [O1. Fix forward only](#o1-fix-forward-only)
  - [O2. Trunk-based development with per-gear release branches, cut on demand](#o2-trunk-based-development-with-per-gear-release-branches-cut-on-demand)
  - [O3. A long-lived maintenance branch per gear and major](#o3-a-long-lived-maintenance-branch-per-gear-and-major)
  - [O4. GitFlow](#o4-gitflow)
  - [O5. Fix the oldest line first and merge forward](#o5-fix-the-oldest-line-first-and-merge-forward)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-adr-release-branching`
## Context and Problem Statement

Gear crates are versioned independently by the Cargo rules of
[ADR-0001](./0001-crate-versioning-and-maturity.md). New features, and the next incompatible
version of a gear, are developed on `main`. Consumers that stay on an earlier incompatible
version still need security fixes and critical bug fixes for it. Examples are a consumer on
`1.x` after `2.0.0` is released, or on `0.7.x` after `0.8.0`.

`main` cannot release those fixes, because its version of the gear is already higher. Which
branches hold earlier release lines, how a fix reaches them, and how releases are automated
from them?

## Decision Drivers

* **D1. One place for new work.** Features and the next incompatible version are developed on
  one branch, so contributors do not choose where to work.
* **D2. Fixes reach every supported line.** A security or critical fix is released for every
  supported release line, and no fix is lost on a later line (no regression on upgrade).
* **D3. Independent gears.** Each gear has its own versions and its own supported lines. A fix
  for one gear does not need a release of another gear.
* **D4. Low cost when nothing needs fixing.** Branches that nobody needs are not kept alive.
* **D5. Automation.** release-plz publishes from the branch, tags it, and creates a GitHub
  Release that is not marked as the latest.
* **D6. Industry and Cargo practice.** Follow the backport practice of Rust, Cargo and large Rust
  projects. Cargo itself publishes any version that is not taken yet, so it does not
  constrain the branching model.
* **D7. Few rules.** A contributor can remember them.

## Considered Options

* **O1. Fix forward only.** No branches for earlier lines. Every fix ships in the next release
  from `main`, and consumers upgrade to get it.
* **O2. Trunk-based development with per-gear release branches, cut on demand.** Fixes merge
  to `main` first and are cherry-picked to each supported release branch.
* **O3. A long-lived maintenance branch per gear and major**, created at every incompatible
  release whether or not a fix is needed. Fixes are cherry-picked as in O2.
* **O4. GitFlow.** `develop` for features, `main` for released code, `release/*` and `hotfix/*`
  branches between them.
* **O5. Fix the oldest line first and merge forward.** Fixes land on the oldest supported
  release branch and are merged forward into newer branches and `main`.

## Decision Outcome

Chosen option: **"O2. Trunk-based development with per-gear release branches, cut on demand"**,
because it is the only option that releases fixes to earlier lines (D2) without keeping
branches alive that nobody needs (D4). It also follows the backport practice of Kubernetes, the Rust project and
tokio (D6). O1 leaves earlier lines without fixes. O3 pays for
branches before any fix needs them. O4 and O5 split new work across several branches (D1).

### Policy

1. **`main` is the trunk.** All features, all incompatible changes and every fix merge to `main`
   first.
2. **A release branch exists per gear and incompatible line**, named
   `release/<gear>-<line>.x`, for example `release/file-storage-1.x` or
   `release/file-storage-0.7.x`. The line is the breaking component of the Cargo rules: the
   major from `1.0`, the minor below `1.0`. One branch carries every crate of the gear (the gear,
   its SDK and its plugins). `0.0.z` versions have no compatible line, so they get no release
   branch. A fix for an incubation gear is a new `0.0.z` release, and consumers upgrade by hand.
3. **Record a baseline, cut on demand.** The pull request that moves a gear to a new incompatible
   line also tags the last commit of the old line, for example `file-storage-1.x-base`. At that
   commit, every crate of the gear is at its latest published version of the old line. The
   release branch is created from that tag only when the first fix for the line is needed.
   Until then no branch exists.
4. **Merge to `main` first, then cherry-pick.** A fix is merged to `main` and then cherry-picked
   to every supported release branch, newest first, as Kubernetes does. A fix is written
   directly on a release branch only when the fix on `main` does not apply there, and its pull
   request says why.
5. **Only fixes on release branches.** A release branch accepts security fixes, regression
   fixes and critical bug fixes (data loss, crash, hang), and nothing else. No features, no
   incompatible changes, and no dependency upgrades except the ones an approved fix needs.
6. **Compatible releases only.** A release branch releases the next patch of its line, for
   example `1.4.3` or `0.7.4`. From `1.0` on, it may release the next minor (`1.5.0`) when an
   approved fix needs a new compatible API. Below `1.0`, a minor (`0.8.0`) is breaking, so it is
   never released from a release branch.
7. **Support window.** By default a gear supports its current line on `main` and the previous
   incompatible line for 6 months after the new line's first release. Gear maintainers can
   support more lines or longer, and they list the supported lines and end dates in the gear's
   README. At the end date, the release notes announce the end of support, publishing from the
   branch is turned off, and the branch becomes read-only. It is not deleted.
8. **One maintainer approves each cherry-pick.** Cherry-pick pull requests are labelled
   `backport` and name the original pull request.

### Release automation

- The release workflow also runs on pushes to `release/**`.
- A release branch is a copy of the whole repository, so it may hold unpublished versions of
  other crates. Its release configuration enables only the crates of the branch's gear
  (`release = false` for the workspace, `release = true` for those crates). Before publishing,
  CI checks the list of crates to publish and stops on any other crate.
- On a release branch the patch version is set by hand in the cherry-pick pull request, and the
  workflow runs `release-plz release` only, not `release-plz release-pr`.
  - release-plz does not document a release from a branch whose version is lower than the
    latest on crates.io. Running `release-pr` on such a branch would compute bumps against the
    newer line.
  - Before the first backport release, an integration test proves this flow. It covers an older
    version published after a newer major, crates with different versions, path dependencies, a
    crate already on crates.io, and a second run after a successful publish. If release-plz fails
    the test, release branches use a scoped `cargo publish` workflow instead.
- Publishing several crates is not atomic. If it stops halfway, the workflow is run again: it
  skips the crates already on crates.io and publishes the rest. A published crate that turns out
  broken is yanked and fixed with the next patch.
- `git_release_latest = false` on release branches, so a `1.4.3` GitHub Release never hides
  `2.1.0` as the latest.
- The per-crate tags (`<crate>-v<version>`) are created on the release branch as on `main`.

### Consequences

* Maintainers decide per gear which lines are supported. The default is the current major line (0.y or x.y)
* The release workflow needs a `release/**` trigger, and a branch configuration that runs
  `release` without `release-pr` and with `git_release_latest = false`.
* Maintainers need to cherry-pick security or critical fixes per supported line.
* A release branch builds against the ToolKit versions its line was released with. A fix that
  needs a ToolKit fix needs a ToolKit release on that ToolKit line too, through the same policy.
* Branch protection for `release/**` matches `main`: CI must pass and a maintainer approves.
* Moving a gear to a new incompatible line includes the baseline git tag (policy 3).

### Confirmation

* Branch protection allows only pull requests into `release/**`, and only with the `backport`
  label and a link to the original pull request.
* CI on a release branch fails when a manifest version is not allowed by policy 6, when a crate
  outside the gear would be published, or when the change is not in `main` and the pull request does not say why.
* Release notes of every patch release on a release branch list the original pull requests.

## Pros and Cons of the Options

### O1. Fix forward only

Every fix ships from `main`, and consumers on earlier lines must upgrade.

* Good, because there is one branch and nothing to cherry-pick (D1, D4, D7).
* Good, because it suits gears whose scope still moves, which have no promise to keep.
* Bad, because a consumer on `1.x` gets a security fix only by taking every incompatible change
  of `2.0` (D2).
* Bad, because it makes every incompatible release urgent for every consumer.

### O2. Trunk-based development with per-gear release branches, cut on demand

* Good, because all new work happens on `main` (D1).
* Good, because a fix reaches every supported line, and `main` always has it first, so no fix
  is lost on upgrade (D2).
* Good, because branches are per gear and per line, so gears stay independent (D3).
* Good, because a branch exists only when a fix needs it (D4).
* Good, because it follows the backport practice of Kubernetes (cherry-picks from `master` to
  `release-1.x`), the Rust project (backports to beta and stable after merge to the main branch)
  and tokio (backported patch releases for LTS minor lines) (D6).
* Bad, because a cherry-pick can conflict when `main` has moved far. The fix is then rewritten
  for the branch.
* Bad, because release-plz has no documented maintenance-branch mode. Patch versions on release
  branches are set by hand (D5).

### O3. A long-lived maintenance branch per gear and major

A branch is created at every incompatible release and kept for the whole support window.

* Good, because the branch is ready the moment a fix is needed.
* Good, because the same cherry-pick flow as O2 applies (D1, D2).
* Bad, because a monorepo with many gears gets a branch for every incompatible release, most
  of them never used (D4).
* Bad, because idle branches still need branch protection, CI and cleanup.

### O4. GitFlow

* Good, because it names a branch for every activity, and it is widely known.
* Bad, because work is split between `develop`, `release/*`, `hotfix/*` and `main` (D1, D7).
* Bad, because it is built around one repository-wide release, not independent crate versions
  (D3).
* Bad, because its long-lived branches drift and merge back late. Trunk-based development
  avoids this, and most Rust projects do not use GitFlow (D6).

### O5. Fix the oldest line first and merge forward

* Good, because a merge carries the fix to every newer line, with no cherry-picks.
* Bad, because every release branch must be merged forward into each newer branch and `main`.
  The merges repeat conflict resolution, and branch-only changes can leak forward (D2).
* Bad, because large Rust projects do not use it (D6).

## More Information

- Kubernetes, [Cherry picks](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-release/cherry-picks.md):
  merge to `master` first, cherry-pick to every supported `release-1.x` branch, newest to oldest,
  and only security, regression and critical fixes.
- Rust Forge, [Backporting](https://forge.rust-lang.org/release/backporting.html): patches are
  ported to the beta and stable branches after they are merged into the main branch.
- tokio, [LTS releases](https://github.com/tokio-rs/tokio/blob/master/README.md): each LTS minor
  line gets backported fixes as new patch releases for at least a year.
- Cargo, [Publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html): a
  published version can never be overwritten.
- release-plz, [configuration](https://release-plz.dev/docs/config): `release`,
  `git_release_latest` and `git_tag_name`.

## Traceability

- **Versioning**: [ADR-0001](./0001-crate-versioning-and-maturity.md), `cpt-cf-adr-crate-versioning-and-maturity`.
  Its incompatible lines are the lines this ADR maintains.
- **Maturity**: [MATURITY.md](../../../guidelines/MATURITY.md). Gear maintainers set the
  maturity of each gear and choose which lines are supported.
