---
cf: true
type: project-rule
topic: security
generated-by: auto-config
version: 1.0
---
# Security

<!-- toc -->

- [Security Baseline](#security-baseline)
- [FIPS and Secrets](#fips-and-secrets)

<!-- /toc -->

Use this when writing security-sensitive code, tenant-scoped data access, credentials, or FIPS behavior.

## Security Baseline
- Keep `unsafe_code = "forbid"` at workspace policy unless an explicit reviewed exception exists. Evidence: `Cargo.toml:101-104`
- Use Secure ORM patterns for tenant-scoped data access. Evidence: `docs/security/SECURITY.md:69-99`
- Use `SecurityContext` as the request/operation security boundary. Evidence: `docs/security/SECURITY.md:100-142`
- Enforce dependency governance through `cargo deny` policy. Evidence: `deny.toml:20-153`

## FIPS and Secrets
- Preserve platform-specific FIPS dependency and runtime checks. Evidence: `docs/security/SECURITY.md:381-496`
- Keep credential handling isolated through credentials storage/plugin boundaries. Evidence: `docs/security/SECURITY.md:237-272`
