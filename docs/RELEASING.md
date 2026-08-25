## Releasing (automated)

This repository uses **release-plz** to automate:
- version bumps
- changelog updates
- crates.io publishing
- GitHub releases

### How the flow works

**Every push to `main`** runs both release-plz commands, in this order:

1. `release-plz release` publishes crates to crates.io and creates GitHub Releases. It
   only publishes versions that are in the manifests but **not yet on crates.io**, so on
   an ordinary push it finds nothing to do and exits. This is also what makes the pipeline
   self-healing: if a publish is interrupted, the next push to `main` finishes it — no
   manual intervention.
2. `release-plz release-pr` opens or updates a **Release PR** with:
   - crate versions (per-crate, based on each crate's `Cargo.toml`)
   - the root [`CHANGELOG.md`](../CHANGELOG.md)

   It is ordered after `release` so it never derives the next versions from crates.io
   while a publish is mid-flight.

The Release PR is labelled **`release-plz`** automatically. Merging it is what puts the new
versions on `main`; the workflow then attempts to publish on that merge's push, like any
other. If that attempt fails, the versions stay on `main` unpublished until a later push
retries them — see the self-healing note above.

Nothing here keys off the label or off the Release PR being merged — `release` decides for
itself by comparing manifests against crates.io. That is the shape release-plz documents,
and it is why a release cannot be lost by a workflow being skipped or cancelled.

Workflows:
- Root workspace: [`.github/workflows/release-plz.yml`](../.github/workflows/release-plz.yml)

### What gets published

Publishing is controlled by Cargo manifests, and by them alone — `release-plz.toml` has
no say in it:
- crates with `publish = false` are **never published**
- so are crates with no `version` field at all: cargo refuses to publish those, and
  reports them exactly like the explicit form (`gears/bss/**` relies on this today)
- everything else is **publishable** (subject to crates.io rules)

A crate that is not publishable is skipped by release-plz completely — no version bump,
no tag, no changelog, no release. It therefore needs no entry in `release-plz.toml`, and
adding one is dead config.

This repo is configured so that:
- `apps/**`, `examples/**` and `tools/**` are **not** publishable (we set `publish = false`)
- `libs/**` and `gears/**` are publishable as intended

That first rule is enforced, not just documented: `make check-release-config` fails if a
crate under those directories is publishable, and CI runs it on every build (see
[`ci.yml`](../.github/workflows/ci.yml)). Forgetting `publish = false` in a new example is
otherwise invisible until the crate is on crates.io, where a publish cannot be undone.

### What gets a changelog and a GitHub Release

Publishing is one thing; the changelog section and the GitHub Release page that come with
it are configured separately, in [`release-plz.toml`](../release-plz.toml), by path:

- **`gears/**`** — gears, their SDKs and their plugins each get their own section in the
  root [`CHANGELOG.md`](../CHANGELOG.md) and their own GitHub Release. This is what the
  `[workspace]` defaults in `release-plz.toml` give them, so **a new gear needs no entry
  in that file**.
- **`libs/**`** — the framework is published and tagged as usual, but folded into the
  single `cf-gears-toolkit` release: no changelog section, no Release page of its own.
  Every such crate is listed explicitly, so **a new `libs/**` crate does need one entry**
  (`changelog_update = false`, `git_release_enable = false`).

Per-crate git tags (`<crate>-v<version>`) are created for every published crate under
either rule — `git_tag_enable` is left at its default and never set in this repo.

`make check-release-config` also enforces this half: a missing `libs/**` entry, an entry
for a `gears/**` crate, an entry for an unpublishable or no-longer-existing crate, or a
flag set to the wrong value all fail the build. An omitted entry is not inert — the crate
silently starts producing its own changelog and releases.

Known rough edge: release-plz parses the root `CHANGELOG.md` to build a release body, and
one shared file for ~80 independently versioned crates is ambiguous — when two crates
have released the same version number, the parse fails and that release body comes out
empty (`WARN … multiple release notes for 'X'` in the release job log). The fix, if this
starts to matter, is a changelog file per crate via `changelog_path` in a `[[package]]`
section; it is deliberately not done today.

### Versioning policy (as implemented)

Every crate carries its own explicit `version` in its `Cargo.toml`; nothing inherits a
version from the workspace (the root `[workspace.package]` has no `version` field, and
`version.workspace = true` is used nowhere). release-plz bumps each one independently
based on that crate's commits.

Crates that share a version number do so because they are released together in practice,
not because Cargo enforces it.

### Dependency ordering

release-plz publishes crates in the correct order for intra-workspace dependencies.

One case it cannot order away: a **dev-dependency cycle**, where `A` regular-depends on
`B` and `B` dev-depends on `A`. Cargo permits the cycle, and `B` is published first (it is
`A`'s dependency), but `cargo package` still resolves `B`'s dev-dependencies against
crates.io — so a versioned dev-dep on `A` demands a release of `A` that does not exist
yet, and the release fails with:

```
error: failed to prepare local package for uploading
  failed to select a version for the requirement `A = "^x.y.z"`
```

Declare such a dev-dependency **path-only, with no version** — not `workspace = true`,
which carries one. Cargo drops path-only dev-dependencies from the published manifest, so
the cycle never reaches crates.io. `make check-release-config` enforces this too: a
versioned dev-dep is allowed only on a crate that is also a (transitive) normal dependency,
because that is the only case where release-plz guarantees it is already published.
Existing examples:
[`libs/toolkit-contract`](../libs/toolkit-contract/Cargo.toml),
[`libs/toolkit-odata-macros`](../libs/toolkit-odata-macros/Cargo.toml),
[`gears/system/cluster/cluster`](../gears/system/cluster/cluster/Cargo.toml).

### Safety checks

The release workflow does **not** run tests and does **not** block on CI. Merging the
Release PR is the maintainer's release decision; if you merge it, the workflow attempts
to publish the crates.

What does verify a release:

- [`ci.yml`](../.github/workflows/ci.yml) runs on every push to `main`, so the tip of
  `main` gets tested — including the integrated result of merging, which PR CI never
  sees. Pushes touching only markdown or `docs/**` are filtered out by the workflow's
  `paths-ignore`, so no run is created for them: such a commit has no CI result of its
  own and inherits none. That is acceptable only because its code is identical to the
  previous commit, which does have one. If `CI` is ever made a required status check,
  this case needs an always-succeeding placeholder job — a requirement whose workflow
  never runs is never satisfied.
- The publish job runs on the pushed commit itself, so each crate is built from the source
  that landed on `main`. Not a byte-for-byte copy of it: cargo rewrites every manifest on
  the way out, resolving workspace inheritance such as `version.workspace = true` into
  concrete values.
- `cargo publish` compiles every crate, so a crate that does not build cannot be
  published. It builds lib and bin targets only — it runs no tests.

What this does **not** give you: a green light at publish time. A CI run takes 34-57
minutes while the publish starts within a few minutes of the push, so CI for the published
commit is still running while the crates are going out. The CI result lands on the same
commit afterwards and is visible on it in GitHub.

If you want a blocking gate, that belongs in branch protection on `main` (required
status checks) or a merge queue, not in the release workflow.

### Emergency / manual release

If you need a hotfix / manual release, prefer triggering the GitHub Actions workflow instead of publishing locally:

1. Ensure versions are bumped (edit the relevant `Cargo.toml` version fields) and the change is on the target branch.
2. Go to GitHub → **Actions** → **Release (release-plz)** → **Run workflow**.
3. Select `mode = release` (publishes crates + creates GitHub Releases).

Note: `mode = release` publishes whatever is on the target branch and does not block on
CI, so confirm the branch is green first. Running the workspace tests locally gives
faster feedback than waiting for CI:

```bash
cargo test --workspace --no-fail-fast --exclude cf-gears-toolkit-macros-tests --exclude cf-gears-toolkit-db-macros
```

Fallback if CI is unavailable: publish locally from a clean checkout (you must have `CARGO_REGISTRY_TOKEN` set):

```bash
export CARGO_REGISTRY_TOKEN=***   # your crates.io token
cargo publish -p <crate_name>
```

### Notes for the very first publish (bootstrap)

- **crates.io rate limiting (HTTP 429)** can happen when publishing many crates for the first time.
  If the publish job fails with 429, just re-run the same workflow after the timestamp shown in the error.
  The process is idempotent: already-published crates will be skipped on retry.

