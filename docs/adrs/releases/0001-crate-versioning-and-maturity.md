---
description: "How every gear crate is published and versioned: every release goes to crates.io and follows the official Cargo versioning rules, and gear maintainers choose the version form and any pre-releases to show the maturity stage they set."
status: proposed
date: 2026-11-21
decision-makers: Constructor Fabric Steering Committee
---

# Publish every gear release to crates.io and version it by the official Cargo rules

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Cargo rules this decision relies on](#cargo-rules-this-decision-relies-on)
  - [Version forms](#version-forms)
  - [Pre-releases](#pre-releases)
  - [Rules](#rules)
  - [Commit messages and release-plz](#commit-messages-and-release-plz)
  - [Consequences](#consequences)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [O1. Do not publish early versions](#o1-do-not-publish-early-versions)
  - [O2. Use a private or alternative Cargo registry for early versions](#o2-use-a-private-or-alternative-cargo-registry-for-early-versions)
  - [O3. Put the maturity in the crate name](#o3-put-the-maturity-in-the-crate-name)
  - [O4. Use plain SemVer and put the maturity only in metadata](#o4-use-plain-semver-and-put-the-maturity-only-in-metadata)
  - [O5. Publish every version to crates.io and follow the official Cargo rules](#o5-publish-every-version-to-cratesio-and-follow-the-official-cargo-rules)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-adr-crate-versioning-and-maturity`

## Context and Problem Statement

A gear ships as Rust crates: the gear crate, its SDK crate, and its plugin crates. Other gears,
applications and vendors use them as Cargo dependencies.

Gear maintainers give each gear a maturity stage in `gear.gdl`
([MATURITY.md](../../../guidelines/MATURITY.md)). Consumers need to see that stage.
Contributors need room to change and try new APIs before they are frozen.

Some early gears are already on crates.io, and a published version can never be removed. A gear
can also be stable in one major version while its next major version is still early.

Where do we publish gear crates, and how do we number their versions?

## Decision Drivers

* **D1. Reliable channel.** Consumers get gear crates from a reliable place where a published
  version never changes. It must work well with Cargo, lock files, mirrors, offline builds and
  docs.
* **D2. SemVer and Cargo rules.** Versions follow SemVer 2.0 and the official Cargo rules. Then
  the default Cargo requirements and `cargo-semver-checks` protect consumers.
* **D3. Versions never go back.** Within a release line, a new version is always higher than
  every version already published. A fix for an older line, such as `1.4.6` after `2.0.0`, is
  allowed.
* **D4. Visible maturity.** A consumer should be able to see the maturity without reading
  `gear.gdl`. Most consumers see only the version, the crates.io page and the README. The
  version can give a hint, but the README states the stage.
* **D5. Opt-in risk, handled by the consumer.** A consumer gets unfinished code only if they ask
  for it. A consumer who asks for it accepts that it may break between releases. A dependency can bring in
  an early gear without the application asking, so gears disclose such dependencies (rule 5).
* **D6. Maturity is not a straight line.** A new major version can be early while the current
  major version is stable.
* **D7. Freedom to try new APIs.** We can ship and try new APIs, SDK traits and GTS types before
  we freeze them, also in a stable gear, without breaking its stable promise.
* **D8. Automation.** release-plz, `cargo-semver-checks` and GitHub Releases support the
  strategy, including pre-release versions. Manual steps are rare and planned.
* **D9. Few rules.** A contributor can remember them.
* **D10. Three maturity stages.** Gear maintainers give each gear one of three stages:
  incubation, preview or stable. A stage says how settled the gear's scope, meaning and features
  are. Versioning must support all three
  stages and keep them separate from compatibility.
* **D11. Incubation gears that are already published.** Some incubation gears are already on
  crates.io with versions like `0.4.3` or `0.6.0`. The strategy must give them a clear way
  forward without lowering their version.
* **D12. Maintainers choose pre-releases.** The gear's maintainers decide when a release line
  uses a pre-release (`-alpha.N`, `-beta.N`, `-rc.N`) and how long it lasts. Cargo's advice
  applies: libraries should avoid depending on pre-releases.
* **D13. Maintainers set expectations on purpose.** A move into, between or out of pre-release
  labels is a maintainer decision, never a side effect of a commit message. While a line is a
  pre-release, every release only raises its counter, so consumers know that anything may
  change until the label is removed.

## Considered Options

* **O1. Do not publish early versions.** Consumers use git dependencies until the gear is ready.
* **O2. Use a private or alternative Cargo registry for early versions.** Use crates.io from
  preview on.
* **O3. Put the maturity in the crate name**, for example `foo-incubation`, `foo-preview` and
  `foo`.
* **O4. Use plain SemVer and put the maturity only in metadata**: `gear.gdl`,
  `[package.metadata]` or README badges.
* **O5. Publish every version to crates.io and follow the official Cargo rules.** Use `0.0.z`,
  `0.y.z` and `x.y.z`. Gear maintainers may add `-alpha.N`, `-beta.N` or `-rc.N` to any release
  line, when they choose and for as long as they choose.

## Decision Outcome

Chosen option: **"O5. Publish every version to crates.io and follow the official Cargo rules"**,
because it is the only option that meets every driver. It gives one reliable channel (D1) and
keeps Cargo's own compatibility rules (D2). The version shows the stage (D4, D10), and
maintainers decide when to use pre-releases (D12, D13). The other options fail:

* O1 and O2 do not give a reliable channel for everyone.
* O3 breaks automation and renames crates at every stage change.
* O4 hides the maturity and gives unfinished code to everyone.

### Cargo rules this decision relies on

- **The first non-zero number is the breaking one.** In `0.0.z`, Cargo treats every
  version as incompatible with the others. In `0.y.z`, a change of `y` is breaking. In `x.y.z`, a change of `x` is breaking
  ([SemVer compatibility](https://doc.rust-lang.org/cargo/reference/semver.html)).
  - The default requirement `0.0.3` means only `0.0.3`.
  - `0.2.3` means `>=0.2.3, <0.3.0`.
  - `1.2.3` means `>=1.2.3, <2.0.0`.
  - See [version requirement syntax](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#version-requirement-syntax).
- **Cargo uses a pre-release only if you ask for it.** `foo = "1.0"` does not match
  `1.0.0-alpha`
  ([pre-releases](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#pre-releases)).
- **A pre-release requirement moves forward by itself.** `foo = "1.0.0-alpha"` also accepts
  `1.0.0-beta`, `1.0.0` and `1.2.0`. Projects may break things between pre-releases, so Cargo
  suggests an exact `=` requirement. Cargo also advises that a library should not depend on a
  pre-release unless the library is a pre-release too
  ([pre-releases](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#pre-releases)).
- **Do not narrow the upper bound.** Avoid requirements like `~2.0` or `2.0.*`. Use `Cargo.lock`
  to fix an exact version
  ([version requirement syntax](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#version-requirement-syntax)).
- **Removing a Cargo feature is a breaking change.** Adding one is not
  ([SemVer compatibility](https://doc.rust-lang.org/cargo/reference/semver.html#cargo-feature-remove)).

### Version forms

| Version | What Cargo reads | Stage hint | Breaking change |
|---|---|---|---|
| `0.0.z` | every version is incompatible with the others | incubation | any release: `0.0.(z+1)` |
| `0.y.z` (`y` is 1 or more) | `y` is the breaking number | preview | `0.(y+1).0` |
| `x.y.z` (`x` is 1 or more) | `x` is the breaking number | stable | `(x+1).0.0`, after a deprecation (below) |
| `X.Y.Z-alpha.N`, `-beta.N`, `-rc.N` | unfinished versions of the upcoming `X.Y.Z` | the stage the maintainers choose for that line | any pre-release |

**Deprecation before removal.** A stable crate removes an item only in its next major version.
Before that, a minor release should mark the item `#[deprecated(note = "use ... instead")]` and adds
its replacement.

For example, a `1.3.0` release deprecates `list_files` and adds `list_files_paged`. The `2.0.0`
release then removes `list_files`. Consumers can move to `list_files_paged` while they are still
on `1.x`. A security or correctness fix may skip the deprecation step. The release notes say why.

### Pre-releases

Gear maintainers decide when a line uses a pre-release, which label it uses, and how long it
lasts (D12). These are the common uses:

| Situation | Typical versions | Note |
|---|---|---|
| New gear in incubation | `0.0.1`, `0.0.2`, … | A pre-release is optional, for example `0.0.4-alpha.1`. It keeps the release away from consumers who did not ask for it. |
| Incubation gear already published at `0.y` | `0.(y+1).0-alpha.1`, `-alpha.2`, … | The label shows incubation. It ends with `0.(y+1).0` at promotion to preview. |
| Large breaking change in preview | `0.(y+1).0-beta.N` or `-rc.N` | Optional. Usually a new `0.(y+1).0` is enough. |
| Before the first stable release | `1.0.0-rc.N` | Optional. A last check of the stable API. |
| Next major of a stable gear | `2.0.0-alpha.N`, `-beta.N`, `-rc.N` | When the change cannot sit behind an `unstable-v2` feature. |

The labels mean:

- **`alpha`:** anything may change;
- **`beta`:** the scope is set, but details may change;
- **`rc`:** this is the release unless a problem is found.

The labels are ordered `alpha.9 < alpha.10 < beta.1 < rc.1 < final`.

### Rules

1. **Publish every release to crates.io**, incubation releases too.
2. **Never go back.** A new version is higher than every published version of its line, yanked
   versions too. Fixes for an older line come from a release branch
   ([ADR-0002](./0002-release-branching.md)).
3. **Bump by the Cargo rules.** The first non-zero number is the breaking one.
4. **Let the version show the stage.** Use `0.0.z` or a pre-release for incubation, `0.y.z` for
   preview, and `1.0.0` or higher for stable. A stage change moves to the next form, for example
   `0.0.7` → `0.1.0` → `1.0.0`, or `0.7.0-alpha.4` → `0.7.0`.
5. **Pre-releases are the maintainers' choice.** Maintainers start and end a pre-release with a
   reviewed pull request.
6. **One version line per gear.** The gear crate, its SDK crate and its plugin crates always
   have the same breaking number and the same pre-release label. Their other numbers may differ.
   - Allowed: `foo 1.4.2` with `foo-sdk 1.6.0`.
   - Allowed: `foo 2.0.0-beta.1` with `foo-sdk 2.0.0-beta.3`.
   - Not allowed: `foo 2.0.0` with `foo-sdk 1.6.0`, because the breaking numbers differ.
   - Not allowed: `foo 2.0.0-beta.1` with `foo-sdk 2.0.0`, because only one is a pre-release.
7. **Requirements.**
   - **Pre-releases.** A consumer that uses a pre-release pins it, for example
     `foo-sdk = "=2.0.0-beta.2"`. Pinning controls updates. It does not accept the maturity
     risk.
   - **Conflicting pins.** Two dependencies that pin different pre-releases of one crate can
     make the graph impossible to resolve.
   - **Other versions.** Use default requirements.
   - **Cargo.lock.** Applications commit `Cargo.lock`. A library's `Cargo.lock` does not
     affect its consumers.
8. **Unstable features keep their name.** When an `unstable-<feature>` Cargo feature becomes
   stable or is dropped, the feature name stays, empty, until the next breaking version.
   [MATURITY.md](../../../guidelines/MATURITY.md) has the rules for unstable features.
9. **Stage changes are planned.** A stage change is a reviewed pull request. It changes
   `gear.gdl` and sets the versions by hand.

**Sources of truth.**

- `gear.gdl` is authoritative for the stage. The gear's maintainers set it.
- The first line of each crate's `README.md`, which crates.io shows, presents the stage to
  people. crates.io no longer shows `[badges]`, and Cargo has no maturity field.
- The SemVer version is the compatibility contract. Its form is a hint to the stage, not an exact
  mapping.

### Commit messages and release-plz

release-plz picks the next version from Conventional Commits and from `cargo-semver-checks`:

| Commit | `0.0.z` | `0.y.z` | `x.y.z` | pre-release |
|---|---|---|---|---|
| `fix:` | patch | patch | patch | counter: `beta.1` → `beta.2` |
| `feat:` | patch | patch | minor | counter |
| `feat!:`, `fix!:`, or a `BREAKING CHANGE:` footer | patch | `0.(y+1).0` | `(x+1).0.0` | counter |

- When `cargo-semver-checks` finds an API break in a commit that has no breaking marker,
  release-plz still bumps the breaking number. The tool finds many Rust API breaks, not all,
  and it does not check wire contracts.
- No commit message moves a gear from `0.0` to `0.1`, to `1.0.0`, or into or out of a
  pre-release. Maintainers do that by hand (rules 5 and 9, D13).
- With `git_release_type = "auto"`, the GitHub Release of a pre-release is marked as a
  pre-release. A `0.0.z` release has no label, so it is a normal GitHub Release. Its release
  notes state the stage.

### Consequences

* CI needs a version-policy check before publishing. release-plz calculates bumps, but it does
  not enforce rules 5, 6 and 9, or the agreement with `gear.gdl`, or the limits of a backport
  line (ADR-0002). Its `version_group` makes versions identical, which is stricter than rule 6.
  The check allows planned manual version edits and rejects inconsistent versions before
  publishing. It also checks that the dependency graph of the affected gears still resolves.
* Publishing cannot be undone. Before it, CI runs `cargo package`. It checks the package
  contents, secrets and licenses. A maintainer approves the release.
* Gear maintainers keep the stage in `gear.gdl` and on the first line of each crate's
  `README.md`. Most gears need a README for this.
* An incubation gear that is already published can show its stage with a pre-release, for
  example `0.7.0-alpha.1`, or keep `0.y.z` and say "incubation" in its README.
* Moving from `0.0` to `0.1`, from `0.y` to `1.0.0`, and into or out of a pre-release are
  planned pull requests. The steering committee approves stage changes by the rules of
  [MATURITY.md](../../../guidelines/MATURITY.md).
* Fixes for the older major gear versions follow [ADR-0002](./0002-release-branching.md).
* The release configuration sets `git_release_type = "auto"`. It does not set
  `features_always_increment_minor`, because release-plz says that option breaks the Cargo rules
  for `0.x`.
* Incubation releases stay on crates.io forever. A bad release can be yanked, but not deleted.

## Pros and Cons of the Options

`+` meets the driver, `~` meets it in part, `−` does not meet it.

| Driver | O1 git | O2 private registry | O3 crate name | O4 metadata | O5 Cargo rules |
|---|---|---|---|---|---|
| D1 Reliable channel | − | ~ | + | + | + |
| D2 SemVer and Cargo | ~ | + | + | + | + |
| D3 Never go back | + | ~ | ~ | + | + |
| D4 Visible maturity | ~ | ~ | + | ~ | + |
| D5 Opt-in risk | + | + | + | − | + |
| D6 Not a straight line | ~ | ~ | ~ | ~ | + |
| D7 Freedom to try APIs | + | + | ~ | ~ | + |
| D8 Automation | − | ~ | − | + | + |
| D9 Few rules | + | ~ | ~ | + | + |
| D10 Three stages | ~ | ~ | ~ | ~ | + |
| D11 Published incubation gears | − | − | ~ | + | + |
| D12 Maintainers choose pre-releases | − | ~ | − | ~ | + |
| D13 Expectations set on purpose | − | ~ | ~ | − | + |

### O1. Do not publish early versions

Consumers use `git = "..."` dependencies until the gear is published.

* Good, because nothing permanent is published before the scope settles. A mistake can be
  rewritten.
* Good, because no crate name is taken on crates.io for a gear that may be dropped.
* Bad, because git hosting is less reliable than a package registry. A branch or tag can change
  unless consumers pin a commit, and an outage stops every build that has no cached copy (D1).
* Bad, because it ties consumers to one git provider and its URLs. Mirrors, proxies and offline
  builds need extra setup for each provider (D1).
* Bad, because a crate on crates.io cannot have a git dependency, so no published crate can use
  the gear (D1).
* Bad, because there is no docs.rs page, and release tools do not version git dependencies (D8).
* Bad, because there is no version to read. Consumers see a commit hash or a branch name, and
  maintainers have no version labels to set expectations with (D4, D12, D13).
* Bad, because it works only for the first stage, not for an early next major (D6, D10).
* Bad, because incubation gears that are already on crates.io cannot go back to git (D11).

### O2. Use a private or alternative Cargo registry for early versions

* Good, because it works with Cargo and published versions do not change. Consumers keep version
  requirements and lock files (D2).
* Good, because early versions can be deleted, if the registry allows it.
* Bad, because it is a second service to run, secure and keep available (D1).
* Bad, because every application, CI system and vendor must add the registry to
  `.cargo/config.toml` (D1).
* Bad, because crates.io does not accept a crate that depends on another registry. A crate on
  crates.io cannot use an early gear (D1, D6).
* Bad, because the crate moves to crates.io at preview, and every consumer must change its
  dependency (D9).
* Bad, because incubation gears that are already on crates.io cannot move to the private
  registry (D3, D11).

### O3. Put the maturity in the crate name

* Good, because the stage shows in every listing and every `Cargo.toml` (D4). SurrealDB publishes
  separate `-nightly` and `-beta` crates this way.
* Good, because each crate name follows the plain Cargo rules (D2).
* Good, because an incubation gear that is already published can move to a new
  `foo-incubation` name (D11).
* Bad, because a stage change renames the crate. Every consumer changes `Cargo.toml` and its
  imports, and the old name stays on crates.io (D3, D9).
* Bad, because there are three times as many crate names, and release tools do not know that
  `foo-preview` and `foo` belong together (D8).
* Bad, because the name, not a version label, carries the stage, so maintainers cannot run a
  short pre-release of one line (D6, D12).

### O4. Use plain SemVer and put the maturity only in metadata

* Good, because there are no new version rules (D9).
* Good, because the stage is fully separate from the version (D10).
* Good, because an incubation gear that is already published needs no version change (D11).
* Bad, because crates.io does not show `[badges]` and Cargo ignores `[package.metadata]`. A
  consumer cannot tell an incubation `0.6.0` from a preview `0.6.0` without reading the README or
  `gear.gdl` (D4).
* Bad, because every consumer with a default requirement gets incubation releases automatically
  (D5).
* Bad, because a stage change in metadata does not reach the consumer's build, so it does not
  set expectations (D13).

### O5. Publish every version to crates.io and follow the official Cargo rules

* Good, because there is one reliable channel that works with Cargo (D1).
* Good, because each version means what Cargo says it means. There is no extra convention to
  learn (D2, D9).
* Good, because `gear.gdl` and the README state the stage, and the version form hints at it
  without tying stages to compatibility (D4, D10).
* Good, because an incubation gear that is already published can use `0.(y+1).0-alpha.N`, or
  keep `0.y.z` and say "incubation" in its README (D11).
* Good, because maintainers choose when a line is a pre-release and for how long. While it is,
  release-plz only raises the counter, so the label alone tells consumers that anything may
  change. Labels change only when maintainers decide (D12, D13).
* Good, because `unstable-*` features and pre-releases cover an early next major of a stable
  gear (D6, D7).
* Good, because release-plz handles `0.0.z`, `0.y.z`, `x.y.z` and pre-release counters without
  special settings (D8).
* Neutral, because consumers of a pre-release handle it in their own build: they pin it with `=`
  and resolve conflicts between pre-releases (D5).
* Neutral, because Cargo treats every `0.0.z` release as breaking. Consumers of an incubation gear
  update by hand, and two consumers on different `0.0.z` releases each get their own copy.
* Bad, because a gear that depends on a gear in pre-release must be a pre-release too, or wait,
  as Cargo advises. A long pre-release can hold back the gears that depend on it.

## More Information

**Cargo rules.**

- [Specifying dependencies: pre-releases](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#pre-releases)
- [Specifying dependencies: version requirement syntax](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#version-requirement-syntax)
- [SemVer compatibility](https://doc.rust-lang.org/cargo/reference/semver.html)
- [Specifying dependencies from other registries](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#specifying-dependencies-from-other-registries)
- [SemVer 2.0](https://semver.org): how pre-releases are ordered (§9, §11)
- [RFC 3493](https://rust-lang.github.io/rfcs/3493-precise-pre-release-cargo-update.html): known
  limits of pre-releases in Cargo

**release-plz.**

- [Configuration](https://release-plz.dev/docs/config)
- [set-version](https://release-plz.dev/docs/usage/set-version)
- [Discussion #2443](https://github.com/release-plz/release-plz/discussions/2443): pre-release
  label changes are manual
- [next_version](https://docs.rs/next_version/latest/next_version/): `0.0.x` gets only patch
  bumps, and a pre-release counter goes up whatever the commit type
- [Rust blog, 2023-10-26](https://blog.rust-lang.org/2023/10/26/broken-badges-and-23k-keywords/):
  crates.io no longer shows badges from `Cargo.toml`

**Industry practice.**

| Project | Early versions | Unstable APIs in a stable line | Next major |
|---|---|---|---|
| tokio | — | `--cfg tokio_unstable`, may break in `1.x` minor releases | — |
| clap | — | `unstable-*` Cargo features, may break in minor releases | `unstable-v4` feature in `3.x` to try `4.0` |
| axum, bevy | `0.y.z` | — | `-rc.N` and `-alpha.N` pre-releases before a breaking release |
| AWS SDK for Rust | `0.0.x-alpha` developer preview, then `0.x` | — | `1.0` GA |
| OpenTelemetry Rust | `0.y.z` | status of each crate in its README | — |
| SurrealDB | separate `-nightly` and `-beta` crates | — | — |

## Traceability

- **Maturity**: [MATURITY.md](../../../guidelines/MATURITY.md) defines the stages, who sets them,
  and the compatibility rules for wire contracts, GTS types and stored data. This ADR only
  decides how crates are published and versioned for those stages.
- **Branching**: [ADR-0002](./0002-release-branching.md), `cpt-cf-adr-release-branching`, decides
  how fixes reach older release lines.
