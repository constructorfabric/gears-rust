<!-- Updated: 2026-04-20 by Constructor Tech -->

# Rust SDK Contracts — Resource Group

> Reference document for planned Rust trait contracts and SDK types.
> Canonical source after implementation: `resource-group-sdk/src/`.

## SDK Models

Defined in `resource-group-sdk/src/models.rs`. Aligned with REST API schemas ([openapi.yaml](./openapi.yaml)).

```rust
use uuid::Uuid;

// There are no type models: resource group types are GTS Type Schemas in the Types Registry
// (ADR-002). The SDK declares only the platform tenant type and `TENANT_RG_TYPE_PATH`.

// ── Hierarchy context ────────────────────────────────────────────────────

/// RG hierarchy context — position in the resource group tree.
#[derive(Debug, Clone)]
pub struct Hierarchy {
    pub parent_id: Option<Uuid>,
    pub tenant_id: Uuid,
}

/// Hierarchy context with computed depth (for hierarchy traversal responses).
#[derive(Debug, Clone)]
pub struct HierarchyWithDepth {
    pub parent_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub depth: i32,
}

// ── Group ───────────────────────────────────────────────────────────────

/// Matches REST `Group` schema. GTS-aligned: `id`/`type`/`name` + derived type
/// fields at top level. Hierarchy context in `hierarchy` envelope.
/// Derived type fields (e.g. `menu_bold`, `barrier`) are flattened to top level
/// in API; stored in `metadata` JSONB column in DB. `id`/`name` are not duplicated
/// in DB `metadata`.
#[derive(Debug, Clone)]
pub struct ResourceGroup {
    pub id: Uuid,
    pub r#type: String,
    pub name: String,
    pub hierarchy: Hierarchy,
    /// Derived type fields, flattened. Stored in DB as `metadata` JSONB.
    #[serde(flatten)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// Matches REST `GroupWithDepth` schema.
#[derive(Debug, Clone)]
pub struct ResourceGroupWithDepth {
    pub id: Uuid,
    pub r#type: String,
    pub name: String,
    pub hierarchy: HierarchyWithDepth,
    #[serde(flatten)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// Matches REST `CreateGroupRequest` schema.
/// Derived type fields sent as top-level properties.
#[derive(Debug, Clone)]
pub struct CreateGroupRequest {
    pub id: Option<Uuid>,
    pub r#type: String,
    pub name: String,
    pub parent_id: Option<Uuid>,
    #[serde(flatten)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// Matches REST `UpdateGroupRequest` schema.
#[derive(Debug, Clone)]
pub struct UpdateGroupRequest {
    pub r#type: String,
    pub name: String,
    pub parent_id: Option<Uuid>,
    #[serde(flatten)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

// ── Membership ──────────────────────────────────────────────────────────

/// Matches REST `Membership` schema.
#[derive(Debug, Clone)]
pub struct ResourceGroupMembership {
    pub group_id: Uuid,
    pub resource_type: String,
    pub resource_id: String,
}

/// Matches REST `addMembership` / `deleteMembership` path params.
#[derive(Debug, Clone)]
pub struct AddMembershipRequest {
    pub group_id: Uuid,
    pub resource_type: String,
    pub resource_id: String,
}

/// Matches REST `addMembership` / `deleteMembership` path params.
#[derive(Debug, Clone)]
pub struct RemoveMembershipRequest {
    pub group_id: Uuid,
    pub resource_type: String,
    pub resource_id: String,
}

// ── Pagination ──────────────────────────────────────────────────────────

/// Cursor-based pagination metadata. Matches REST `PageInfo` schema.
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u64,
}

/// Generic paginated response. Matches REST `*Page` schemas.
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page_info: PageInfo,
}
```

## Core API Trait — `ResourceGroupClient`

Defined in `resource-group-sdk/src/api.rs`. Full read+write contract for general consumers.

```rust
use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[async_trait]
pub trait ResourceGroupClient: Send + Sync {
    // ── Group lifecycle ─────────────────────────────────────────────
    async fn create_group(&self, ctx: &SecurityContext, request: CreateGroupRequest) -> Result<ResourceGroup, ResourceGroupError>;
    async fn get_group(&self, ctx: &SecurityContext, group_id: Uuid) -> Result<ResourceGroup, ResourceGroupError>;
    async fn list_groups(&self, ctx: &SecurityContext, query: ListQuery) -> Result<Page<ResourceGroup>, ResourceGroupError>;
    async fn update_group(&self, ctx: &SecurityContext, group_id: Uuid, request: UpdateGroupRequest) -> Result<ResourceGroup, ResourceGroupError>;
    async fn delete_group(&self, ctx: &SecurityContext, group_id: Uuid) -> Result<(), ResourceGroupError>;

    // ── Hierarchy ───────────────────────────────────────────────────
    async fn list_group_depth(&self, ctx: &SecurityContext, group_id: Uuid, query: ListQuery) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    // ── Membership lifecycle ────────────────────────────────────────
    async fn add_membership(&self, ctx: &SecurityContext, request: AddMembershipRequest) -> Result<ResourceGroupMembership, ResourceGroupError>;
    async fn remove_membership(&self, ctx: &SecurityContext, request: RemoveMembershipRequest) -> Result<(), ResourceGroupError>;
    async fn list_memberships(&self, ctx: &SecurityContext, query: ListQuery) -> Result<Page<ResourceGroupMembership>, ResourceGroupError>;
}
```

