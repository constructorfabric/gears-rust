# Token Issuer SDK

SDK crate for the Token Issuer gear, providing the public API contracts for minting and signing short-lived platform tokens in Gears.

## Overview

This crate defines the transport-agnostic interface for the Token Issuer gear:

- **`TokenIssuerClientV1`** — Async trait for consumers (mint capability tokens, mint data-plane grant tokens)
- **`SigningClientV1`** — Async signing port implemented by the backing signing plugin (e.g. an `OpenBao` Transit plugin)
- **`CapabilityClaims`** / **`GrantClaims`** — Claim sets carried inside the minted `cap` and `grant+jwt` tokens
- **`MintCapabilityRequest`** / **`MintGrantRequest`** — Mint inputs
- **`SigningKeyRef`**, **`SigAlg`**, **`SignatureResult`**, **`PublicKeyVersion`** — Signing value types
- **`TokenIssuerError`** / **`SigningError`** — Error types for mint and signing failures
- **`SigningPluginSpecV1`** — GTS schema for signing-plugin registration

Signing keys are platform-scoped; tenant context travels via the `SecurityContext`, never in the key reference.

## Usage

### Getting the Client

Consumers obtain the client from `ClientHub`:

```rust
use token_issuer_sdk::TokenIssuerClientV1;

let issuer = hub.get::<dyn TokenIssuerClientV1>()?;
```

### Minting a Capability Token

```rust
use token_issuer_sdk::MintCapabilityRequest;

let token = issuer
    .mint_capability(
        &ctx,
        MintCapabilityRequest {
            context_tenant: tenant_id,
            context_project_id: None,
            audience: "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1".to_owned(),
            operation: Some("read".to_owned()),
            resource_type: Some("bucket".to_owned()),
        },
    )
    .await?;
```

### Minting a Grant Token

`mint_grant` returns the compact `grant+jwt` together with its absolute expiry, so the caller can
populate an issuance response's `expires_at` without re-decoding the JWT.

```rust
let grant = issuer.mint_grant(&ctx, req).await?;
println!("{} expires at {}", grant.token, grant.expires_at);
```

### Implementing the Signing Port

A signing plugin implements `SigningClientV1` and registers itself under the
`SigningPluginSpecV1` GTS type chain
(`cf.toolkit.plugins.plugin.v1~cf.core.token_issuer.signing_plugin.v1~`). The gear selects the
instance by configured vendor and calls `sign` / `public_keys` through the scoped plugin client.

```rust
#[async_trait]
impl SigningClientV1 for MySigner {
    async fn sign(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
        signing_input: &[u8],
    ) -> Result<SignatureResult, SigningError> { /* ... */ }

    async fn public_keys(
        &self,
        ctx: &SecurityContext,
        key: &SigningKeyRef,
    ) -> Result<Vec<PublicKeyVersion>, SigningError> { /* ... */ }
}
```

## Key Validation

`SigningKeyRef` is a validated newtype — `[a-z0-9-]+`, 1 to 64 characters — enforced both in
`SigningKeyRef::new` and on deserialization, so a malformed key name can never reach a signing
backend.

## Related Crates

- `cf-gears-token-issuer` — the gear that implements this SDK
- `cf-gears-types-registry-sdk` — GTS instance discovery used to select the signing plugin

## License

Apache-2.0
