# ToolKit Security

Security context and authorization primitives for Gears / ToolKit.

## Overview

The `cf-gears-toolkit-security` crate provides:

- `SecurityContext`
- `AccessScope`
- Permission / policy engine interfaces
- Binary codec helpers for encoding/decoding security context
- Application cryptography through `toolkit_security::crypto`

## Application cryptography

Use `crypto::hash::Sha256`, `crypto::mac::HmacSha256`, `crypto::aead::Aes256Gcm`,
`crypto::rng::fill`, `crypto::constant_time_eq` and `crypto::signature` instead of
backend crates.
The optional `jwt` feature provides `jwt::decode`, preserving jsonwebtoken
claim validation after verifying signatures through the facade, and `jwt::encode`,
which signs `ES256` tokens with an `EcdsaP256KeyPair` without constructing a
jsonwebtoken signer. The explicit TLS adapter is `tls::configured_provider`.

Ordinary builds use AWS-LC. With `fips`, Linux uses AWS-LC FIPS, macOS uses the
existing corecrypto provider and Windows uses CNG. `toolkit/fips` and
`toolkit-http/fips` forward this feature. Standalone consumers enable
`toolkit-security/fips` explicitly. Rust features unify across the dependency graph.
Call `crypto::initialize()` at startup to handle unavailable providers before
processing requests; infallible hash methods panic when backend policy cannot be met.

ECDSA P-256/SHA-256 key generation, import, signing and verification use the selected
backend. Its API accepts PKCS#8 DER or unencrypted PKCS#8 PEM keys with embedded
uncompressed SEC1 public points, and emits ASN.1 DER signatures (`sign`) or the
fixed-width JOSE `r || s` form (`sign_fixed`); `signature::der_to_fixed` and
`signature::fixed_to_der` convert between the two for P-256 and P-384 widths.
Verification also supports P-384/SHA-384.

`aead::Aes256Gcm` seals `ciphertext || tag` with associated data under a 12-byte nonce
the module draws and returns (FIPS 140-3 IG C.H); opening takes the stored nonce. The
layout matches the RustCrypto `aes-gcm` crate. It routes to AWS-LC only: FIPS builds
on macOS and Windows have no AEAD route and report `UnsupportedAlgorithm` at key import.

HMAC FIPS keys must be at least 14 bytes. HMAC, AEAD and RSA/ECDSA messages are limited
to `u32::MAX` bytes for portable CNG dispatch; SHA-256 and RNG split larger buffers.
Facade-owned HMAC keys/normalization buffers are zeroized. PKCS#8 import transfers
a private-key copy to rustls, and native key export uses OS-owned buffers; the
facade cannot guarantee erasure of these copies or other backend intermediates.

A provider's `fips()` status is a routing/runtime witness, not deployment certification.
Follow the module certificate's operating-environment and security-policy requirements.
macOS has an existing runtime OS-version check; Linux/Windows operating-environment
coverage still requires release validation, and Windows must enable system FIPS policy.

## License

Licensed under Apache-2.0.
