---
cf: true
type: project-rule
topic: security-data-access
generated-by: auto-config
version: 1.0
---
# Security And Data Access

<!-- toc -->

- [Authorization](#authorization)
  - [Authorize before data access](#authorize-before-data-access)
  - [Preserve fail-closed secure ORM behavior](#preserve-fail-closed-secure-orm-behavior)
  - [Use Scopable entities for tenant/resource tables](#use-scopable-entities-for-tenantresource-tables)

<!-- /toc -->

## Authorization
### Authorize before data access
Compile `SecurityContext` into an `AccessScope` before reading or mutating tenant-scoped data.
Evidence: `modules/simple-user-settings/simple-user-settings/src/domain/service.rs:76` - authorization before repository access.

### Preserve fail-closed secure ORM behavior
Unknown properties, deny-all scopes, and invalid scope matches must reject access rather than widen queries.
Evidence: `libs/modkit-db/src/secure/cond.rs:32` - scope condition compilation.

### Use Scopable entities for tenant/resource tables
SeaORM entities that store tenant/resource-owned data must derive `Scopable` with secure column annotations.
Evidence: `modules/simple-user-settings/simple-user-settings/src/infra/storage/entity.rs:1` - Scopable entity declaration.
