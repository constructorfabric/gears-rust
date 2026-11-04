# cf-gears-rustls-corecrypto-provider

A `rustls::crypto::CryptoProvider` backed by Apple **corecrypto** (the FIPS
140-3 validated cryptographic module shipped inside macOS) via
`Security.framework` and `CommonCrypto`.

## Why

Apple's `corecrypto` user-space module carries its own FIPS 140-3 certificates
per macOS release. Routing rustls through it gives a macOS-valid FIPS claim
without leaving the rustls ecosystem — same TLS state machine, same
`HttpsConnector` type, same APIs as on Linux (aws-lc-rs FIPS) or Windows
(future `rustls-cng-crypto`).

This crate **only compiles on macOS** (`cfg(target_os = "macos")`); on other
platforms the public API is empty.

## Scope

| Category | Algorithms |
|---|---|
| TLS 1.3 | `TLS_AES_128_GCM_SHA256`, `TLS_AES_256_GCM_SHA384` |
| TLS 1.2 | `ECDHE_ECDSA/RSA_WITH_AES_128/256_GCM_SHA256/384` |
| Key exchange | NIST P-256, P-384 |
| Signature verify | ECDSA P-256/P-384/P-521, RSA-PSS, RSA PKCS#1 v1.5 (SHA-256/384/512) |
| Signature sign (server / mTLS) | ECDSA P-256/P-384/P-521, RSA-PSS, RSA PKCS#1 v1.5 (SHA-256/384/512) — same scope as verify, see [ADR 0004](../../docs/security/fips/adrs/0004-macos-server-side-tls-via-corecrypto.md) |
| Hash / HMAC / HKDF | SHA-256, SHA-384 |
| AEAD | AES-128-GCM, AES-256-GCM |
| Random | `SecRandomCopyBytes` |

Out of scope: CBC ciphers, X25519, ChaCha20-Poly1305, ED25519,
Keychain-stored / Secure-Enclave private keys (the current
`load_private_key` path imports from in-memory PKCS#1 / PKCS#8 / SEC1 DER
bytes — Keychain-store integration is a documented future opt-in in
ADR 0004). **Minimum macOS version for the FIPS witness: 12 (Monterey)**
— the OE whitelist in [`oe.rs`](src/oe.rs) covers macOS 12, 13, 14, 15;
the crate compiles and runs on older macOS but `CryptoProvider::fips()`
will return `false` on those hosts (no panic — see "Runtime FIPS witness"
below).

## Usage

This crate ships **two provider factories** AND a Cargo `fips` feature
flag — pick the entry point that matches your deployment.

```rust
use rustls_corecrypto_provider::{default_provider, fips_provider};

// General-purpose (TLS 1.2 + TLS 1.3, max peer interop).
// CryptoProvider::fips() = false because TLS 1.2 PRF is not CAVS-validated
// on macOS (see "FIPS claim boundaries" below).
let p = default_provider();

// Explicit FIPS-claim (TLS 1.3 only). Available in any build profile.
// CryptoProvider::fips() = true ONLY when the runtime FIPS witness
// agrees — i.e. the running macOS major is inside the active corecrypto
// CMVP cert OE (see "Runtime FIPS witness" below). On unsupported
// macOS the witness reports false and so does fips() — the provider
// remains usable without a FIPS claim.
let p = fips_provider();
```

### `feature = "fips"` — single-entry-point pattern

Mirrors `rustls-cng-crypto`'s flag: when compiled with `--features fips`,
`default_provider()` is **aliased** to `fips_provider()`. Downstream
crates that already have their own `fips` feature can forward it
unconditionally:

```toml
# in downstream Cargo.toml
[features]
fips = ["rustls-corecrypto-provider/fips", ...]
```

…and write build-mode-agnostic code:

```rust
// Always calls the same factory; under `--features fips` it returns the
// TLS-1.3-only FIPS-claim variant automatically.
let p = rustls_corecrypto_provider::default_provider();
```

The two-factory API (`default_provider` / `fips_provider`) remains
available for callers that want to switch at runtime rather than at
compile time. `fips_provider()` always returns the TLS-1.3-only set
regardless of feature flags.

Server-side `rustls::ServerConfig`:

```rust
let mut cfg = rustls::ServerConfig::builder_with_provider(fips_provider().into())
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(cert_chain, key_der)?;
cfg.require_ems = true; // SP 800-52 Rev. 2 §3.5 (for completeness)
assert!(cfg.fips()); // true on fips_provider() + TLS 1.3-only + EMS
```

## FIPS claim boundaries

Two distinctions matter and the crate enforces both honestly:

1. **Component-level FIPS** (every crypto primitive routes through Apple
   corecrypto, the CMVP-validated module).
2. **Composite-level FIPS** (the cipher suite as a whole). For TLS 1.2,
   the PRF is a generic HMAC P_hash composition and macOS does **not**
   ship a separately CAVS-validated TLS-PRF primitive. (Compare
   `aws-lc-fips`, which has `tls_prf::Algorithm` listed in its CAVS
   scope.) So our TLS 1.2 cipher suites' `Tls12CipherSuite::fips()`
   returns `false`. TLS 1.3 cipher suites are full-FIPS because HKDF
   over a validated HMAC is itself an Approved KDF (NIST SP 800-56C).

