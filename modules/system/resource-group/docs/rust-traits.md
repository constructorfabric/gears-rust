<!-- Updated: 2026-04-07 by Constructor Tech -->

# Rust SDK Contracts — Resource Group

> Reference document for planned Rust trait contracts and SDK types.
> Canonical source after implementation: `resource-group-sdk/src/`.

## SDK Models

Defined in `resource-group-sdk/src/models.rs`. Aligned with REST API schemas ([openapi.yaml](./openapi.yaml)).

```rust
use uuid::Uuid;

// ── Type ────────────────────────────────────────────────────────────────

/// Matches REST `Type` schema.
#[derive(Debug, Clone)]
pub struct ResourceGroupType {
    pub code: String,
    pub can_be_root: bool,
    pub allowed_parents: Vec<String>,
    pub allowed_memberships: Vec<String>,
    pub metadata_schema: Option<serde_json::Value>,
}

/// Matches REST `CreateTypeRequest` schema.
#[derive(Debug, Clone)]
pub struct CreateTypeRequest {
    pub code: String,
    pub can_be_root: bool,
    pub allowed_parents: Vec<String>,
    pub allowed_memberships: Vec<String>,
    pub metadata_schema: Option<serde_json::Value>,
}

/// Matches REST `UpdateTypeRequest` schema.
#[derive(Debug, Clone)]
pub struct UpdateTypeRequest {
    pub can_be_root: bool,
    pub allowed_parents: Vec<String>,
    pub allowed_memberships: Vec<String>,
    pub metadata_schema: Option<serde_json::Value>,
}

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
use modkit_security::SecurityContext;
use uuid::Uuid;

#[async_trait]
pub trait ResourceGroupClient: Send + Sync {
    // ── Type lifecycle ──────────────────────────────────────────────
    async fn create_type(&self, ctx: &SecurityContext, request: CreateTypeRequest) -> Result<ResourceGroupType, ResourceGroupError>;
    async fn get_type(&self, ctx: &SecurityContext, code: &str) -> Result<ResourceGroupType, ResourceGroupError>;
    async fn list_types(&self, ctx: &SecurityContext, query: ListQuery) -> Result<Page<ResourceGroupType>, ResourceGroupError>;
    async fn update_type(&self, ctx: &SecurityContext, code: &str, request: UpdateTypeRequest) -> Result<ResourceGroupType, ResourceGroupError>;
    async fn delete_type(&self, ctx: &SecurityContext, code: &str) -> Result<(), ResourceGroupError>;

    // ── Group lifecycle ─────────────────────────────────────────────
    async fn create_group(&self, ctx: &SecurityContext, request: CreateGroupRequest) -> Result<ResourceGroup, ResourceGroupError>;
    async fn get_group(&self, ctx: &SecurityContext, group_id: Uuid) -> Result<ResourceGroup, ResourceGroupError>;
    async fn list_groups(&self, ctx: &SecurityContext, query: ListQuery) -> Result<Page<ResourceGroup>, ResourceGroupError>;
    async fn update_group(&self, ctx: &SecurityContext, group_id: Uuid, request: UpdateGroupRequest) -> Result<ResourceGroup, ResourceGroupError>;
    async fn delete_group(&self, ctx: &SecurityContext, group_id: Uuid, force: bool) -> Result<(), ResourceGroupError>;

    // ── Hierarchy ───────────────────────────────────────────────────
    async fn list_group_depth(&self, ctx: &SecurityContext, group_id: Uuid, query: ListQuery) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    // ── Membership lifecycle ────────────────────────────────────────
    async fn add_membership(&self, ctx: &SecurityContext, request: AddMembershipRequest) -> Result<ResourceGroupMembership, ResourceGroupError>;
    async fn remove_membership(&self, ctx: &SecurityContext, request: RemoveMembershipRequest) -> Result<(), ResourceGroupError>;
    async fn list_memberships(&self, ctx: &SecurityContext, query: ListQuery) -> Result<Page<ResourceGroupMembership>, ResourceGroupError>;
}
```

## Integration Read Trait — `ResourceGroupReadHierarchy`

Narrow hierarchy-only read contract used exclusively by AuthZ plugin.

```rust
/// Used by AuthZ plugin — provides only hierarchy traversal, no memberships.
#[async_trait]
pub trait ResourceGroupReadHierarchy: Send + Sync {
    /// Matches REST `GET /groups/{group_id}/hierarchy` with OData query.
    async fn list_group_depth(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: ListQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;
}
```

## Plugin Trait — `ResourceGroupReadPluginClient`

Gateway delegates to selected scoped plugin (vendor-specific provider path).

```rust
/// Plugin hierarchy read contract. Extends `ResourceGroupReadHierarchy`.
#[async_trait]
pub trait ResourceGroupReadPluginClient: ResourceGroupReadHierarchy {
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: ListQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError>;
}
```

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
use modkit_security::SecurityContext;
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

// Hierarchy traversal — descendants
let descendants = rg_hierarchy
    .list_group_depth(&ctx, group_id, ListQuery::filter("hierarchy/depth ge 0"))
    .await?;

// Full CRUD — create group
let group = rg
    .create_group(&ctx, CreateGroupRequest {
        id: None,
        r#type: "gts.x.system.rg.type.v1~y.system.tn.tenant.v1~".into(),
        name: "Acme Corp".into(),
        parent_id: None,
        metadata: Default::default(),
    })
    .await?;
```

## Trait Hierarchy Summary

| Trait | Methods | Consumers | ClientHub key |
|-------|---------|-----------|---------------|
| `ResourceGroupClient` | 14 (full CRUD: types, groups, memberships, hierarchy) | Domain services, Apps, Admins | `dyn ResourceGroupClient` |
| `ResourceGroupReadHierarchy` | 1 (`list_group_depth`) | AuthZ plugin | `dyn ResourceGroupReadHierarchy` |
| `ResourceGroupReadPluginClient` | 1 + inherited (`list_memberships` + `list_group_depth`) | Vendor-specific plugin gateway | Scoped plugin resolution |
