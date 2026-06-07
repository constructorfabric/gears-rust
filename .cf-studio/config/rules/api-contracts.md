---
cf: true
type: project-rule
topic: api-contracts
generated-by: auto-config
version: 1.0
---
# API Contracts

<!-- toc -->

- [REST](#rest)
  - [Register endpoints with OperationBuilder](#register-endpoints-with-operationbuilder)
  - [Put handlers behind typed extractors](#put-handlers-behind-typed-extractors)
  - [Register OData list endpoints explicitly](#register-odata-list-endpoints-explicitly)

<!-- /toc -->

## REST
### Register endpoints with OperationBuilder
Define method, path, operation ID, auth/license policy, schemas, and error responses through `OperationBuilder`.
Evidence: `docs/modkit_unified_system/04_rest_operation_builder.md:18` - OperationBuilder basics.

### Put handlers behind typed extractors
Handlers should use `SecurityContext`, service `Extension`, typed body/query/path extractors, and `ApiResult`.
Evidence: `modules/simple-user-settings/simple-user-settings/src/api/rest/handlers.rs:14` - handler extraction pattern.

### Register OData list endpoints explicitly
Use the project OData helpers for filtering, ordering, selection, and cursor pagination.
Evidence: `docs/modkit_unified_system/07_odata_pagination_select_filter.md:409` - common OData queries.