| Provider | TLS 1.2 included | `CryptoProvider::fips()` | Intended use |
|---|---|---|---|
| `default_provider()` | yes | `false` (TLS 1.2 PRF gap) | General outbound TLS, peer-interop fallback |
| `fips_provider()` | no | runtime witness — `true` iff macOS major ∈ [`SUPPORTED_OE_MACOS_MAJOR`] | Contractual FIPS deployments, server-side TLS termination, mTLS |

A FIPS 140-3 claim under either path further rests on the running
macOS version + arch being inside the Operational Environment of the
**current** Apple corecrypto CMVP certificate. Verify before each
release — see "Open questions / TODO" below.

### Runtime FIPS witness

Every `fips()` impl in this crate delegates to a single function,
[`oe::fips_witness_ok()`](src/oe.rs). It returns `true` iff (a) the
running macOS major is inside [`oe::SUPPORTED_OE_MACOS_MAJOR`] (today:
`[12, 13, 14, 15]`), or (b) the override env-var
[`oe::OE_OVERRIDE_ENV`] (`CF_GEARS_FIPS_OE_OVERRIDE`) is set. On any
other macOS version `fips_witness_ok()` returns `false` and so does
every `fips()` impl in the crate; downstream `ClientConfig::fips()` /
`ServerConfig::fips()` then honestly reflect the runtime witness rather
than a design-intent claim.

This mirrors `rustls-cng-crypto`'s pattern on Windows, where
`fips::enabled()` consults `BCryptGetFipsAlgorithmMode` and every
`fips()` method delegates to it. The two key consequences:

- **No startup panic** on OE failure. Earlier versions of this crate
  panicked under `--features fips` if the running macOS major was
  outside the whitelist. That panic has been removed: an OE mismatch
  now surfaces as `CryptoProvider::fips() == false`, plus a single
  `tracing::warn!` on the first call. The provider remains usable —
  just without a FIPS claim. This makes the crate suitable for
  non-FIPS deployments too.
- **Cached.** The witness is computed once per process via
  `std::sync::OnceLock<bool>`. One `sysctlbyname` call total.

Override semantics: `CF_GEARS_FIPS_OE_OVERRIDE=1` forces the witness
to `true` on a macOS version that is *not* in the whitelist. Intended
for CI on pre-release macOS while Apple's next corecrypto CMVP cert is
pending. **Must never be set in production.**

### TLS 1.3 signature-scheme filtering

`WebPkiSupportedAlgorithms.all` lists both RSA-PSS and RSA-PKCS#1 v1.5
entries. RFC 8446 §4.2.3 forbids PKCS#1 v1.5 for the TLS 1.3
`CertificateVerify` message; rustls itself enforces this in
`rustls::crypto::verify_tls13_signature` (it filters the `all` list
against the TLS 1.3 admissible-schemes set). The PKCS#1 v1.5 entries
in our list exist only for (a) the TLS 1.2 path, where they are still
in widespread use, and (b) webpki certificate-chain validation where
PKCS#1 v1.5 issuer signatures remain common in PKI today. The
integration test `tls13_pkcs1_v1_5_certificate_verify_is_rejected`
(in `tests/handshake_smoke.rs`) pins this contract.

