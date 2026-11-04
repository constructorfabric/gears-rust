# Orders Test AuthZ Plugin

> **Orders test and isolated development hosts only — never production.** This plugin answers authorization from a policy written in
> the host's config file. It exists so end-to-end suites can exercise constraint-bearing
> decisions without an external policy engine.

Declarative policy decision point (PDP) for the AuthZ Resolver gateway.

## Purpose

The static AuthZ plugin emits only tenant predicates. An end-to-end suite that
checks a permission matrix needs more: per-subject decisions, several constraint paths, delegation
proofs and property-less permits. This plugin reads those decisions from configuration:

- `rules` permit a matching request **with constraints** (one per declared path);
- `unconditional_grants` permit a matching request **without constraints**;
- anything else is denied.

This fixture is owned by Orders and lives under `gears/bss/orders/test-authz-plugin`.
It also answers permission checks from Pricing, Products and Event Broker when those gears
participate in Orders scenarios. It is linked into the example server only through the
explicit `bss-orders-test-authz` feature, which enables Orders and is disabled by default.
The normal Orders gear uses this crate only as a dev-dependency.

**Do not use in production.** The crate sets `publish = false` and must never be enabled
in a production composition. It is not a general-purpose system authorization plugin.

## Selection

The plugin registers a `cf.builtin.bss_orders_test_authz_resolver.plugin.v1` instance under its
configured `vendor`. The AuthZ resolver uses the plugin with the **lowest `priority`** among the
vendor's registered AuthZ plugins, so on a host that also links the static plugin (priority 100)
this one (default priority 10) answers. Wherever the feature is linked, the gear block must
declare a policy: a block with no `rules` and no `unconditional_grants` (or no block at all) fails
the gear init rather than registering a deny-all plugin that outranks the static one.

## Configuration

Gear block key: `bss-orders-test-authz-plugin`. Every object refuses unknown keys.

```yaml
gears:
  bss-orders-test-authz-plugin:
    config:
      vendor: "constructorfabric"     # default
      priority: 10                    # default; lower wins
      policy_revision: "suite-r1"     # free text, logged at init
      rules:
        # Two paths, OR-ed: the seller operator sees orders it sells, and orders it buys.
        - id: "seller-operator-read"
          subject: { id: "22222222-6a88-4768-9dfc-6bcd5187d9ed" }
          resource_type: "gts.cf.bss.orders.order.v1~"
          actions: ["read", "list"]
          paths:
            - predicates:
                - { property: "seller_tenant_id", values: ["c0000000-0000-4000-8000-00000000000c"] }
            - predicates:
                - { property: "resource_tenant_id", value: "b0000000-0000-4000-8000-00000000000b" }
        # Delegation: matches only with an accepted proof on the resource.
        - id: "partner-delegated-read"
          subject:
            id: "dddddddd-6a88-4768-9dfc-6bcd5187d9ed"
            tenant_id: "d0000000-0000-4000-8000-00000000000d"
          resource_type: "gts.cf.bss.orders.*"
          actions: ["*"]
          paths:
            - predicates:
                - { property: "resource_tenant_id", values: ["a0000000-0000-4000-8000-00000000000a"] }
          delegation:
            proof_property: "delegation_proof_ref"
            accepted: ["e2e-proof-ok"]
      unconditional_grants:
        # A property-less check (the PEP does not require constraints).
        - id: "producer-event-type"
          subject: { id: "0e0e0000-0000-4000-8000-000000000005" }
          resource_type: "gts.cf.core.events.event_type.v1~"
          action: "produce"
```

| Field | Type | Meaning |
|-------|------|---------|
| `vendor` | string | Vendor of the registered plugin instance (default `constructorfabric`). |
| `priority` | integer | Registration priority, lower wins (default `10`). |
| `policy_revision` | string | Free text, logged at init (default empty). |
| `rules[].id` | string | Unique across rules and grants. |
| `rules[].subject` | `{ id, tenant_id? }` | Subject id (UUID); optional subject tenant (UUID). |
| `rules[].resource_type` | string | Exact type, or a prefix when it ends with `*`. |
| `rules[].actions` | list of strings | Action names, or exactly `["*"]` for any action. |
| `rules[].paths[].predicates[]` | `{ property, values }` or `{ property, value }` | One constraint per path; predicates within a path are AND-ed. |
| `rules[].delegation` | `{ proof_property, accepted }` | Optional delegation-proof requirement. |
| `unconditional_grants[]` | `{ id, subject, resource_type, action }` | Same matchers as rules; `action` may be `*`. |

