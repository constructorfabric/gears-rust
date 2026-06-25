# gears-rust-dev

Kitsoki dev-story instance for the Gears Rust checkout.

Run from the Gears Rust repo root:

```sh
kitsoki run stories/gears-rust-dev/app.yaml
```

Or start the browser UI:

```sh
kitsoki web
```

This instance imports `@kitsoki/dev-story` from the Kitsoki binary. The shared
dev-story hub defines the general workflow; this repository owns the local
profile, command defaults, and any project-specific extensions.

Project profile: `.kitsoki/project-profile.yaml`

gears-rust extensions:

- PRDs publish to `gears/notes-service/docs/PRD.md`.
- Designs publish to `gears/notes-service/docs/DESIGN.md`.
- Design authoring reads `docs/spec-templates/gears-sdlc`.
- Bugfix build/test gates use `make build` and `make test`.

Inferred project commands:

```sh
make dev
make test
make build
```

Command map:

- `dev`: `make dev`
- `test`: `make test`
- `build`: `make build`

Testing:

No deterministic flow fixtures are generated for this project instance yet. Use
the imported dev-story fixtures in the Kitsoki checkout for hub coverage, and
add project-local flows when this repo needs its own story-specific assertions.
