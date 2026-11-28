# Outbound auth: AWS SigV4 with a streamed request body

A SigV4 signature covers the SHA-256 of the body, which is not known while the body streams. Request bodies stream from REST and in-process SDK callers alike. Upstream as in [positive-9.11](positive-9.11-aws-sigv4-signed-request.md).

## Scenario A: read whole and signed by default

A caller sends `POST .../model/<id>/invoke` with a streamed body (REST, or `Body::Stream` in-process).

Expected:
- The gateway reads the body whole (within `max_body_size_bytes` and the request timeout), hashes it and signs the hash.
- The upstream gets the body with `Content-Length`, whether or not the caller declared one.

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
- With a `Content-Length` the request is signed with the payload hash `UNSIGNED-PAYLOAD`, sent as `X-Amz-Content-Sha256: UNSIGNED-PAYLOAD`, and the body is streamed to the upstream with that length.
- Without a `Content-Length` (a chunked client) the body is still read whole and hashed: sent chunked, S3 would refuse it without `x-amz-decoded-content-length`.
- A buffered body on the same upstream is still hashed: `unsigned_payload` affects only a streamed body.
- Only services that accept `UNSIGNED-PAYLOAD` (for example S3) take such a request.

Covered by Rust tests (`infra/proxy/aws_sigv4_tests.rs`).
