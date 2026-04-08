# Contributing to Cyber Fabric

Thank you for your interest in contributing to Cyber Fabric! This document provides guidelines and information for contributors.

We welcome contributions in:

- **New modules**: Add functionality to the platform
- **Bug fixes**: Fix issues in existing code
- **Documentation**: Improve guides and examples
- **Testing**: Add test coverage and improve test quality
- **Performance**: Optimize critical paths
- **Developer experience**: Improve tooling and workflows


## 1. Quick Start

### 1.1 Prerequisites

- **Rust stable** with Cargo (Edition 2024, Rust MSRV 1.92.0)
- **Protocol Buffers compiler** (`protoc`) (see `README.md`)
- **Git** for version control
- **Your favorite editor** (VS Code with rust-analyzer recommended)

### 1.2 Development Setup

```bash
# Clone the repository
git clone --recurse-submodules <repository-url>
cd cyberfabric-core

# If you didn't clone with --recurse-submodules (includes Cypilot for PR reviews)
git submodule update --init --recursive

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install required components
rustup component add clippy rustfmt

# Build the project
make build

# Run tests
make test

# Start the development server (SQLite quickstart)
make quickstart

# Start the development server with the example users_info module
cargo run --bin hyperspot-server --features users-info-example -- --config config/quickstart.yaml run
```

## 2. Development Workflow

### 2.1. Create a Feature Branch or Fork

```bash
git checkout -b feature/your-feature-name
```

Use descriptive branch names:
- `feature/user-authentication`
- `fix/memory-leak-in-router`
- `docs/api-gateway-examples`
- `refactor/entity-to-contract-conversions`

As an alternative, you can fork the repository to your own GitHub account.


### 2.2. Make Your Changes

Follow the coding standards and guidelines:

1. See common [RUST.md](./guidelines/DNA/languages/RUST.md) guideline
2. When develop new REST API use [API.md](./guidelines/DNA/REST/API.md), [STATUS_CODES](./guidelines/DNA/REST/STATUS_CODES.md)
3. When develop new Module use [ModKit Unified System](./docs/modkit_unified_system/README.md)
4. Security policy [SECURITY.md](./SECURITY.md) and secure coding [guidelines/SECURITY.md](./guidelines/SECURITY.md)
5. ModKit architecture and invariants [docs/modkit_unified_system/README.md](./docs/modkit_unified_system/README.md)

Module directories under `modules/` must use kebab-case (validated by `scripts/validate_module_names.py` and enforced in CI).

Always include unit tests when introducing new code.


### 2.3. Run Code Quality Checks

Build and run all the quality checks:

```bash
# Run the complete quality check suite: formatting, linting, tests, and security
make check # Linux/Mac
python scripts/ci.py check # Windows

# Run the full pipeline (includes build + e2e-local)
make all # Linux/Mac
python scripts/ci.py all # Windows
```

Note: CI workflows may not run for PRs that only touch `*.md` files or `docs/**` due to path filters.

Aim for high test coverage:
- **Unit tests**: Test individual functions and methods
- **Integration tests**: Test module interactions
- **End-to-end tests**: Test complete request flows

```bash
# Run tests with coverage (automatically detects your OS)
make coverage # Run both unit and e2e tests with code coverage
make coverage-unit # Run only unit tests with code coverage
make coverage-e2e-local # Run only e2e tests with code coverage
```

### 2.4. Run Fuzzing Tests (Recommended)

Before submitting changes to parsers or validation logic, run fuzzing:

```bash
# Quick smoke test (30s per target)
make fuzz

# Longer test for critical changes (5 minutes per target)
python scripts/ci.py fuzz --seconds 300

# Target specific component
make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=600
```

Fuzzing helps catch:
- Parser crashes
- Performance problems
- Edge cases

See `fuzz/README.md` for detailed fuzzing documentation.

Helpful environment variables:

```bash
# Turn on debug-level logging
export RUST_LOG=debug

# Show backtraces on panic
export RUST_BACKTRACE=full
```


### 2.5. Sign Your Commits (DCO)

This project uses the Developer Certificate of Origin (DCO) version 1.1.
- The DCO text is included in `guidelines/DNA/DCO.txt` (Version 1.1). This is the current and widely adopted version; please keep it as 1.1.
- Every commit must include a Signed-off-by line to certify you have the right to submit the contribution under the project license (Apache-2.0).

