---
title: Type system (GTS)
description: The Global Type System — versioned, schema-validated type identity that underpins Gears' extension model.
sidebar:
  label: Type system (GTS)
  order: 12
---

The [Global Type System (GTS)](https://github.com/GlobalTypeSystem/gts-rust) gives the platform a **versioned, schema-validated type identity** for domain objects. It is the mechanism behind extensibility: error identities, plugin contracts, and domain-object schemas are all GTS-typed.

## Identifiers

A GTS identifier looks like:

```text
# Base data type format
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~

# Derived data type format
gts.<vendor>.<package>.<namespace>.<type>.v<MAJOR>[.<MINOR>]~<derived_type_vendor>.<derived_type_package>.<derived_type_namespace>.<derived_type>.v<DERIVED_MAJOR>[.<DERIVED_MINOR>]
```

The version is part of the identity, so schemas can evolve without silently breaking consumers.

## Extend without forking

New data types — event formats, document schemas, permission types, license types, custom attributes — can be introduced by registering new GTS types and instances, **without modifying existing API endpoints, Rust SDK or storage**. In Rust, schemas are derived from source types and registered in the **Types Registry** (the same way OpenAPI is generated from route metadata), so the catalog of types stays in sync with the code by construction.

This is what lets vendors extend the domain model without forking the framework, and what makes plugins and platform integrations type-safe rather than stringly-typed.

## Where it shows up

- **Errors** carry a GTS identifier so they are routable and machine-branchable. See [Error model](../error-model/).
- **Plugins** are keyed by GTS instance id. See [Plugins and extension points](../plugins-and-extension-points/).
- **Custom types** in an existing platform are added via GTS. See [Integrate into an existing platform](../../build-with-gears/integrate-existing-platform/).

## See also

- [Types Registry](../../capabilities/core-gears/) — the core gear in the system gear group that stores GTS schemas.
- [GTS reference](https://github.com/GlobalTypeSystem/gts-rust) — the type-system specification.
