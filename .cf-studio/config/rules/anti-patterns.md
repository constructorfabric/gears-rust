---
cf: true
type: project-rule
topic: anti-patterns
generated-by: auto-config
version: 1.0
---
# Anti-Patterns

<!-- toc -->

- [Avoid](#avoid)

<!-- /toc -->

Use this when reviewing code or refactoring risky patterns.

## Avoid
- Do not put REST DTOs or HTTP-specific types into SDK/domain contract layers. Evidence: `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md:61-80`
- Do not give gears raw database connections; runtime owns privileged DB access. Evidence: `libs/toolkit/src/contracts.rs:41-49`
- Do not bypass `OperationBuilder` for REST route contracts that need OpenAPI/error metadata. Evidence: `gears/simple-user-settings/simple-user-settings/src/api/rest/routes.rs:23-90`
- Do not use `time.sleep()` in E2E tests; poll with timeout when async state must settle. Evidence: `docs/toolkit_unified_system/13_e2e_testing.md:154-176`
- Do not silently broaden registry matching; preserve existing ignore/autodetect trade-offs. Evidence: `.cf-studio/config/artifacts.toml:1-260`