Sign off your commits:
```bash
git commit -s -m "your message"
```
This adds a footer like:
```
Signed-off-by: Your Name <your.email@example.com>
```
Enable auto sign-off for all commits:
```bash
git config --global format.signoff true
```


### 2.6. Commit Changes

Follow a structured commit message format:

```text
<type>(<module>): <description>
```

- `<type>`: change category (see table below)
- `<module>` (optional): the area touched (e.g., api_gateway, modkit, ecommerce)
- `<description>`: concise, imperative summary

Accepted commit types:

| Type       | Meaning                                                     |
|------------|-------------------------------------------------------------|
| feat       | A new feature                                               |
| fix        | A bug fix                                                   |
| tech       | A technical improvement                                     |
| cleanup    | Code cleanup                                                |
| refactor   | Code restructuring without functional changes               |
| test       | Adding or modifying tests                                   |
| docs       | Documentation updates                                       |
| style      | Code style changes (whitespace, formatting, etc.)           |
| chore      | Misc tasks (deps, tooling, scripts)                         |
| perf       | Performance improvements                                    |
| ci         | CI/CD configuration changes                                 |
| build      | Build system or dependency changes                          |
| revert     | Reverting a previous commit                                 |
| security   | Security fixes                                              |
| breaking   | Backward incompatible changes                               |

Examples:

```text
feat(auth): add OAuth2 support for login
fix(ui): resolve button alignment issue on mobile
tech(database): add error abstraction for database and API errors
refactor(database): optimize query execution
test(api): add unit tests for user authentication
docs(readme): update installation instructions
style(css): apply consistent spacing in stylesheet
```

Best practices:

- Keep the title concise (ideally ≤ 50 chars)
- Use imperative mood (e.g., "Fix bug", not "Fixed bug")
- Make commits atomic (one logical change per commit)
- Add details in the body when necessary (what/why, not how)
- For breaking changes, either use `feat!:`/`fix!:` or include a `BREAKING CHANGE:` footer

New functionality development:

- Follow the repository structure in `README.md`
- Prefer soft-deletion for entities; provide hard-deletion with retention routines
- Include unit tests (and integration tests when relevant)

### 2.7. Push and Create PR

```bash
git push origin feature/your-feature-name
```

Then create a Pull Request on GitHub with:
- Clear title and description
- Reference to related issues
- Test coverage information
- Breaking changes (if any)

Use the following PR Description Template

```markdown
## Description
Brief description of the changes made.

## Type of Change
- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Documentation update

## Testing
- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Manual testing completed
- [ ] New tests added for new functionality

## Documentation
- [ ] Code is documented with rustdoc comments
- [ ] README updated (if applicable)
- [ ] API documentation updated (if applicable)

## Checklist
- [ ] Code follows project style guidelines
- [ ] Self-review completed
- [ ] No linting errors (`cargo clippy`)
- [ ] Code is properly formatted (`cargo fmt`)
- [ ] Tests pass (`cargo test`)

## Related Issues
Closes #issue_number
```

### 2.8. Review Process

1. **Automated checks** must pass (CI/CD pipeline)
2. **At least one approval** from maintainer required
3. **All conversations resolved** before merge
4. **Up-to-date with main** branch

Merge Strategy:

- **Squash and merge** for feature branches
- **Rebase and merge** for simple fixes
- **Merge commit** for release branches

### 2.9. Local PR Review with Cypilot

After pushing your PR and waiting for the cloud AI bots (CodeRabbit, Qodo, etc.) to complete their reviews, run a local Cypilot review to catch additional issues
before requesting human review:

```text
cypilot review PR <number>
```

Use any supported IDE agent (Windsurf, Cursor, Claude, Copilot) — each redirects to the canonical workflows via `/cypilot-pr-review` and `/cypilot-pr-status` commands.

You can also check the PR status (unreplied comments, severity, etc.):

```text
cypilot get status for PR <number>
```

See the results in `.prs/{ID}/` folder.

See [docs/pr-review/README.md](./docs/pr-review/README.md) for full setup (GitHub CLI authentication, configuration, available review prompts) and usage details.


## 3. Versioning

This topic defines how Cyber Fabric versions crates and handles breaking changes.

## Scope

Applies to:
- All Rust crates in this repository (libraries, modules, SDKs, macros).
- Public APIs and contracts exposed to downstream users (Rust API, REST, gRPC/proto, CLI).

Non-goals:
- Internal-only refactors that do not change any public contract (still must be tested, but do not force version bumps).

## SemVer Rules (Crates)

