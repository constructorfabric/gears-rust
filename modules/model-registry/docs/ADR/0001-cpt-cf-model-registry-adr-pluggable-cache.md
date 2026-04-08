---
status: accepted
date: 2026-02-18
---

# Pluggable Cache Backend with TTL Strategy

**ID**: `cpt-cf-model-registry-adr-pluggable-cache`

## Context and Problem Statement

Model Registry must achieve <10ms P99 latency for `get_tenant_model` operations at scale (10K tenants, 2M models, 1000:1 read:write ratio). How should we implement caching to meet these performance requirements while allowing deployment flexibility?

## Decision Drivers

* `cpt-cf-model-registry-nfr-performance` — P99 latency <10ms for model resolution
* `cpt-cf-model-registry-nfr-scale` — Support 10K tenants, 2M models
* `cpt-cf-model-registry-fr-cache-isolation` — Tenant data must be isolated in cache
* Deployment flexibility — single-node to multi-node clusters (Redis optional, DB-only viable for moderate scale)

## Considered Options

* Pluggable Cache Backend (Redis default, InMemory, Custom)
* Redis-only distributed cache
* In-memory cache per instance
* No cache (database only)

## Decision Outcome

Chosen option: "Pluggable Cache Backend", because it provides the performance benefits of distributed caching while allowing deployment flexibility and vendor customization.

### Consequences

* Good, because different deployment scenarios are supported (lightweight single-node without Redis, high-load clusters with Redis)
* Good, because testing is simplified with in-memory backend
* Good, because Redis provides proven horizontal scaling for production
* Bad, because additional abstraction layer adds complexity
* Bad, because cache behavior may differ slightly between backends

### Confirmation

* Code review verifies `CacheService` trait is implemented correctly
* Integration tests run against all supported backends
* Performance benchmarks confirm <10ms P99 with Redis backend at scale

## Pros and Cons of the Options

### Pluggable Cache Backend

Cache abstraction with compiled-in implementations selected via Cargo feature flags:
- `RedisCache` — default for production, horizontal scaling
- `InMemoryCache` — for lightweight deployments and testing (also valid for moderate-scale production where DB query caching suffices)

TTL Strategy (common across backends):
- Own data (tenant created): 30 minutes
- Inherited data (from parent tenant): 5 minutes

Cache key format: `mr:{tenant_id}:{entity}:{id}`

* Good, because deployment flexibility (single-node → cluster)
* Good, because vendor customization supported
* Good, because simpler testing with in-memory backend
* Good, because proven Redis solution for production scale
* Neutral, because requires trait abstraction
* Bad, because slight complexity increase

### Redis-only Distributed Cache

Hardcoded Redis implementation without abstraction.

* Good, because simpler implementation
* Good, because proven horizontal scaling
* Bad, because no flexibility for lightweight deployments
* Bad, because harder to test without Redis infrastructure
* Bad, because no vendor customization

### In-memory Cache per Instance

Local cache in each application instance.

* Good, because fastest (no network hop)
* Good, because simplest implementation
* Bad, because cache inconsistency between instances
* Bad, because memory pressure on instances
* Bad, because cold start penalty

### No Cache (Database Only)

Direct PostgreSQL queries with indexes, no caching layer.

* Good, because simplest architecture
* Good, because always consistent
* Bad, because cannot meet <10ms P99 at scale
* Bad, because database load increases linearly with read traffic

## More Information

Configuration example:
```yaml
model_registry:
  cache:
    backend: redis | memory | custom
    ttl_own_minutes: 30
    ttl_inherited_minutes: 5
    # Redis-specific
    redis:
      url: redis://localhost:6379
      pool_size: 10
```

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-model-registry-nfr-performance` — Distributed cache enables <10ms P99 latency
* `cpt-cf-model-registry-nfr-scale` — Redis backend supports horizontal scaling
* `cpt-cf-model-registry-fr-cache-isolation` — Cache key format ensures tenant isolation
* `cpt-cf-model-registry-principle-cache-first` — Establishes cache-first read pattern
