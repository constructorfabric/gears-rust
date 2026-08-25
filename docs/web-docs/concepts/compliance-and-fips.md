---
title: Compliance and FIPS
description: The security baseline Gears provides — FIPS 140-3-ready crypto, supply-chain policy, auditability, and tenant isolation — and where product certification remains your responsibility.
sidebar:
  label: Compliance and FIPS
  order: 9
---

Gears provides a security and compliance **baseline** so regulated products start from a defensible position. It is
important to be precise about what the framework gives you versus what product and process certification still requires.

## FIPS 140-3-ready crypto

Builds can route TLS through a supported FIPS-capable crypto provider with the `fips` feature:

```sh
cargo build --features fips
```

This is available on Linux, macOS, and Windows via the `rustls-corecrypto-provider` and `rustls-fips-shim` crates.
Whether a given build is FIPS-*validated* depends on the platform and the provider's validation status — the framework
provides the routing path, not the certificate. See
the [FIPS probe example](https://github.com/constructorfabric/gears-rust/tree/main/examples/cf-gears-fips-probe).

## Supply-chain policy as code

Dependency risk is made reviewable rather than implicit:

- pinned toolchain (`rust-toolchain.toml`) and committed lockfiles;
- `cargo-deny` for licenses, bans, and advisories (with a separate FIPS deny policy);
- continuous fuzzing of parsers and validation surfaces;
- CI security scans.

## Auditability and isolation

- **Tenant isolation** is enforced at the query layer through `SecureConn` (
  see [Secure data path](../secure-data-path/)), which supports data-residency and isolation requirements.
- **Access trails / audit** — the authorization decisions and usage measurements provide the raw material for access
  trails and audit logs (the dedicated Audit gear is *planned*).

## What Gears gives you vs. what you still own

- **Gears provides**: FIPS-capable crypto routing, secure-by-default data access, tenant isolation, canonical errors,
  supply-chain controls, and the hooks for audit and usage.
- **You still own**: the actual certification (SOC 2, ISO 27001, HIPAA, GDPR processes), your organization's controls
  and evidence, and validating that your specific build and deployment meet the standard you claim.

## See also

- [Secure data path](../secure-data-path/) — the enforcement layers.
- [Deploy Gears](../../build-with-gears/deploy/) — building with `--features fips`.
- [Why Gears](../../introduction/why-gears/) — the security rationale.
