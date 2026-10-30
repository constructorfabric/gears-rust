# Registry-managed resources are read-only

An upstream or route provisioned from the types registry changes only with its registry instance, at the next boot. The Management API cannot update or delete it.

## Setup

The registry upstream and route of [19.1](positive-19.1-registry-instances-provisioned-at-startup.md).

## Update or delete through the API

```http
DELETE /api/oagw/v1/upstreams/gts.cf.core.oagw.upstream.v1~5e0a1900-0000-4000-8000-000000000001 HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
```

Also: a valid `PUT` of the upstream, and `PUT` and `DELETE` of the route.

## Expected response

- `400 Bad Request`, `application/problem+json`, `type` = `...failed_precondition...`.
- `context.violations` = one violation with `type` = `REGISTRY_MANAGED` and `subject` = `managed_by`.
- `context.resource_type` names the upstream or the route.
- Afterwards both resources are unchanged and the proxy still reaches the upstream.

## Routes created through the API on a registry upstream

- `POST /routes` with the registry upstream's `upstream_id` returns `201`. The route is API-managed and proxies normally.
- `DELETE` of that route returns `204`, and the registry route stays.
