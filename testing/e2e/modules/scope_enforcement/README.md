# Route Policy Enforcement E2E Tests

Tests for the Route Policy Enforcement middleware, which performs coarse-grained
early rejection of requests based on token scopes without calling the PDP.

## Prerequisites

1. Build the server with required features:
   ```bash
   cargo build --release --bin cf-server --features users-info-example,static-authn,static-authz,static-tenants
   ```

2. Start the server with the scope enforcement config:
   ```bash
   cargo run --release --bin cf-server --features users-info-example,static-authn,static-authz,static-tenants \
     -- --config config/e2e-scope-enforcement.yaml
   ```

3. Install Python dependencies:
   ```bash
   pip3 install -r testing/e2e/requirements.txt
   ```

## Running Tests

These tests require the scope enforcement config and are **not** part of the standard CI e2e suite.

```bash
# Set the environment variable to enable scope enforcement tests
export E2E_SCOPE_ENFORCEMENT=1

# Run all scope enforcement tests
python3 -m pytest testing/e2e/modules/scope_enforcement -v

# Run specific test class
python3 -m pytest testing/e2e/modules/scope_enforcement/test_scope_enforcement.py::TestScopeEnforcementDenied -v
```

## Test Tokens

The config defines these test tokens:

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
