# Rules AuthZ Plugin

AuthZ resolver plugin that evaluates an explicitly configured, fail-closed permission policy.
It is a platform PDP provider: it knows no consumer gear's types. Consumers declare resource
types and properties; this plugin returns constraints in the standard resolver-SDK model.

## Policy model

Each rule names:

| Field | Meaning |
|---|---|
| `subject` | Authenticated subject selector (`id`, `tenant_id`, `subject_type`); every present field must match; at least one is required |
| `resource_type`, `actions` | Exact resource type and action names; no wildcard |
| `paths` | OR alternatives; each path is an AND of `property IN values` predicates (finite resource-ID sets use the `id` property) |
| `payer_use` | Optional separate condition: the named property must be one of `values`; added to every path and checked against a supplied proposed value |
| `delegation` | Optional delegated-path condition: the request property `proof_property` must carry one of `accepted` |

`unconditional_grants` (optional, default none) lists explicit grants for **property-less**
resource checks, such as Event Broker's `event_type` `produce` check, which declares no
constraint properties and cannot be expressed as a path:

| Field | Meaning |
|---|---|
| `id` | Unique identifier, shared with rule ids |
| `subject` | Exact subject: both `id` and `tenant_id` are required and must match |
| `resource_type`, `action` | One exact resource type and one exact action; no list, no wildcard |

An unconditional grant allows with **no constraints** only when the subject, resource type and
action all match **and** the PEP declares no supported constraint properties **and** does not
require constraints. Any other check ignores it, so it can never stand in for row-level scope
or turn a scoped resource into allow-all. A blank, wildcard or nil field, a reused id or a
duplicate triple fails startup.

Decision: candidate rules match subject, resource type and action. A path survives only if
every predicate whose property the request supplies holds for the supplied value; surviving
paths are returned as OR constraints. Paths of different rules are never merged. No surviving
path denies with a reason code.

Fail closed: no matching rule or grant denies (`no_matching_rule`), a malformed configuration
fails startup, and there is no default policy, wildcard, allow-all or tenant-membership shortcut.

**Interim delegation semantics.** `delegation_proof_required` and `delegation_proof_invalid`
deny codes and the opaque-reference membership check are interim, pending platform agreement on
the delegation-proof request carrier and deny codes. They do not verify issuer keys, expiry or
revocation; revoke by removing a reference from `accepted`.

This plugin is suitable for integration and live end-to-end environments with explicitly
provisioned identities. Selecting it for production requires the platform policy-provisioning
process and deployed verification.

## Configuration

```yaml
gears:
  rules-authz-plugin:
    config:
      vendor: "constructorfabric"
      priority: 10   # lower than static-authz (100) to take precedence
      policy_revision: "orders-e2e-2026-10-06"
      rules:
        - id: "seller-operator"
          subject: { id: "00000000-0000-0000-0000-000000000101" }
          resource_type: "gts.cf.bss.orders.order.v1~"
          actions: ["read", "hold", "resume", "cancel"]
          paths:
            - predicates:
                - { property: "seller_tenant_id", values: ["00000000-0000-0000-0000-000000000020"] }
      unconditional_grants:
        - id: "orders-producer-event-type-produce"
          subject: { id: "00000000-0000-0000-0000-000000000105", tenant_id: "00000000-0000-0000-0000-0000000000aa" }
          resource_type: "gts.cf.core.events.event_type.v1~"
          action: "produce"
```

Event Broker's publish also makes a separate tenant-scope check
(`gts.cf.core.events.request.v1~`, `produce`, property `owner_tenant_id`); grant it with an
ordinary rule whose path names the permitted tenant.

## Feature flag

The example server includes this plugin only when built with the `rules-authz` feature.