## Matching

A rule or grant applies to a request when all of these hold:

- **Subject:** `subject.id` equals the declared `id`; when `tenant_id` is declared,
  `subject.properties["tenant_id"]` is a string holding that UUID (missing, non-string or
  different → no match).
- **Resource type:** equal to the declared type, or starts with the head of a type ending in `*`
  (`gts.cf.bss.pricing.*` matches `gts.cf.bss.pricing.price.v1~`).
- **Action:** one of the declared actions, or any action for `*`.

The answer, in order:

1. Every applying rule (whose delegation, if any, is satisfied) contributes its paths, in
   declaration order. If at least one did: `decision: true` with those constraints. A predicate
   with `values` becomes an `in` predicate, one with `value` an `eq` predicate; values are strings.

   **Local decision.** When the request has `require_constraints: false` the enforcement point
   does not enforce constraints (Event Broker's tenant check sends `owner_tenant_id` and discards
   the scope), so the rule is decided here: it applies only if at least one of its paths is
   satisfied by `resource.properties` — every predicate's property is present as a string, equal
   to the `value` (`eq`) or one of the `values` (`in`). For the UUID properties listed under
   [Validation](#validation) the strings are compared as parsed UUIDs, and a value that is not a
   UUID is unsatisfied. A rule with no satisfied path does not apply, and evaluation continues
   with step 2. A rule decided locally still returns its paths as constraints, which the
   enforcement point ignores. A rule skipped this way is skipped before its delegation check,
   so it records no delegation reason (a plain deny, not `delegation_proof_required`). With
   `require_constraints: true` properties are not checked: the
   constraints are returned for the enforcement point to enforce.
2. Otherwise, when the request has `require_constraints: false` and a grant applies:
   `decision: true` with no constraints. A grant is never used for a PEP that requires
   constraints.
3. Otherwise, when a delegation rule applied but its proof was not accepted: `decision: false`
   with deny reason `delegation_proof_required` (the resource property `proof_property` is
   absent) or `delegation_proof_invalid` (present but not a string in `accepted`). When several
   delegation rules failed, `delegation_proof_required` wins.
4. Otherwise `decision: false` with no reason.

The plugin never reads token scopes, the tenant context or the PEP's capabilities, and never
emits group or tenant-subtree predicates. Policy outcomes are never errors.

## Validation

The policy is validated when the gear initialises; any of the ten errors below fails the init.

| Error | Cause |
|-------|-------|
| `DuplicateId` | Two rules or grants share an `id`. |
| `EmptyActions` | A rule has no action, or an empty action name; a grant has an empty action. |
| `EmptyPaths` | A rule declares no path (use a grant for a constraint-less permit). |
| `EmptyPredicates` | A path declares no predicate. |
| `EmptyProperty` | A predicate names a blank `property`. |
| `PredicateValues` | A predicate does not declare exactly one of a non-empty `values` or `value`. |
| `InvalidUuid` | A value of `owner_tenant_id`, `id`, `resource_tenant_id`, `seller_tenant_id`, `payer_tenant_id` or `subject_tenant_id` is not a UUID. |
| `Wildcard` | `*` anywhere but a trailing resource-type suffix or a sole action, or a resource type that is only `*`. |
| `EmptyResourceType` | A rule's or grant's resource type is empty. |
| `EmptyDelegation` | A delegation has a blank `proof_property`, no `accepted` value, or a blank `accepted` value. |

## Feature Flag

The example server links this plugin only when built with the `bss-orders-test-authz` feature:

```bash
cargo build --bin cf-gears-example-server --features bss-orders-test-authz
```
