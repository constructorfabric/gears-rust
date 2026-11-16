# Refuse a private shadow or recreated record (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "private_secret": {
    "reference": "provider-research",
    "record_id": "44444444-4444-4444-8444-444444444444",
    "value": "private-key"
  }
}
```

## Inbound request

```http
POST /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Content-Type: application/json
X-OAGW-Credential: research

{"messages":[]}
```

## Expected response

```http
HTTP/1.1 403 Forbidden
Content-Type: application/problem+json
X-OAGW-Error-Source: gateway

{
  "status": 403,
  "error_domain": "oagw.credential.v1",
  "error_code": "CREDENTIAL_PIN_MISMATCH"
}
```

## What to check

- Exactly one caller-context CredStore read; the mock receives no request. Admission debits remain.
- Preview succeeds without certifying the pin or reading the secret.
- A caller resolving the pinned record succeeds. Rotating its value changes the forwarded value without changing its ID, generation or name.
- Delete/recreate the secret record under the same reference: use and same-reference PUT/registry replay cannot repin. Deliberately recreate the credential to adopt it.
- Race two PUTs from reference X to Y resolved to different records: after the first commits Y, the second must not replace Y's pin.
