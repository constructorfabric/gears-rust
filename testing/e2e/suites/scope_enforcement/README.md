# Route Policy Enforcement E2E Tests

Tests for the Route Policy Enforcement middleware, which performs coarse-grained
early rejection of requests based on token scopes without calling the PDP.

## Running Tests

These tests use the scope-enforcement overlay
(`testing/e2e/suites/scope_enforcement/e2e.yaml`, applied over
`config/e2e-local.yaml`) and are **not** part of the standard CI e2e suite.

```bash
# Build the server, apply the overlay, start it, and run this suite:
make e2e-local SUITE=scope-enforcement
```

The runner sets `E2E_SCOPE_ENFORCEMENT=1` automatically (the suite's conftest
skips without it). The overlay adds the api-gateway `route_policies` rules and
the scoped test tokens below on top of the shared config.

## Test Tokens

The overlay defines these test tokens:

| Token | Scopes | Expected Access |
|-------|--------|-----------------|
| `token-full-access` | `["*"]` | All routes (first-party) |
| `token-users-read` | `["users:read"]` | `/users-info/v1/users*` |
| `token-users-admin` | `["users:admin"]` | `/users-info/v1/users*` |
| `token-cities-admin` | `["cities:admin"]` | `/users-info/v1/cities/**` |
| `token-no-scopes` | `["unrelated:scope"]` | Only unconfigured routes |

## Route Configuration

The config enforces these scope requirements:

| Route Pattern | Required Scopes |
|---------------|-----------------|
| `/users-info/v1/users` | `users:read` OR `users:admin` |
| `/users-info/v1/users/*` | `users:read` OR `users:admin` |
| `/users-info/v1/cities` | `cities:admin` |
| `/users-info/v1/cities/*` | `cities:admin` |

## Expected Behavior

- **403 Forbidden**: Returned immediately when token scopes don't match required scopes
- **401 Unauthorized**: Returned when no token or invalid token is provided
- **Pass-through**: Routes not in the config are not subject to scope enforcement
