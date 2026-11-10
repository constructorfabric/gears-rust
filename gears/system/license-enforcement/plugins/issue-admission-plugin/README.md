# License Enforcement issue admission plugin

The admission plugin of the `license-enforcement` gear for the `issue` intake. It admits grant
parameters from a caller that the PDP authorized, and it verifies nothing: the rows of the gear
store are the source of truth. See [DESIGN §3.3, Admission plugin](../../docs/DESIGN.md#33-api-contracts).

- `admit` gives each root a new id, and stores as evidence the request in a canonical form with
  the identity of the caller. The replay key is the tenant of the caller and the idempotency token.
- `verify` returns verified without root fields, so a check uses the stored fields.
- The `import` intake is not served.

Link it into the binary with `use le_issue_admission_plugin as _;`.
