# GUIDELINES

1. See common [RUST.md](./DNA/languages/RUST.md) guideline
2. When develop new REST API use [API.md](./DNA/REST/API.md), [STATUS_CODES](./DNA/REST/STATUS_CODES.md)
3. When develop new Gear use [ToolKit Unified System](../docs/toolkit_unified_system/README.md)
4. Check [SECURITY.md](./SECURITY.md) for security best practices.
5. Review [DEPENDENCIES.md](./DEPENDENCIES.md) for preferred dependencies and libraries.
6. Use [GTS.md](./GTS.md) when defining or changing GTS types, schemas and instances.
7. Use [DESIGN_REVIEW.md](./DESIGN_REVIEW.md) when reviewing PRDs, DESIGNs, ADRs and public contracts.
8. Follow [CONTRIBUTING.md](../CONTRIBUTING.md) for commits and pull requests.

## Precedence

The DNA documents (items 1 and 2) are shared across projects. Where they conflict with this repository, the
[ToolKit docs](../docs/toolkit_unified_system/README.md) and the
[error architecture](../docs/arch/errors/DESIGN.md) win. Known conflicts:

- **Handler and OpenAPI registration.** DNA annotates handlers with `#[utoipa::path]`. Gears register routes and
  their OpenAPI entries with `OperationBuilder` (`libs/toolkit/src/api/operation_builder.rs`,
  [04_rest_operation_builder.md](../docs/toolkit_unified_system/04_rest_operation_builder.md)).
- **Error body.** DNA defines its own `Problem` struct. Gears return `CanonicalError` from
  `libs/toolkit-canonical-errors`, which renders as that crate's RFC 9457 `Problem`; do not define a local one
  ([canonical error categories](../docs/arch/errors/DESIGN.md#12-canonical-error-categories)).
- **Validation status.** DNA uses `422` for validation errors. In Gears canonical `InvalidArgument` maps to `400`
  (`ProblemCategory::http_status` in `libs/toolkit-canonical-errors/src/problem.rs`). `413`, `415` and `422` come only
  from request extraction rejections in `toolkit::api::rest::extract` (see the `OperationBuilder::standard_errors` doc
  comment).