## Integration Read Trait — `ResourceGroupReadHierarchy`

Narrow read-only contract for in-process plugin consumers — the AuthZ resolver
plugin, the tenant-resolver RG plugin, and an in-process AuthZ PDP. Carries
hierarchy walks, flat group listing, single-group existence lookup, and
membership listing. All methods are resolved **unscoped** (they bypass
`PolicyEnforcer`): a consumer that *is* the PDP cannot route reads back through
the PEP without recursing. Writes remain on `ResourceGroupClient`.

```rust
/// Narrow reads for in-process plugin consumers (AuthZ resolver, tenant-resolver
/// RG plugin, in-process AuthZ PDP). Resolved unscoped — bypasses PolicyEnforcer.
#[async_trait]
pub trait ResourceGroupReadHierarchy: Send + Sync {
    /// Descendants of a reference group (depth >= 0).
    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    /// Ancestors of a reference group (depth <= 0).
    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    /// Flat OData-filtered group listing; enables batch reads (`id in (…)`).
    async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError>;

    /// Single-group existence + tenant-ownership lookup. Backs PDP scope
    /// validation (`/tenants/{t}/resourceGroups/{rg}`). Resolved unscoped.
    async fn get_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, ResourceGroupError>;

    /// Membership listing. Backs PDP group-membership resolution; the caller
    /// MUST supply a subject-scoped filter (`resource_id eq '<subject_id>'`).
    /// Resolved unscoped.
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError>;
}
```

## Plugin Trait — collapsed into `ResourceGroupReadHierarchy`

The earlier two-tier design had a separate `ResourceGroupReadPluginClient`
extending `ResourceGroupReadHierarchy` with `list_memberships`. The shipped SDK
collapses this: `list_memberships` (and `get_group`) live directly on
`ResourceGroupReadHierarchy`, and the vendor-specific plugin gateway resolves
`dyn ResourceGroupReadHierarchy`. A vendor backend replaces the registered
`ResourceGroupReadHierarchy` implementation at gear init rather than
implementing a distinct plugin trait.

## ClientHub Registration

Single implementation, two registrations:

```rust
let svc: Arc<RgService> = Arc::new(RgService::new(/* ... */));

// Full read+write client: hub.get::<dyn ResourceGroupClient>()
hub.register::<dyn ResourceGroupClient>(svc.clone());

// AuthZ plugin: hub.get::<dyn ResourceGroupReadHierarchy>()
hub.register::<dyn ResourceGroupReadHierarchy>(svc.clone());
```

## Usage Example

```rust
use toolkit_security::SecurityContext;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupReadHierarchy};
use uuid::Uuid;

// AuthZ plugin — hierarchy only
let rg_hierarchy = hub.get::<dyn ResourceGroupReadHierarchy>()?;

// General consumer — full CRUD including reads
let rg = hub.get::<dyn ResourceGroupClient>()?;

let ctx = SecurityContext::builder()
    .subject_id(Uuid::new_v4())
    .subject_tenant_id(Uuid::parse_str("11111111-1111-1111-1111-111111111111")?)
    .build()?;

// Hierarchy traversal — descendants (unscoped read)
let query = ODataQuery::default(); // e.g. $filter "hierarchy/depth ge 0"
let descendants = rg_hierarchy
    .get_group_descendants(&ctx, group_id, &query)
    .await?;

// Full CRUD — create group
let group = rg
    .create_group(&ctx, CreateGroupRequest {
        id: None,
        r#type: "gts.cf.core.rg.type.v1~y.system.tn.tenant.v1~".into(),
        name: "Acme Corp".into(),
        parent_id: None,
        metadata: Default::default(),
    })
    .await?;
```

## Trait Hierarchy Summary

| Trait | Methods | Consumers | ClientHub key |
|-------|---------|-----------|---------------|
| `ResourceGroupClient` | full CRUD: groups, memberships, hierarchy | Domain services, Apps, Admins | `dyn ResourceGroupClient` |
| `ResourceGroupReadHierarchy` | 5 (`get_group_descendants`, `get_group_ancestors`, `list_groups`, `get_group`, `list_memberships`; all unscoped / PEP-bypassing) | AuthZ resolver plugin, tenant-resolver RG plugin, in-process AuthZ PDP | `dyn ResourceGroupReadHierarchy` |