We use Semantic Versioning: `MAJOR.MINOR.PATCH`.

### PATCH (x.y.Z)
Allowed:
- Bugfixes
- Performance improvements
- Internal refactors
- Doc and test updates
  Not allowed:
- Any public API or behavior change that can break downstream compilation

### MINOR (x.Y.z)
Allowed:
- Backward compatible new features
- New APIs that do not break existing code
  Not allowed:
- Breaking changes

### MAJOR (X.y.z)
Required for:
- Any breaking change (see the definition below)

## Pre-1.0 Policy (0.x)

For crates with version `0.x.y`:
- `0.(x+1).0` is treated as a breaking release.
- `0.x.(y+1)` is treated as non-breaking.

Rule of thumb: before 1.0, MINOR behaves like MAJOR.

## What Counts as a Breaking Change (Rust)

Breaking means: existing downstream code may fail to compile or a stable contract is violated.

Examples:
- Removing or renaming any `pub` item
- Changing function signatures (params, generics, bounds, return type)
- Changing public struct/enum layout in a way that breaks construction or pattern matching
- Removing a `pub` field
- Removing trait impls that downstream relies on
- Adding a method to a public trait without a default implementation
- Tightening trait bounds or visibility in a way that reduces what compiles

If in doubt: treat it as breaking.

## What Is Not Breaking (But Must Be Noted)

Not breaking (SemVer-wise), but must be documented in changelog/release notes:
- Performance changes
- Changes in logging text, error strings, metrics naming
- More strict validation returning errors in previously accepted edge cases (allowed only if it does not violate an explicit documented contract)

## Public Contract Types and Their Versioning

### Rust crate API
- Governed by SemVer rules above.

### REST API
- Versioned in the URL (`/v1/...`, `/v2/...`).
- Breaking REST changes require a new API version (for example `/v2`) and a deprecation period for old versions.

### gRPC / Protobuf
- Treat `.proto` as a public contract.
- Follow protobuf compatibility rules:
    - Do not reuse field numbers.
    - Do not change field types in incompatible ways.
    - Prefer adding optional fields over changing existing ones.
- Breaking proto changes require a MAJOR bump for affected crates and, if exposed externally, a new API/proto version strategy.

### CLI
- User-facing CLI flags, commands, and output formats are public.
- Breaking CLI changes require MAJOR.

## Deprecation Policy

When feasible, prefer deprecation over immediate removal:
- Mark APIs as deprecated for at least one MINOR release before removal.
- Include migration notes (what to use instead).

Removal of deprecated APIs is breaking and requires MAJOR.

## Workspace Policy (Monorepo)

We use per-crate versioning, controlled via Cargo manifests and release automation.

### Publishable vs internal crates
- `publish = false` means the crate is internal and must not be published.
- Internal crates can still follow SemVer for sanity, but do not promise external stability.

### Version sources
- Modules and SDKs keep explicit `version = "..."` in their `Cargo.toml`.
- ModKit libs may use `version.workspace = true` (unified framework versioning).

## ModKit Unified Release Rule

ModKit is released as a unified framework:
- Only `cf-modkit` produces changelog entries and GitHub releases.
- Other `cf-modkit-*` crates are published to crates.io but do not create separate changelog entries/releases.

## Release Process (Automation)

We use release-plz:
- Release PRs are labeled `release-plz`.
- Repository-level `CHANGELOG.md` is the single changelog source.
- GitHub releases are enabled where configured.

SemVer checks:
- We aim to run semver checks for published crates.
- If temporarily disabled (bootstrap or tooling noise), do not use that as an excuse to sneak breaking changes into MINOR/PATCH.

## How to Decide the Version Bump

Use this table:

| Change | Bump |
|------|------|
| Bugfix only | PATCH |
| Backward compatible new API/feature | MINOR |
| Any breaking change | MAJOR |
| Pre-1.0: breaking change | bump 0.(x+1).0 |

## Required Release Notes

Every release must document:
- Added
- Changed
- Fixed
- Breaking (if any) with migration steps

No "minor fixes" wording for releases that break users.

## Enforcement

Before merging changes that affect public crates/contracts:
- Tests must pass.
- If you touched a public surface, you must justify the version bump category in the PR description.

## Getting Help

- **GitHub Issues**: For bug reports and feature requests
- **GitHub Discussions**: For questions and general discussion
- **Documentation**: Check existing docs first
- **Code Examples**: Look at existing modules for patterns

---

Thank you for contributing to Cyber Fabric!
