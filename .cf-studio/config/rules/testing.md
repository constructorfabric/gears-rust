---
cf: true
type: project-rule
topic: testing
generated-by: auto-config
version: 1.0
---
# Testing

<!-- toc -->

- [Test Placement](#test-placement)
  - [Keep Rust tests near the module behavior](#keep-rust-tests-near-the-module-behavior)
  - [Assert security boundaries explicitly](#assert-security-boundaries-explicitly)
  - [Use E2E orchestration fixtures](#use-e2e-orchestration-fixtures)

<!-- /toc -->

## Test Placement
### Keep Rust tests near the module behavior
Use module-local unit/integration tests and focused helpers for service/domain behavior.
Evidence: `modules/mini-chat/mini-chat/src/domain/service/chat_service_test.rs:18` - service test builders.

### Assert security boundaries explicitly
Security-sensitive modules need tests for traversal, tenant scope, authz denial, and masked access.
Evidence: `modules/file-parser/tests/path_traversal_tests.rs:32` - path traversal tests.

### Use E2E orchestration fixtures
Python E2E tests should use shared base URL, auth headers, server orchestration, health waits, and teardown fixtures.
Evidence: `testing/e2e/lib/orchestrator.py:160` - server startup and health wait.
