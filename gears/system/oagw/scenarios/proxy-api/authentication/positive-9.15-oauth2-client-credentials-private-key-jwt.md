# Outbound auth: OAuth2 client credentials (`private_key_jwt` client)

The client authenticates to the token endpoint with a JWT signed by its
private key (RFC 7523 §2.2, OIDC Core §9 `private_key_jwt`) instead of a
client secret. See [ADR-0016](../../../docs/ADR/0016-oauth2-client-credentials-auth-plugin.md).

## Upstream configuration

```json
{
  "alias": "vendor-jwt.example.com",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "vendor-jwt.example.com", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_private_key_jwt.v1",
    "config": {
      "token_endpoint": "https://<oauth-host>/oauth/token",
      "client_id_ref": "cred://vendor-jwt-client-id",
      "private_key_ref": "cred://vendor-jwt-signing-key",
      "signing_alg": "ES256",
      "kid": "vendor-jwt-2026-11",
      "scopes": "read"
    }
  }
}
```

- `private_key_ref`: a credential store secret holding the private key as an
  unencrypted PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`). Never inline. The
  base64 may be wrapped at any width or sit on one line.
- `signing_alg`: `ES256` (P-256 EC key with the public key embedded, as
  `openssl genpkey` writes it), `RS256` or `PS256` (RSA key, 2048 to
  8192 bits, with the `rsaEncryption` algorithm identifier). An RSA key with
  the `id-RSASSA-PSS` identifier, as `openssl genpkey -algorithm RSA-PSS`
  writes it, is refused, for `PS256` too; `openssl genpkey -algorithm RSA`
  and `openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256` write
  usable keys.
- `kid`: optional; sent in the assertion header so the identity provider can
  pick the registered public key. 1 to 256 characters.
- `audience`: optional; the assertion's `aud` when the identity provider
  expects something other than the token endpoint (its issuer, say). 1 to
  2048 characters. Without it, `aud` is the token endpoint the request goes
  to, the discovered one with `issuer_url`.
- `token_endpoint` or `issuer_url` (exactly one, checked on write),
  `client_id_ref` and `scopes` as for the client-secret variants (9.5, 9.6).

The assertion header carries `alg`, `typ` and `kid` only. `x5t` and
`x5t#S256` (certificate thumbprint) headers are not supported: identity
providers that require one, such as Microsoft Entra ID, cannot use this
plugin yet.

## Scenario A: token fetched with a signed assertion

Invoke the proxy twice:

```http
GET /api/oagw/v1/proxy/vendor-jwt.example.com/v1/resource HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Expected:

- The token request is a form POST with `grant_type=client_credentials`,
  `client_assertion_type=urn:ietf:params:oauth:client-assertion-type:jwt-bearer`
  and `client_assertion`. It carries no `client_secret` and no
  `Authorization` header.
- The assertion is a JWS signed with `signing_alg`, with the configured
  `kid` in its header, verifiable with the registered public key. Claims:
  `iss` = `sub` = the client id, `aud` = the token endpoint (or `audience`,
  see D), `iat`, `exp` (60 s later) and a fresh `jti`.
- The upstream receives `Authorization: Bearer <access_token>`.
- The access token is cached as for 9.5: one token request for the two
  calls. Each token request signs a new assertion.

## Scenario B: unusable key

The secret under `private_key_ref` is not a PKCS#8 PEM private key, or not
of the type `signing_alg` needs (for example an RSA key with `ES256`, or a
P-256 key exported without its public key by `openssl ec -no_public`).

Expected:

- `500` `internal`, `X-OAGW-Error-Source: gateway`; no token request and no
  upstream request.
- The response shows neither the key nor the reference. The server log names
  `auth.config.private_key_ref`, the algorithm and the key type it needs
  (`RSA (rsaEncryption)` or `EC P-256`), never key material.

## Scenario C: invalid configuration refused on write

Create or update the upstream with one of:

| Config | Field violation (`400 invalid_argument`) |
|---|---|
| `client_secret_ref` set as well as `private_key_ref` | `auth.config.client_secret_ref`, `VALIDATION` |
| no `private_key_ref` | `auth.config.private_key_ref`, `REQUIRED` |
| no `signing_alg` | `auth.config.signing_alg`, `REQUIRED` |
| `signing_alg` not one of `ES256`, `RS256`, `PS256` (exact case) | `auth.config.signing_alg`, `INVALID_VALUE` |
| neither `token_endpoint` nor `issuer_url` | `auth.config.token_endpoint`, `REQUIRED` |
| both `token_endpoint` and `issuer_url` | `auth.config.issuer_url`, `VALIDATION` |
| `token_endpoint` or `issuer_url` not an absolute URL | that field, `INVALID_VALUE` |
| `kid` empty, over 256 characters, or with control characters | `auth.config.kid`, `INVALID_VALUE` |
| `audience` empty, over 2048 characters, or with control characters | `auth.config.audience`, `INVALID_VALUE` |

Expected: the write is refused and nothing is stored. A `private_key_ref`
the credential store does not hold is a `400 failed_precondition` naming
`auth.config.private_key_ref` (9.7-B); access and cross-subject isolation of
the key reference are as for the other references (9.7, 9.10).

## Scenario D: configured audience

The identity provider expects its issuer, not the token endpoint, as `aud`.
Add `"audience": "https://<oauth-host>/"` to the config and invoke the proxy.

Expected:

- The assertion's `aud` is `https://<oauth-host>/`, a single string; the
  other claims are as in A.
- Two upstreams whose configs differ only in `audience` do not share a
  cached token: each sends its own token request.
