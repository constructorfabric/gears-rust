# Static Tenant Resolver Plugin

Config-based plugin for testing multi-tenant deployments.

## Quick Reference

- Tenants and access rules defined in YAML config
- In-memory tenant data (the plugin itself needs no database)
- Publishes its plugin instance through the database-backed Types Registry, so `types-registry` needs a database binding
- Implements `TenantResolverPluginClient`
