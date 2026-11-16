# Handle preflight without capacity (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{
  "cors": {
    "allowed_origins": [
      "https://consumer.example"
    ],
    "allowed_methods": [
      "POST"
    ],
    "allowed_headers": [
      "content-type",
      "x-oagw-credential"
    ]
  }
}
```

## Inbound request

```http
OPTIONS /oagw/v1/proxy/api.example.com/v1/chat/completions HTTP/1.1
Authorization: Bearer <tenant-token>
Origin: https://consumer.example
Access-Control-Request-Method: POST
Access-Control-Request-Headers: content-type,x-oagw-credential
X-OAGW-Credential: research
```

## Expected response

Existing successful preflight status and CORS headers. Validate the resource response or operation result described below.

## What to check

- No CredStore read, debit or provider call. A subsequent POST still has research's initial token.
- Repeat without selector; retain existing CORS behavior.
