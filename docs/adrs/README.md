# Cross-cutting ADRs

ADRs that span the ToolKit libraries or the whole repository.

## ToolKit

| ADR | Title | Status |
|-----|-------|--------|
| [0001](toolkit/0001-toolkit-hyper-tower-http-client.md) | Build a first-party HTTP client stack for ToolKit instead of using reqwest directly | accepted |
| [0002](toolkit/0002-toolkit-auth-oauth2-client.md) | Outbound OAuth2 Client Credentials for ToolKit gears | not set (implemented, see its Implementation Status section) |
| [0003](toolkit/0003-toolkit-universal-lazy-layer.md) | Universal Lazy Typed REST Clients for OoP Gears | superseded |
| [0004](toolkit/0004-toolkit-http-proxy-engine.md) | Build an in-house upstream proxy engine on hyper instead of using Pingora | accepted |
| [0005](toolkit/0005-toolkit-client-circuit-breaker.md) | Own client-side circuit breaker in `toolkit-resilience`, counted per attempt below retry | accepted |

## Tests

| ADR | Title | Status |
|-----|-------|--------|
| [0001](tests/0001-tests-in-separate-files.md) | Enforce test code in separate files via architecture lint DE1101 | accepted |

Other ADRs live next to the design they belong to: `docs/arch/<area>/ADR/` for platform areas, `docs/security/fips/adrs/` for the FIPS build, and `docs/ADR/` inside each gear (e.g. `gears/credstore/docs/ADR/`, `gears/system/oagw/docs/ADR/`).
