# Token Issuer

System gear that mints short-lived signed platform tokens and publishes the JWKS/discovery surfaces
verifiers use to validate them. Signing itself is delegated to a signing plugin discovered via the
GTS types-registry — this gear holds no private key material.

## Overview

The `cf-gears-token-issuer` gear provides:

- **Capability tokens (`cap`)** — Short-lived ES256 JWTs authorizing a caller for an audience, operation, and resource type, with a get-or-mint reuse cache
- **Grant tokens (`grant+jwt`)** — Data-plane tokens bound to exactly one adapter audience and a closed set of operations, verified offline by adapters
- **OBO tokens** — On-behalf-of re-mint of a presented capability token, down-scoped against the adapter's registry allowlist (feature-gated, off by default)
- **JWKS + discovery** — Public, unversioned identifier surfaces per token class so external verifiers can fetch keys
- **Plugin-based signing** — Selects a `SigningPluginSpecV1` instance by vendor and signs through the scoped plugin client
- **Readiness gating** — Stays not-ready (503) with capped-backoff retry until signing keys are readable and the JWKS is buildable

Gear registration:

```rust
#[toolkit::gear(
    name = "token-issuer",
    deps = [types_registry],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", await_ready)
)]
```

## Architecture

```
Consumers (e.g. grants gear)
    │
    ▼
TokenIssuerClientV1  (SDK trait, registered in ClientHub)
    │
    ▼
token-issuer  (this crate — claims, cache, JWS assembly, JWKS)
    │
    ▼
SigningClientV1  (SDK port → scoped signing plugin, selected by vendor via GTS)
```

Layers follow the DDD-light gear layout:

| Layer | Contents |
|---|---|
| `api/rest` | Route registration (`OperationBuilder`), handlers, DTOs |
| `domain` | Claim assembly, cap/OBO caches, JWS + ES256 assembly, JWKS builder, down-scoping, loop guard, peer identity, cap verification, metrics, error model |
| `infra` | GTS signing-plugin selector, local `ClientHub` client, lazy RMS adapter registry, SDK error mapping |
| `config` | Static gear config with defaults and validated invariants |

## REST Endpoints

All routes are public — JWKS and discovery are published so verifiers can validate minted tokens.
Issuer paths are intentionally unversioned identifier surfaces (like `/livez` / `/readyz`).

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/issuers/cap/jwks.json` | Capability-token JWKS |
| `GET` | `/issuers/cap/.well-known/openid-configuration` | Capability issuer discovery |
| `GET` | `/issuers/grant/jwks.json` | Grant-token JWKS |
| `GET` | `/issuers/grant/.well-known/openid-configuration` | Grant issuer discovery |
| `GET` | `/issuers/obo/jwks.json` | OBO-token JWKS (only when `obo.enabled`) |
| `GET` | `/issuers/obo/.well-known/openid-configuration` | OBO issuer discovery (only when `obo.enabled`) |
| `POST` | `/internal/v1/issuers/obo/tokens` | Re-mint a capability token into a down-scoped OBO token (only when `obo.enabled`) |

The re-mint endpoint is public by design: its authentication is the presented capability token plus
the mTLS peer identity, both verified in-handler and in-domain rather than by a bearer middleware.

## Configuration

```yaml
token-issuer:
  issuer_base_url: "https://core.example.com"   # required, ${VAR}-expanded
  vendor: constructorfabric                     # selects the signing plugin instance
  cap_ttl_secs: 300
  cap_reuse_floor_secs: 150
  obo_ttl_secs: 60
  clock_skew_secs: 30
  cap_key_name: cap-token-sign
  obo_key_name: obo-token-sign
  obo_audience: public-api
  grant_ttl_secs: 300
  grant_key_name: grant-token-sign
  transit_mount: transit
  obo:
    enabled: false
```

Unknown keys are rejected. Validated invariants: `issuer_base_url` must be non-blank,
`clock_skew_secs <= cap_reuse_floor_secs < cap_ttl_secs <= 86400`,
`clock_skew_secs < obo_ttl_secs <= 60`, and `0 < grant_ttl_secs <= 86400`. Explicit grant TTLs
have the same 24-hour issuer ceiling, and expiration arithmetic fails instead of saturating.

Each token class gets its own Transit key — `cap`, `obo`, and `grant` keys are never shared. The gear
registers a composite REST healthcheck, so `/readyz` remains unhealthy until all required JWKS
documents are warm.

## Related Crates

- `cf-gears-token-issuer-sdk` — public API traits, models, errors, and the signing-plugin GTS schema
- `cf-gears-types-registry` / `-sdk` — GTS instance discovery for signing-plugin selection

## License

Apache-2.0
