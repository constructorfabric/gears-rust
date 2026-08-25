---
status: accepted
date: 2026-08-19
decision-makers: Constructor Fabric Steering Committee
---

# Required Headers Guard Plugin — Request/Response Header Enforcement


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Plugin Config (ctx.config keys)](#plugin-config-ctxconfig-keys)
  - [Decision Flow](#decision-flow)
  - [Registry Integration](#registry-integration)
  - [Upstream Configuration Example](#upstream-configuration-example)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Dedicated builtin `GuardPlugin`](#dedicated-builtin-guardplugin)
  - [Per-upstream custom Starlark guard plugin](#per-upstream-custom-starlark-guard-plugin)
  - [Push the check into `Upstream.headers` passthrough config](#push-the-check-into-upstreamheaders-passthrough-config)
- [Out of Scope](#out-of-scope)
- [Future Considerations](#future-considerations)
- [Related ADRs](#related-adrs)
- [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-required-headers-guard-plugin`

## Context and Problem Statement

Many upstreams need to enforce presence of specific headers before a request
reaches them — for example, requiring a correlation ID or an API version
header on inbound requests. Symmetrically, operators sometimes need to
reject upstream responses that omit an expected header (e.g. `Content-Type`)
as a defense against a misbehaving or compromised upstream. Without a
built-in plugin for this common pattern, each operator would have to write
and maintain a bespoke Starlark guard (per [ADR: Plugin System](./0003-plugin-system.md))
for what is, in most cases, a simple presence check.

## Decision Drivers

* This is a common validation pattern that recurs across many upstreams and
  should not require a custom Starlark guard each time.
* The check must be opt-in per upstream and fail-open when unconfigured — an
  upstream that doesn't configure the plugin should see no behavior change.
* Both request and response phases are legitimate use cases and should be
  handled symmetrically by the same plugin, independently configurable.

## Considered Options

* Dedicated builtin `GuardPlugin`
* Per-upstream custom Starlark guard plugin
* Push the check into `Upstream.headers` passthrough config

## Decision Outcome

Chosen option: "Dedicated builtin `GuardPlugin`". A stateless
`RequiredHeadersGuardPlugin` checks for the presence of configured header
names on the request (before proxying to upstream) and/or on the upstream's
response (before returning to the caller), rejecting with a phase-specific
status code on the first missing header.

### Plugin Config (ctx.config keys)

| Key | Required | Description |
|---|---|---|
| `required_request_headers` | No | Comma-separated header names checked in `guard_request`. Absent or blank (after trimming each entry) → phase is a no-op. |
| `required_response_headers` | No | Comma-separated header names checked in `guard_response`. Absent or blank → phase is a no-op. |

Both keys are independent — configuring one does not affect the other
phase. Header names are matched case-insensitively; only presence is
checked, not header values.

### Decision Flow

```text
guard_request(ctx) / guard_response(ctx)
  ├─ Read required_request_headers / required_response_headers from ctx.config
  ├─ Absent or blank → Allow (fail-open, unconfigured)
  ├─ Parse: split on ',', trim, lowercase, drop empty entries
  ├─ Scan ctx.headers for each required name (case-insensitive), in order
  ├─ All present → Allow
  └─ First missing name found → Reject
      ├─ Request phase  → status 400, error_code REQUIRED_HEADER_MISSING
      └─ Response phase → status 502, error_code REQUIRED_HEADER_MISSING
```

Only the first missing header is reported per rejection, not the full set.

### Registry Integration

```rust
impl GuardPluginRegistry {
    pub fn with_builtins() -> Self {
        let mut plugins: HashMap<String, Arc<dyn GuardPlugin>> = HashMap::new();
        plugins.insert(
            REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
            Arc::new(RequiredHeadersGuardPlugin),
        );
        Self { plugins }
    }
}
```

Note: `TIMEOUT_GUARD_PLUGIN_ID` and `CORS_GUARD_PLUGIN_ID` are declared as
GTS constants and registered in the types-registry catalog
(`type_catalog.rs`), but timeout and CORS enforcement are implemented as
core Data Plane logic rather than `GuardPlugin` trait implementations —
`RequiredHeadersGuardPlugin` is currently the only entry in
`GuardPluginRegistry::with_builtins()`.

### Upstream Configuration Example

```json
{
  "plugins": {
    "sharing": "private",
    "items": [
      {
        "plugin_ref": "gts.cf.core.oagw.guard_plugin.v1~cf.core.oagw.required_headers.v1",
        "config": {
          "required_request_headers": "x-correlation-id,accept",
          "required_response_headers": "content-type"
        }
      }
    ]
  }
}
```

### Consequences

#### Positive

- Good, because a common validation need is covered without writing a
  per-upstream custom Starlark guard.
- Good, because request and response enforcement are both covered by the
  same plugin, independently configurable.
- Good, because fail-open on absent/blank config means adding the plugin to
  the registry has no effect on upstreams that don't opt in.
- Good, because the plugin is stateless — no cache, no security-sensitive
  material, trivial to reason about and test.

#### Negative

- Bad, because only header *presence* is validated, not header *values* —
  an upstream needing value validation (e.g. a specific API version) still
  needs a custom guard.
- Bad, because only the first missing header is reported per rejection,
  which can require multiple round-trips to discover all missing headers.

#### Risks

- **Misconfigured comma list**: an all-blank or empty config value (e.g.
  `", , ,"`) silently no-ops rather than erroring, matching the
  `blank_header_names_are_ignored` test. An operator expecting enforcement
  from a malformed config string will not be warned.

### Confirmation

Code review confirms: `RequiredHeadersGuardPlugin` implemented in
`oagw/src/infra/plugin/required_headers_guard.rs`, registered under
`REQUIRED_HEADERS_GUARD_PLUGIN_ID` (`oagw/src/domain/gts_helpers.rs`) in
`GuardPluginRegistry::with_builtins()`
(`oagw/src/infra/plugin/registry.rs`). Behavior is covered by 10 unit tests
in the plugin module (unconfigured-allow, request/response allow and
reject, case-insensitivity, first-missing-reported, phase isolation,
blank-header-names-ignored) and 4 e2e tests in
`testing/e2e/gears/oagw/test_guard_plugins.py`.

## Pros and Cons of the Options

### Dedicated builtin `GuardPlugin`

A stateless plugin shipped in the `oagw` crate and registered by default.

* Good, because it requires zero setup per upstream beyond config — no code
  to write or deploy
* Good, because it is covered by the same test and review process as other
  builtin plugins
* Bad, because it only covers presence checks — anything more elaborate
  still needs a custom guard

### Per-upstream custom Starlark guard plugin

Each operator writes their own guard using the mechanism described in
[ADR: Plugin System](./0003-plugin-system.md).

* Good, because it is fully flexible — any check, including value
  validation, is possible
* Bad, because it duplicates the same presence-check logic across every
  upstream that needs it
* Bad, because it pushes maintenance and testing burden onto each operator
  for a pattern that is almost always identical

### Push the check into `Upstream.headers` passthrough config

Extend the existing header passthrough/rewrite configuration to also
support "required" markers.

* Good, because it avoids introducing a new plugin type
* Bad, because it conflates header *forwarding* concerns with header
  *validation* concerns in a single config surface
* Bad, because it does not naturally extend to response-phase enforcement,
  which is a header validation concern independent of passthrough

## Out of Scope

- **Header value validation**: matching a header against a regex, allow-list,
  or expected value. Only presence is checked today.

## Future Considerations

- **Header value validation**: extending the config schema to support
  value constraints per header, not just presence.
- **Reporting all missing headers**: returning the full set of missing
  header names in the rejection detail instead of only the first.

## Related ADRs

- [ADR: Plugin System](./0003-plugin-system.md) — `GuardPlugin` trait and
  execution model
- [ADR: CORS](./0006-cors.md) — contrasting precedent: CORS was deliberately
  built as core Data Plane logic rather than a `GuardPlugin` trait
  implementation, for preflight-fast-path reasons that don't apply here

## References

- `oagw/src/infra/plugin/required_headers_guard.rs` — plugin implementation
- `testing/e2e/gears/oagw/test_guard_plugins.py` — e2e coverage

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADR**: [ADR: Plugin System](./0003-plugin-system.md)

This decision directly addresses the following requirements or design
elements:

* `cpt-cf-oagw-fr-builtin-plugins` — Built-in plugin implementation
