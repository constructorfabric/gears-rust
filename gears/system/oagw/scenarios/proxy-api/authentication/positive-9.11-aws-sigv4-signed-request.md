# Outbound auth: AWS SigV4 signed request (Bedrock)

The `aws_sigv4` plugin signs the request with AWS Signature Version 4 in the signing phase ([ADR-0023](../../../docs/ADR/0023-request-signing.md)), on the request as the upstream receives it. Config keys: [F0003](../../../docs/features/0003-cpt-cf-oagw-feature-plugin-system.md#aws-sigv4-auth-plugin).

## Upstream configuration

```json
{
  "alias": "bedrock-runtime.us-east-1.amazonaws.com",
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "bedrock-runtime.us-east-1.amazonaws.com", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1",
    "config": {
      "region": "us-east-1",
      "service": "bedrock",
      "access_key_id_ref": "cred://aws-bedrock-access-key-id",
      "secret_access_key_ref": "cred://aws-bedrock-secret-access-key",
      "session_token_ref": "cred://aws-bedrock-session-token"
    }
  }
}
```

`session_token_ref` is optional (temporary credentials only). Bedrock signs as `bedrock` also on `bedrock-runtime` hosts.

## Scenario A: the request is signed as sent

```http
POST /api/oagw/v1/proxy/bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-v2/invoke HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{"prompt":"\n\nHuman: hello\n\nAssistant:","max_tokens_to_sample":64}
```

Expected outbound request:

```http
POST /model/anthropic.claude-v2/invoke HTTP/1.1
Host: bedrock-runtime.us-east-1.amazonaws.com
Content-Type: application/json
X-Amz-Date: <YYYYMMDDTHHMMSSZ>
X-Amz-Content-Sha256: <hex SHA-256 of the body>
X-Amz-Security-Token: <session token>
Authorization: AWS4-HMAC-SHA256 Credential=<access key id>/<YYYYMMDD>/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature=<hex>
```

What to check:
- The signature verifies at the upstream (E2E: the mock's `/aws/*` verifier, written independently with Python `hmac`/`hashlib`).
- Signed headers: `host`, every `x-amz-*`, `content-type`, `content-md5`; never `cookie`, `content-length`, `user-agent` or hop-by-hop headers.
- The path is encoded twice in the canonical request for every service but the S3 signing names `s3`, `s3-object-lambda`, `s3-outposts` and `s3express` (`uri_encoding`, overridable).
- A query is sent in SigV4 encoding, exactly as signed: `?q=a+b&q=c%2Bd&e=` from the client (or added by a transform) goes out as `?q=a%20b&q=c%2Bd&e=`. Repeated names, their order and empty values are kept.
- On a non-default port the signed `Host` carries the port.
- A streamed response comes back as usual: only the request is signed.
- Neither the secret key, the derived signing key nor the session token appear in logs or errors, on success or failure. The references do not appear either.

## Scenario B: wrong secret

`secret_access_key_ref` names a secret that is not the access key's secret.

Expected:
- The gateway signs and sends the request; the upstream refuses the signature with `403` (AWS: `SignatureDoesNotMatch`).
- The `403` is passed through with `X-OAGW-Error-Source: upstream`.

## Scenario C: configuration errors

On create or update of the upstream:
- `region`, `service`, `access_key_id_ref` or `secret_access_key_ref` missing: `400` `invalid_argument`, one field violation on `auth.config.<key>`, reason `REQUIRED`, for example `auth.config.region: required by the AWS SigV4 auth plugin`.
- `region` or `service` empty, over 64 bytes or not ASCII letters, digits, `-`, `_` and `.`; `unsigned_payload` or `content_sha256_header` not `true`/`false`; `uri_encoding` not `single`/`double`: `400`, reason `INVALID_VALUE`.
- A reference that does not resolve for the caller (the optional `session_token_ref` included): `400` `failed_precondition`, violation `type` = `STATE`, `subject` = `auth.config.<key>`. The reference is not quoted.

The plugin checks its config again on every request: a row that lacks a required key fails at proxy time with `500` `internal` (a plugin misconfiguration), before anything is sent; the server log names the key.

## Scenario D: credential failures at proxy time

The references are read on every signed request, with the mapping every auth plugin uses (DESIGN "Credential Failures"); nothing is sent to the upstream:
- A referenced secret that is missing or not accessible to the caller's tenant: `401` `unauthenticated`, reason `AUTH_CREDENTIAL_NOT_FOUND`, naming `auth.config.<key>`, not the reference.
- The credential store unavailable: `503` `service_unavailable`, reason `CREDENTIAL_STORE_UNAVAILABLE`, `Retry-After` from the store's hint (default 5 s).
- A stored access key id that is not 1 to 128 ASCII letters and digits, an empty secret key, or a session token that is not visible ASCII: `401` `unauthenticated`, reason `AUTH_PLUGIN_INTERNAL`. The value is not quoted.
