---
status: accepted
date: 2026-02-18
---

# Additive Tenant Inheritance with Provider Shadowing

**ID**: `cpt-cf-model-registry-adr-tenant-inheritance`

## Context and Problem Statement

Model Registry operates in a multi-tenant hierarchy where child tenants inherit from parent tenants. How should providers and approvals be inherited, and can child tenants customize inherited configuration?

## Decision Drivers

* `cpt-cf-model-registry-fr-provider-management` — Provider CRUD with inheritance
* `cpt-cf-model-registry-fr-tenant-isolation` — Tenant-scoped operations
* PRD requirement: "Child tenant can only restrict, not expand parent permissions"
* Compliance isolation — tenants may need to exclude certain providers
* Flexibility — different tenants have different vendor relationships

## Considered Options

* Additive Inheritance with Shadowing
* Strict Inheritance (no override)
* Explicit Copy (no inheritance)

## Decision Outcome

Chosen option: "Additive Inheritance with Shadowing", because it provides the flexibility needed for compliance isolation while maintaining the "restrict only" security model.

### Consequences

* Good, because child tenants can exclude unwanted providers (compliance)
* Good, because inherited providers are available by default (convenience)
* Good, because shadowing provides clear override semantics
* Good, because "restrict only" model prevents privilege escalation
* Bad, because resolution logic is more complex
* Bad, because potential confusion when shadowing occurs

### Confirmation

* Unit tests verify inheritance resolution order (tenant → parent → root)
* Unit tests verify shadowing correctly overrides parent provider
* Unit tests verify child cannot approve what parent blocked
* Integration tests verify compliance isolation scenarios

## Pros and Cons of the Options

### Additive Inheritance with Shadowing

Inheritance model:
- Child tenant sees: parent's providers + own providers
- Resolution order: tenant → parent → ... → root (first match wins)
- Shadowing: child can create provider with same slug to override parent
- Exclusion: shadow with `status: disabled` to block inherited provider

Approval inheritance:
- Child inherits parent's approvals (additive)
- Child can revoke inherited approval for their scope
- Child cannot approve what parent blocked (restrict only)

* Good, because flexibility for compliance isolation
* Good, because intuitive "inherit and customize" model
* Good, because consistent with PRD shadowing examples
* Good, because "restrict only" prevents privilege escalation
* Neutral, because requires clear documentation
* Bad, because resolution logic complexity
* Bad, because potential shadowing confusion

### Strict Inheritance (No Override)

Child inherits everything from parent without ability to override.

* Good, because simple and predictable
* Good, because no resolution ambiguity
* Bad, because no compliance isolation possible
* Bad, because child cannot exclude vendor they don't want
* Bad, because inflexible for enterprise scenarios

### Explicit Copy (No Inheritance)

No automatic inheritance; admin manually copies configuration.

* Good, because explicit control over everything
* Good, because no hidden inherited state
* Bad, because administrative burden
* Bad, because configuration drift between tenants
* Bad, because doesn't scale with tenant hierarchy depth

## More Information

Shadowing example from PRD:
```
Root tenant: azure-prod → platform Azure subscription (active)
Tenant A:    azure-prod → Tenant A's Azure subscription (active, shadows root)
Tenant B:    (no override) → uses root's azure-prod

Request from Tenant A for azure-prod::gpt-4o → Tenant A's Azure
Request from Tenant B for azure-prod::gpt-4o → Root's Azure
```

Exclusion example:
```
Root tenant: openai → platform OpenAI account (active)
Tenant C:    openai → status: disabled (shadows and excludes)

Tenant C cannot use any OpenAI models (compliance requirement)
```

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-fr-provider-management` — Inheritance with shadowing support
* `cpt-cf-model-registry-fr-tenant-isolation` — Tenant-scoped resolution
* `cpt-cf-model-registry-principle-additive-inheritance` — Establishes inheritance model
* `cpt-cf-model-registry-usecase-register-provider` — Provider registration with shadowing
