use sea_orm::EntityTrait;

/// Defines the contract for entities that can be scoped by tenant, resource, owner, and type.
///
/// Each entity implementing this trait must explicitly declare all four scope dimensions:
/// - `tenant_col()`: Column for tenant-based isolation (multi-tenancy)
/// - `resource_col()`: Column for resource-level access (typically the primary key)
/// - `owner_col()`: Column for owner-based filtering
/// - `type_col()`: Column for type-based filtering
///
/// **Important**: No implicit defaults are allowed. Every scope dimension must be explicitly
/// specified as `Some(Column::...)` or `None` to enforce compile-time safety in secure systems.
///
/// # Example (Manual Implementation)
/// ```rust,ignore
/// impl ScopableEntity for user::Entity {
///     fn tenant_col() -> Option<Self::Column> {
///         Some(user::Column::TenantId)
///     }
///     fn resource_col() -> Option<Self::Column> {
///         Some(user::Column::Id)
///     }
///     fn owner_col() -> Option<Self::Column> {
///         None
///     }
///     fn type_col() -> Option<Self::Column> {
///         None
///     }
///     fn resolve_property(property: &str) -> Option<Self::Column> {
///         match property {
///             "owner_tenant_id" => Self::tenant_col(),
///             "id" => Self::resource_col(),
///             "owner_id" => Self::owner_col(),
///             _ => None,
///         }
///     }
///     fn scope_columns() -> Vec<Self::Column> {
///         // Exactly the columns resolve_property can return.
///         vec![user::Column::TenantId, user::Column::Id]
///     }
/// }
/// ```
///
/// # Example (Using Derive Macro)
/// ```rust,ignore
/// use toolkit_db::secure::Scopable;
///
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "users")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub email: String,
/// }
/// // Macro auto-generates resolve_property:
/// //   "owner_tenant_id" => Some(Column::TenantId)  (from tenant_col)
/// //   "id"              => Some(Column::Id)         (from resource_col)
/// //   _                 => None
/// ```
///
/// # Custom PEP Properties
/// ```rust,ignore
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "resources")]
/// #[secure(
///     tenant_col = "tenant_id",
///     resource_col = "id",
///     no_owner,
///     no_type,
///     pep_prop(department_id = "department_id"),
/// )]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub tenant_id: Uuid,
///     pub department_id: Uuid,
/// }
/// // Macro auto-generates resolve_property:
/// //   "owner_tenant_id" => Some(Column::TenantId)      (from tenant_col)
/// //   "id"              => Some(Column::Id)             (from resource_col)
/// //   "department_id"   => Some(Column::DepartmentId)   (from pep_prop)
/// //   _                 => None
/// ```
///
/// # Unrestricted Entities
/// ```rust,ignore
/// #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
/// #[sea_orm(table_name = "system_config")]
/// #[secure(unrestricted)]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: Uuid,
///     pub config_key: String,
/// }
/// ```
pub trait ScopableEntity: EntityTrait {
    /// Indicates whether this entity is explicitly marked as unrestricted.
    ///
    /// This is a compile-time flag set via `#[secure(unrestricted)]` that documents
    /// the entity's global nature (e.g., system configuration, lookup tables).
    ///
    /// When `IS_UNRESTRICTED` is true, all column methods return `None`.
    ///
    /// Default: `false` (entity participates in scoping logic)
    const IS_UNRESTRICTED: bool = false;

    /// Returns the column that stores the tenant identifier.
    ///
    /// - Multi-tenant entities: `Some(Column::TenantId)`
    /// - Global/system entities: `None`
    ///
    /// Must be explicitly specified via `tenant_col = "..."` or `no_tenant`.
    fn tenant_col() -> Option<Self::Column>;

    /// Returns the column that stores the primary resource identifier.
    ///
    /// Typically the primary key column (e.g., `Column::Id`).
    ///
    /// Must be explicitly specified via `resource_col = "..."` or `no_resource`.
    fn resource_col() -> Option<Self::Column>;

    /// Returns the column that stores the resource owner identifier.
    ///
    /// Used for owner-based access control policies.
    ///
    /// Must be explicitly specified via `owner_col = "..."` or `no_owner`.
    fn owner_col() -> Option<Self::Column>;

    /// Returns the column that stores the resource type identifier.
    ///
    /// Used for type-based filtering in polymorphic scenarios.
    ///
    /// Must be explicitly specified via `type_col = "..."` or `no_type`.
    fn type_col() -> Option<Self::Column>;

    /// Resolve an authorization property name to a database column.
    ///
    /// Maps PEP property names (e.g. `"owner_tenant_id"`) to `SeaORM` columns
    /// so the scope condition builder can translate `AccessScope` constraints
    /// into SQL `WHERE` clauses.
    ///
    /// When using `#[derive(Scopable)]`, this method is auto-generated from
    /// dimension columns and `pep_prop(...)` entries:
    /// - `tenant_col` → `"owner_tenant_id"`
    /// - `resource_col` → `"id"`
    /// - `owner_col` → `"owner_id"`
    /// - `pep_prop(custom = "column")` → `"custom"`
    ///
    /// Manual implementors must provide all property arms explicitly.
    #[must_use]
    fn resolve_property(property: &str) -> Option<Self::Column>;

    /// Every column a scope predicate can address on this entity — exactly the
    /// set [`resolve_property`](Self::resolve_property) can return.
    ///
    /// `resolve_property` answers "which column does this property mean" but
    /// cannot be enumerated, so nothing could ask the opposite question:
    /// *which* columns are scope columns at all. Two things need that answer.
    ///
    /// A property-graph declaration must expose every scope column as a graph
    /// property, because a column absent from a `PROPERTIES` list is invisible
    /// to `MATCH` — not an error, just silently unfilterable, which for a scope
    /// column means the pattern cannot be scoped
    /// (`docs/arch/secure-orm/ADR/0002`, Policy 3). And a graph element must be
    /// rejected up front when it resolves no scope column at all, rather than
    /// compiling to a deny-all traversal that looks like missing data
    /// (Policy 2).
    ///
    /// Required rather than defaulted, because a default cannot be right: it
    /// would either omit the extra properties a hand-written
    /// `resolve_property` maps (their columns then miss the `PROPERTIES` list
    /// and the pattern silently cannot filter on them), or enumerate a column
    /// like [`type_col`](Self::type_col) that no property resolves to (the
    /// entity then passes the Policy 2 gates while resolving nothing — the
    /// silent deny-all those gates exist to refuse). Manual implementors
    /// enumerate the same set their `resolve_property` can return;
    /// `#[derive(Scopable)]` generates both from one configuration so they
    /// cannot disagree.
    ///
    /// Note `type_col` is **not** a scope column: `resolve_property` has no
    /// well-known property name for it, so no scope constraint can address it.
    #[must_use]
    fn scope_columns() -> Vec<Self::Column>;
}
