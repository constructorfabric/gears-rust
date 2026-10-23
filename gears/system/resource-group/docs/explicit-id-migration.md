# Group identifier ownership migration

Ordinary group creation now rejects every caller-supplied `id` with canonical
`InvalidArgument` (HTTP 400). This applies to REST and `ResourceGroupClient`,
regardless of group type or whether `tenant_id` is omitted, the caller's tenant,
or another tenant. Omit `id` to receive a server-generated UUIDv7.

This intentionally tightens the existing v1 security boundary: retaining an
unrestricted legacy endpoint alongside a safer version would leave the global
identifier space vulnerable. The request field remains recognizable so old
clients receive an explicit error rather than silently creating a different ID.
The gear takes a pre-1.0 breaking release (0.5.0) and the SDK 0.4.0 documents the
changed creation contract and adds the replacement capability.

## Deterministic tenant defaults

Resolve `ResourceGroupDefaults` from `ClientHub` and call
`ensure_default_group(&ctx, EnsureDefaultGroupRequest { code, metadata })`.
The operation is authenticated and uses the existing group `create` permission;
confirming an existing group also requires `get`. It accepts no UUID, arbitrary
seed string, name, parent, or target tenant. The context's tenant determines the
namespace, and the server fixes the name to `default` and placement to root.
Tenant-typed groups are rejected because they open a new tenant scope.

The SDK helper `default_group::default_group_id(tenant_id)` preserves the existing
identity formula, `UUIDv5(tenant_id, "resource-group:default")`. Consumers that
need to calculate references without I/O should use this helper. The helper
itself grants no authorization.

The ensure operation only returns an existing row if its tenant, type, name,
and root placement match. It never overwrites metadata or repairs name drift.
Concurrent creators with the same identity converge after a primary-key
conflict; other failures propagate. A foreign or mismatched existing row returns
`AlreadyExists` without disclosing its owner or adopting it. There is one default
identity per tenant across group types, not one per type. Selecting the intended
type remains the application's responsibility.

## Consumer rollout

Upgrade the SDK and implementation together. In VHP Core, migrate IRM's placement
adapter/resolver and startup/backfill callers from `create_group` with an explicit
ID to `ResourceGroupDefaults` in the same change that advances the gears-rust
pin. Preserve IRM's existing system-actor authorization, tenant clamp, identity
checks, and failure reporting. Map an unresolved ensure conflict to its existing
fail-closed unavailable behavior and retain the squat-suspected detection counter.
The new capability is an in-process SDK surface; no unprotected REST bootstrap
endpoint is introduced.

Ordinary clients must stop sending IDs. Other deterministic/import consumers
need an explicitly authorized, ownership-validating operation before upgrading;
do not emulate the old behavior by adding an unrestricted public seeding method.

Trusted deployment configuration still uses `GroupSeedDef` and the existing
unscoped service path during initialization. This path is not published through
the SDK or REST. A pre-existing row must match the seed's tenant, type, name,
and parent before it is skipped. Tenant-type seeds retain their existing rule:
the effective tenant is the seed's group ID.

## Existing collisions

This change prevents new reservations; it cannot safely decide which existing
objects to delete. Before rollout, an administrator should enumerate tenants,
compute their expected default IDs, and compare existing groups' owner, type,
name, and placement with the application's expected identity. Use an authorized
administrative read surface; a tenant-scoped 404 cannot establish global absence.

For each conflict, inspect memberships, child groups, and external references
before choosing a repair. Do not automatically force-delete the conflicting row
or change the victim's deterministic identity. After an approved removal or
migration, rerun the victim's ensure and verify that deployments recover. Retain
the collision alert until any pre-upgrade conflicts have been remediated.
