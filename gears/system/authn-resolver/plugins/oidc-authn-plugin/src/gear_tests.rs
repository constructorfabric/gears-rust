//! The OIDC gear's helpers and its GTS publication.

use super::*;

#[test]
fn normalize_priority_passes_through_small_values() {
    assert_eq!(normalize_priority(0), 0);
    assert_eq!(normalize_priority(100), 100);
}

#[test]
fn normalize_priority_passes_through_i16_max_boundary() {
    assert_eq!(normalize_priority(i16::MAX as u32), i16::MAX);
}

#[test]
fn normalize_priority_clamps_values_exceeding_i16_max() {
    assert_eq!(normalize_priority(i16::MAX as u32 + 1), i16::MAX);
    assert_eq!(normalize_priority(u32::MAX), i16::MAX);
}

/// The publication boundary: the runtime publishes the configured instance
/// before `init`; `init` binds the scoped client to the admitted manifest.
mod publication {
    use std::sync::Arc;

    use authn_resolver_sdk::{AuthNResolverPluginClient, AuthNResolverPluginSpecV1};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;
    use toolkit::client_hub::{ClientHub, ClientScope};
    use toolkit::contracts::Gear;
    use toolkit::gts_publication::{GtsManifest, GtsPublicationCapability as _};
    use toolkit::{ConfigProvider, GearCtx};

    use crate::config::INSTANCE_SUFFIX;
    use crate::gear::OidcAuthNPluginGear;

    struct FixedConfig(Value);

    impl ConfigProvider for FixedConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&Value> {
            Some(&self.0)
        }
    }

    fn config() -> FixedConfig {
        FixedConfig(json!({ "config": {
            "vendor": "cf",
            "priority": 5,
            "jwt": {
                "trusted_issuers": [{ "issuer": "https://oidc/realms/platform" }],
                "claim_mapping": { "subject_tenant_id": "tenant_id" },
            },
            "s2s_oauth": {
                "discovery_url": "https://oidc/realms/platform",
                "default_subject_type": toolkit_gts::gts_id!("cf.core.security.subject_user.v1~"),
            },
        } }))
    }

    fn instance_id() -> String {
        AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX)
            .as_ref()
            .to_owned()
    }

    fn manifest() -> GtsManifest {
        OidcAuthNPluginGear::default()
            .descriptor()
            .build_gts_manifest(&config())
            .unwrap()
    }

    fn ctx() -> GearCtx {
        GearCtx::new(
            "oidc-authn-plugin",
            uuid::Uuid::new_v4(),
            Arc::new(config()),
            Arc::new(ClientHub::new()),
            CancellationToken::new(),
        )
    }

    /// The scope `init` registers the client under is the instance ID the
    /// manifest publishes: what authn-resolver selects from the registry
    /// resolves to this client.
    #[tokio::test]
    async fn the_registered_scope_is_the_published_instance() {
        let ctx = ctx();

        OidcAuthNPluginGear::default()
            .init(&ctx)
            .await
            .expect("init");

        let manifest = manifest();
        let [(published, _)] = manifest.documents() else {
            panic!("one instance: {manifest:?}");
        };
        assert!(
            ctx.client_hub()
                .try_get_scoped::<dyn AuthNResolverPluginClient>(&ClientScope::gts_id(
                    published.id()
                ))
                .is_some(),
            "bound under {published}"
        );
    }

    /// The manifest needs only the configuration, not `init`.
    #[test]
    fn the_manifest_is_the_configured_instance() {
        let manifest = manifest();
        let [(id, content)] = manifest.documents() else {
            panic!("one instance: {manifest:?}");
        };
        assert_eq!(id.id(), instance_id());
        assert_eq!(
            (&content["vendor"], &content["priority"]),
            (&json!("cf"), &json!(5))
        );
    }
}