Runtime OE-validation is currently macOS-only; Linux + Windows OE
coverage is verified manually per release (FIPS PRD §9 acceptance
criteria; FIPS PRD §13 TODO-8). Rationale and trade-off are documented
in [FIPS PRD §5.5](../../docs/security/fips/PRD.md#55-runtime-operational-environment-validation).
Verify the cert coverage for your target macOS version at
<https://csrc.nist.gov/projects/cryptographic-module-validation-program/validated-modules/search>
before each release.

### EC key constraint — embedded publicKey required

Apple's `SecKeyCreateWithData` for EC requires an ANSI X9.63 blob
`0x04 || X || Y || k` — uncompressed public point followed by the private
scalar. We do **not** derive `Q = d · G` from the scalar in Rust code —
that would be an EC scalar-multiplication on the private key performed
outside Apple's FIPS-validated corecrypto module, violating the FIPS
140-3 cryptographic boundary. Instead we read the public point from the
SEC1 `EcPrivateKey.publicKey` OPTIONAL field (RFC 5915 §3).

In practice every standard tool emits `publicKey`:

- `rcgen` — yes, always.
- `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256` — yes.
- `openssl ecparam -genkey -name prime256v1` — yes.
- GnuTLS `certtool --generate-privkey --ecc` — yes.

A SEC1 produced via `openssl ec -no_public` (or any other tool that
strips `publicKey`) will be **rejected** at load time with an error
referencing the FIPS-boundary rationale; re-export the key with the
public point embedded.

## Compliance caveat

A FIPS 140-3 claim under this provider rests on the Apple corecrypto cert
covering the **exact running macOS version + arch**. Apple submits a new
corecrypto-module CMVP package for each major macOS release; certificates
listed at time of writing include (search by "Apple corecrypto Gear" at
the CMVP search URL below):

| Apple module version | macOS release | CMVP cert(s) (Intel, Apple silicon — User+Kernel) |
|---|---|---|
| v12.x | macOS 12 (Monterey) | **TBD — verify at CMVP search before relying** |
| v13.0 | macOS 13 (Ventura) | **TBD — verify at CMVP search before relying** |
| v14.0 / v14.1 | macOS 14 (Sonoma) | **TBD — verify at CMVP search before relying** |
| v18.3 | macOS 15 (Sequoia) | **TBD — verify at CMVP search before relying** |

> Cert numbers intentionally not pinned in this table: Apple publishes /
> updates the macOS-corecrypto CMVP certificates on its own cadence, and a
> snapshot copied at PR-time goes stale within weeks. The release runbook
> (see FIPS PRD §9.3 / §10 TODO-7) MUST verify the exact mapping at the
> [CMVP search](https://csrc.nist.gov/projects/cryptographic-module-validation-program/validated-modules/search)
> before each release. The
> [`SUPPORTED_OE_MACOS_MAJOR`](src/oe.rs) whitelist gates the runtime
> witness and is the authoritative source of macOS-major support; this
> table exists only to remind maintainers where to look.

The table is a **snapshot at PR time** and not authoritative — verify the
exact mapping for your release at
<https://csrc.nist.gov/projects/cryptographic-module-validation-program/validated-modules/search>
before relying on the claim.

## Open questions / TODO

These are deliberately deferred design decisions tracked here so future
maintainers don't lose context. Each is "not blocking" for the current
production use case (outbound TLS + server-side TLS termination with PEM
keys from disk) but **does** need owner-assignment before strict-CMVP
audit.

### TODO-1 — Keychain-stored private keys (`SecItemCopyMatching`)

Today's `KeyProvider::load_private_key` accepts PEM/DER input via rustls's
`PrivateKeyDer`. The private bytes therefore transit user-space heap before
`SecKeyCreateWithData` absorbs them into the corecrypto module — same
posture as `rustls-cng-crypto` on Windows and `aws-lc-rs/fips`'s
PEM-loading path on Linux. Strict-FIPS auditors operating under the
"plaintext CSPs MUST NOT leave the boundary" reading will require a
Keychain-stored key flow: `SecItemCopyMatching` to retrieve an opaque
`SecKeyRef`, then construct `SigningKey` directly around it without ever
materialising private bytes in our process. **Owner**: TBD. Documented
target: post-1.0 release of this crate.

### TODO-2 — Server-side keys from file only

The current production path reads server-side TLS private keys from PEM
files at startup. This is acceptable for development and most production
deployments where filesystem permissions guard the key, but it is the
weaker posture relative to TODO-1. Migration to Keychain-stored keys
should happen together with TODO-1 — they are the same enhancement
viewed from two angles (the key-source-of-truth angle vs. the
FIPS-boundary angle).

### TODO-3 — Cross-implementation interop tests

Current `tests/handshake_smoke.rs` tests our provider against either
`openssl s_server` (client-side) or a rustls server using our own
provider (server-side). We do **not** yet test our provider against a
rustls peer using `aws-lc-rs` or `rustls-cng-crypto` as its
`CryptoProvider`. Without these tests we have no automated guarantee
that signatures our `SigningKey` produces verify under another
implementation's verify path, or vice versa. The right form is an
integration test with both providers compiled in, exchanging a real TLS
handshake over a `TcpListener` — likely needing `testcontainers` or
a multi-feature workspace setup. **Owner**: TBD. Acceptance criteria:
six handshake cases (TLS 1.2 + TLS 1.3 × {RSA, ECDSA P-256, P-384}),
all green, with the corecrypto provider on one side and `aws_lc_rs` on
the other.

### TODO-4 — Verified PSS salt length contract

**Status: resolved (informational).** Apple's
`SecKey.h` (macOS SDK, `Security.framework`)
documents `kSecKeyAlgorithmRSASignatureMessagePSSSHA{256,384,512}` with
`saltLength = 32/48/64` (digest length), matching RFC 8446 §4.2.3 and
RFC 8017 §9.1. Keeping this entry as a regression-watch trigger — if a
future Apple SDK alters the documented salt-length, our TLS-peer
interop and FIPS-claim correctness depend on it.

### TODO-5 — Dedicated CAVS-validated TLS PRF primitive on macOS

`aws-lc-fips` ships `tls_prf::Algorithm` as a separately CAVS-listed
"TlsKdfPrf" primitive (NIST SP 800-135 Component Validation List). Apple
corecrypto, by contrast, exposes only generic HMAC/hash primitives via
CommonCrypto — there is no `CC_TLSPrf` API. Our `PrfSha{256,384}` is
therefore a `PrfUsingHmac` composition with `fips()` honestly returning
`false`. If Apple publishes a dedicated TLS PRF API in a future macOS
release (or via private SPI), revisit and override `fips()`. Until
then, `default_provider()` cannot claim FIPS for TLS 1.2 paths;
`fips_provider()` solves this by excluding TLS 1.2 entirely.
