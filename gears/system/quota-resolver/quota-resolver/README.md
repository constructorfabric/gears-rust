# Quota Resolver

Main gear for quota operations in Gears. It maps a principal to a quota subject,
fixes the tenant, finds a backend plugin through the GTS types registry and
delegates. It holds no quota data and decides no quota question; the backend does
(the `quota-enforcement` gear is one).

## Overview

- **Two planes.** `PlatformQuotaResolverApi` has every operation and takes the
  calling module's platform credential; its routes under
  `/quota-resolver/v1/platform` are internal and not exposed by `api-gateway`.
  `QuotaResolverApi` has `quota-state` only, for the caller's own principal and
  tenant; it is exposed at `POST /quota-resolver/v1/quota-state`.
- **Principal mapping.** A principal in a platform request is replaced by a
  quota subject of the type that `principal_subject_types` names for the principal
  type, with the same id. An unmapped principal type is denied as
  `principal_not_mapped`.
- **Plugin discovery.** Backends are plugin instances of
  `gts.cf.toolkit.plugins.plugin.v1~cf.core.quota_resolver.plugin.v1~`. The gateway keeps
  those with a registered client and the configured `vendor`, picks the lowest
  `priority` and memoizes the choice.
- **GTS publication.** The gear publishes the SDK's GTS types (plugin spec,
  quota subject base, tenant subject type, metric base), so a host that runs a
  backend runs this gear too; the two have no start order.
- **ClientHub and REST.** Both contracts are registered in `ClientHub`.

## Configuration

```yaml
gears:
  quota-resolver:
    config:
      vendor: "constructorfabric"   # selects the backend plugin
      principal_subject_types:      # principal type -> quota subject type
        "gts.cf.core.security.subject_user.v1~": "gts.cf.core.quota.subj.v1~acme.core.quota.user.v1~"
```

Both sides of a mapping must be GTS type ids, and the quota subject type must be
derived from `gts.cf.core.quota.subj.v1~`, other than the base and the tenant
subject type. The quota subject types themselves are registered in the types
registry by the operator.

The exposed `quota-state` route is bound to the API gateway rate-limit zone
`rl_quota_resolver_quota_state`. The gateway configuration must declare it, keyed
on the caller's identity; api-gateway does not start without it.

## Usage

See the SDK README, `../quota-resolver-sdk/README.md`, for charging a user and for
writing a backend. A host links the gear with `--features quota-resolver` on the
example server; `quota-enforcement` implies it.

## Design

`../docs/PRD.md`, `../docs/DESIGN.md`, `../docs/ADR/`.
