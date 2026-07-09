// Created: 2026-06-10 by Constructor Tech
//! GTS plugin-spec scaffolding for the cluster plugin contract (DESIGN §3.4).
//!
//! This gear defines the GTS type for cluster plugin instances. Follow-up
//! plugin crates (Postgres, K8s, Redis, NATS, etcd, standalone) register an
//! instance of this type with the types-registry so the wiring crate can
//! discover them and resolve a backend per profile per primitive.
//!
//! Serde/schemars are pulled in solely for this plugin-discovery spec; the
//! coordination contract types (`Cache*`, `Lock*`, `Leader*`, `Service*`)
//! remain serde-free per `cpt-cf-clst-constraint-no-serde`.

use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

/// GTS type definition for cluster plugin instances.
///
/// Each plugin registers an instance of this type with its vendor-specific
/// instance ID. The wiring crate discovers plugins by querying types-registry
/// for instances matching this schema.
///
/// # Instance ID Format
///
/// Each plugin instance carries a vendor-specific instance segment, which is
/// intentionally distinct from this type's own registered segment
/// (`cf.core.cluster.plugin.v1`, see `type_id` below):
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~<vendor>.<package>.<plugin>.plugin.v1~
/// ```
///
/// where `<vendor>` is the plugin author's registered vendor (e.g. `cf`). The
/// example below mints such a segment for the built-in standalone plugin.
///
/// # Example
///
/// ```ignore
/// // Plugin generates its instance ID
/// let instance_id = ClusterPluginSpecV1::gts_make_instance_id(
///     "cf.builtin.standalone_cluster.plugin.v1"
/// );
///
/// // Plugin creates instance data
/// let instance = PluginV1::<ClusterPluginSpecV1> {
///     id: instance_id.clone(),
///     priority: 100,
///     properties: ClusterPluginSpecV1,
/// };
///
/// // Register with types-registry
/// registry.register(&ctx, vec![serde_json::to_value(&instance)?]).await?;
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = "gts.cf.toolkit.plugins.plugin.v1~cf.core.cluster.plugin.v1~",
    description = "Cluster plugin specification",
    properties = "",
)]
pub struct ClusterPluginSpecV1;

#[cfg(test)]
mod tests {
    use toolkit_gts::{GtsSchema, all_inventory_type_schemas};

    use super::ClusterPluginSpecV1;

    /// The cluster plugin spec must be in the link-time type-schema inventory so
    /// follow-up plugins can register an instance of it and be discovered.
    #[test]
    fn cluster_plugin_spec_is_registered() {
        let blob = serde_json::to_string(
            &all_inventory_type_schemas().expect("collect inventory type schemas"),
        )
        .expect("serialize inventory type schemas");
        assert!(
            blob.contains(ClusterPluginSpecV1::TYPE_ID),
            "cluster plugin spec `{}` not registered in inventory",
            ClusterPluginSpecV1::TYPE_ID
        );
    }
}
