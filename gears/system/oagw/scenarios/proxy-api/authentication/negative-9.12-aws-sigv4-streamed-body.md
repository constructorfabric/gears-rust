# Outbound auth: AWS SigV4 with a streamed request body

A SigV4 signature covers the SHA-256 of the body, which is not known for a streamed body. Only in-process SDK callers can stream a request body; REST callers are always buffered. Upstream as in [positive-9.11](positive-9.11-aws-sigv4-signed-request.md).

## Scenario A: refused by default

An in-process caller sends `POST .../model/<id>/invoke` with `Body::Stream`.

Expected:
- `400` `invalid_argument`, field violation `body`, reason `VALIDATION`, description naming `auth.config.unsigned_payload`.
- Nothing is sent to the upstream and no credential is read.

## Scenario B: `unsigned_payload` opt-in

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1",
    "config": {
      "region": "us-east-1",
      "service": "s3",
      "access_key_id_ref": "cred://aws-access-key-id",
      "secret_access_key_ref": "cred://aws-secret-access-key",
      "unsigned_payload": "true"
    }
  }
}
```

Expected:
- The request is signed with the payload hash `UNSIGNED-PAYLOAD`, sent as `X-Amz-Content-Sha256: UNSIGNED-PAYLOAD`, and the body is streamed to the upstream.
- A buffered body on the same upstream is still hashed: `unsigned_payload` affects only a streamed body.
- Only services that accept `UNSIGNED-PAYLOAD` (for example S3) take such a request.

Covered by Rust tests (`infra/proxy/aws_sigv4_tests.rs`); the E2E suite has no in-process caller.
