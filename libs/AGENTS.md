# libs/

ToolKit and shared crates. Every gear depends on them, and they are published to crates.io.

- New publishable crate: add a `[[package]]` entry to `release-plz.toml` with `changelog_update = false` and `git_release_enable = false` (libs are released under `cf-gears-toolkit`). `make check-release-config` fails without it.
- Each crate has its own explicit `version`; nothing uses `version.workspace`. A dev-dependency that would form a cycle is path-only, with no version (`docs/RELEASING.md` § Dependency ordering).
- Public API changes follow SemVer (`CONTRIBUTING.md` § 3).
- Macro tests: `make test-macros` runs `cf-gears-toolkit-db-macros`, `cf-gears-toolkit-macros-tests` (`libs/toolkit-macros-tests`) and the `cf-gears-toolkit-gts-macros` release-LTO test; `make test-no-macros` runs the rest of the workspace, including `libs/toolkit-contract-macros-tests`.
- Database code is tested against every engine through integration lanes: `make test-integration LANE=toolkit-db-sqlite` (also `toolkit-db-pg`, `toolkit-db-pgq`, `toolkit-db-mysql`; list in `gears-rust.mk` `INTEGRATION_LANES`).
- Design records for these crates: `docs/adrs/toolkit/` and `docs/arch/`; usage guides: `docs/toolkit_unified_system/`.
