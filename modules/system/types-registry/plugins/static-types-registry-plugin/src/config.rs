use serde::Deserialize;
use uuid::Uuid;

/// Plugin configuration for seeding static GTS entities into the Types Registry.
///
/// Entities are registered during `init()` in the configuration phase (before
/// ready-mode validation), following the same lifecycle as programmatic
/// registrations from other modules.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticTypesRegistryPluginConfig {
    /// Default tenant ID injected into entities that don't specify one.
    ///
    /// When an entity in `entities` has no `tenant_id` field, this value is
    /// automatically inserted before registration. Defaults to
    /// `modkit_security::constants::DEFAULT_TENANT_ID`.
    #[serde(default = "default_tenant_id")]
    pub default_tenant_id: Uuid,

    /// Raw GTS entity JSON values to register at startup.
    ///
    /// Each entry must be a valid GTS entity with at least an `$id` (or
    /// `gtsId`/`id`) field. Entities are registered in order.
    pub entities: Vec<serde_json::Value>,
}

fn default_tenant_id() -> Uuid {
    modkit_security::constants::DEFAULT_TENANT_ID
}

impl Default for StaticTypesRegistryPluginConfig {
    fn default() -> Self {
        Self {
            default_tenant_id: default_tenant_id(),
            entities: Vec::new(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_to_empty_entities() {
        let cfg = StaticTypesRegistryPluginConfig::default();
        assert!(cfg.entities.is_empty());
        assert_eq!(
            cfg.default_tenant_id,
            modkit_security::constants::DEFAULT_TENANT_ID
        );
    }

    #[test]
    fn config_deserializes_empty_object() {
        let cfg: StaticTypesRegistryPluginConfig = serde_json::from_str("{}").unwrap();
        assert!(cfg.entities.is_empty());
        assert_eq!(
            cfg.default_tenant_id,
            modkit_security::constants::DEFAULT_TENANT_ID
        );
    }

    #[test]
    fn config_deserializes_entities() {
        let json = r#"{"entities": [{"$id": "gts.x.test.v1~abc123", "tenant_id": "00000000-0000-0000-0000-000000000000"}]}"#;
        let cfg: StaticTypesRegistryPluginConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.entities.len(), 1);
        assert_eq!(
            cfg.entities[0]["$id"].as_str().unwrap(),
            "gts.x.test.v1~abc123"
        );
    }

    #[test]
    fn config_custom_default_tenant_id() {
        let json = r#"{"default_tenant_id": "11111111-1111-1111-1111-111111111111", "entities": []}"#;
        let cfg: StaticTypesRegistryPluginConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            cfg.default_tenant_id,
            uuid::uuid!("11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let json = r#"{"entities": [], "unexpected": true}"#;
        let result = serde_json::from_str::<StaticTypesRegistryPluginConfig>(json);
        assert!(result.is_err());
    }

    #[test]
    fn config_yaml_round_trip() {
        let yaml = r#"
entities:
  - $id: "gts.x.core.oagw.upstream.v1~abc123"
    server:
      endpoints:
        - scheme: https
          host: api.openai.com
          port: 443
    protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1"
    enabled: true
    tags: []
"#;
        let cfg: StaticTypesRegistryPluginConfig = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(cfg.entities.len(), 1);
    }
}
