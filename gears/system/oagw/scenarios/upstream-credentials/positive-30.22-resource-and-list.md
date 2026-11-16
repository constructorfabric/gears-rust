# Read a complete credential resource (planned)

## Setup

Apply [shared setup](README.md#shared-setup), then these fixture changes:

```json
{}
```

## Inbound request

```http
GET /oagw/v1/upstreams/<upstream-id>/credentials/research HTTP/1.1
Authorization: Bearer <tenant-token>
```

## Expected response

200 OK. Validate the resource response or operation result described below.

## What to check

- Validate the response against definitions.response: GTS credential/parent IDs, tenant UUID, name, reference, enabled, suspended, managed_by, approved endpoints, tags and UTC timestamps.
- List validates against definitions.list with pagination. POST/PUT/PATCH return the same shape.
- No secret values, pin, version, generation or registry source UUID in any response. Reject attempts to write internal fields.
- SDK CRUD uses UUIDs and matching typed errors; new preview request returns safe IDs. Existing V1 implementations still compile in phase 2.
