# docs/

- One topic, one owner; link to it instead of copying. Owners: principles and status — `ARCHITECTURE_MANIFEST.md`; gear inventory — `GEARS.md`; gear development, plugins, test commands, CI jobs and coverage numbers — `toolkit_unified_system/` (testing: `15_testing_and_ci.md`); designs are linked from there, never copied into it; releases — `RELEASING.md`; design records — `arch/<area>/` (`PRD.md`, `DESIGN.md`, `ADR/`) and `adrs/`. A single gear's design goes to `gears/<name>/docs/`.
- Every API, path, command and status claim must match the current code. Check it with grep before writing; cite file and symbol, not line numbers. When an ADR's decision ships or is dropped, update its `status`.
- Run `make lychee` (offline link check of tracked `.md` files outside `docs/web-docs/`; `git add` new files first) after editing; `make gts-docs` when GTS IDs change.
- Generated, do not hand-edit: `slides/*.html` (`make slides`), plus the files listed in the root `AGENTS.md`.
- `web-docs/` is the source of the public site; `notify-docs.yml` syncs it to the site repository, so edit it here. Links between site pages are route-relative (`../page/`); check them with `make web-docs-check`, preview with `make web-docs-preview`. Each page needs `title`, `description` and `sidebar` frontmatter.
- Docs-only changes skip Rust CI (`paths-ignore` in `ci.yml`); the `Docs` workflow still checks links.
