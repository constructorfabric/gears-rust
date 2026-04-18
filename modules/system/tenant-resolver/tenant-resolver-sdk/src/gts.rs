//! GTS schema definitions for tenant resolver plugins.
//!
//! This module defines the GTS type for tenant resolver plugin instances.
//! Plugins register instances of this type with the types-registry to be
//! discovered by the gateway.

use gts_macros::struct_to_gts_schema;
use modkit::gts::BaseModkitPluginV1;

/// GTS type definition for tenant resolver plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The gateway discovers plugins by querying types-registry
/// for instances matching this schema.
///
/// # Instance ID Format
///
/// ```text
/// gts.x.core.modkit.plugin.v1~<vendor>.<package>.tenant_resolver.plugin.v1~
/// ```
///
/// # Example
///
/// ```ignore
/// // Plugin generates its instance ID
/// let instance_id = TenantResolverPluginSpecV1::gts_make_instance_id(
///     "hyperspot.builtin.static_tenant_resolver.plugin.v1"
/// );
///
/// // Plugin creates instance data
/// let instance = BaseModkitPluginV1::<TenantResolverPluginSpecV1> {
///     id: instance_id.clone(),
///     priority: 100,
///     properties: TenantResolverPluginSpecV1,
/// };
///
/// // Register with types-registry
/// registry.register(&ctx, vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[struct_to_gts_schema(
    dir_path = "schemas",
    base = BaseModkitPluginV1,
    schema_id = "gts.x.core.modkit.plugin.v1~x.core.tenant_resolver.plugin.v1~",
    description = "Tenant Resolver plugin specification",
    properties = ""
)]
pub struct TenantResolverPluginSpecV1;

/// GTS type path for the tenant resource group type.
///
/// This type path identifies RG groups that represent tenants.
/// The tenant RG type is created externally (via API/config) with:
/// - `is_tenant: true` (instances create their own tenant scope)
/// - `can_be_root: true` (root tenants allowed)
/// - `metadata_schema` with `status` (`TenantStatus`) and `self_managed` (barrier) fields
///
/// The `rg-tr-plugin` only reads groups of this type — it does not seed the type itself.
pub const TENANT_RG_TYPE_PATH: &str = "gts.cf.core.rg.type.v1~x.system.tn.tenant.v1~";
