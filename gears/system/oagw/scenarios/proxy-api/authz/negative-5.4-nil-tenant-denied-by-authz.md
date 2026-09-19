# Nil tenant is denied by authz with a clean 403

## Setup

Create an upstream and a route as usual. The caller authenticates with a token whose `subject_tenant_id` claim is the nil UUID (`00000000-0000-0000-0000-000000000000`) rather than an x-tenant-id header — the tenant is extracted from the token by the auth middleware.

## Inbound request

```http
GET /api/oagw/v1/proxy/<alias>/v1/models HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token-with-nil-subject-tenant-id>
```

## Expected response

- `403 Forbidden`
- `Content-Type: application/problem+json`
- `X-OAGW-Error-Source: gateway`
- Body: `status: 403`, `title: "Permission Denied"`, `type: "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~"`

## What to check

- The authz check runs *before* upstream resolution — a nil tenant is denied without ever attempting to look up the upstream/route, so the failure is always a clean `403`, never a `404`/`503` from a later resolution stage.
- This is the `401` vs `403` boundary in practice: `401 AuthenticationFailed` means no resolvable identity at all (missing/invalid token); `403 PermissionDenied` means an identity resolved but was denied (including the degenerate nil-tenant identity). See [PRD.md §5.6](../../../docs/PRD.md#56-error-codes).
