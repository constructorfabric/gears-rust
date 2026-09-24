// Created: 2026-09-06 by Virtuozzo International GmbH
//! Tests for the composed setting-type schema.

use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::SETTING_TYPE_BASE;

use super::setting_type_schema;

#[test]
fn a_setting_type_derives_from_the_base_and_narrows_the_payload() {
    let key = SettingKey::contributed(
        "cf",
        "settings_demo",
        "network",
        "proxy_enabled",
        std::num::NonZeroU32::new(1).expect("non-zero"),
    )
    .expect("key");
    let schema = setting_type_schema(&key, "gts.cf.core.settings.type_bool_flag.v1~");
    assert_eq!(schema["$id"], json!(format!("gts://{key}")));
    assert_eq!(
        schema["allOf"],
        json!([{ "$ref": format!("gts://{SETTING_TYPE_BASE}") }])
    );
    assert_eq!(
        schema["properties"]["payload"],
        json!({ "$ref": "gts://gts.cf.core.settings.type_bool_flag.v1~" })
    );
}

#[test]
fn a_setting_type_carries_no_default() {
    // The Schema Default lives in the declaration alone; a `default` here would
    // be a second home the two could drift between.
    let key = SettingKey::compose("acme", "network", "enable_proxy").expect("key");
    let schema = setting_type_schema(&key, "gts.cf.core.settings.type_bool_flag.v1~");
    assert!(schema.get("default").is_none());
    assert!(schema["properties"]["payload"].get("default").is_none());
}

mod already_registered {
    //! What "already registered" means: the same type, or a conflict.

    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use settings_service_sdk::SettingKey;
    use settings_service_sdk::gts::SETTING_TYPE_BASE;
    use toolkit_canonical_errors::CanonicalError;
    use types_registry_sdk::{GtsTypeId, GtsTypeSchema, RegisterResult};

    use super::super::{TypeSchemaRegistry, TypesRegistryRegistrar, setting_type_schema};
    use crate::domain::contribution::SettingTypeRegistrar;
    use crate::domain::error::DomainError;

    const BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";
    const PORT: &str = "gts.cf.core.settings.type_port.v1~";

    /// A registry that already holds a schema for every key and says so.
    struct Holding {
        held: Option<GtsTypeSchema>,
    }

    #[async_trait]
    impl TypeSchemaRegistry for Holding {
        async fn register(&self, _schema: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
            Ok(vec![RegisterResult::Err {
                gts_id: None,
                error: CanonicalError::from(DomainError::Conflict {
                    detail: "already registered".to_owned(),
                }),
            }])
        }

        async fn registered(&self, _type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
            self.held
                .clone()
                .ok_or_else(|| CanonicalError::from(DomainError::NotFound { resource: "type" }))
        }
    }

    fn key() -> SettingKey {
        SettingKey::compose("acme", "network", "enable_proxy").expect("key")
    }

    fn holding(value_type_id: &str) -> Holding {
        let key = key();
        let base = GtsTypeSchema::try_new(
            GtsTypeId::new(SETTING_TYPE_BASE),
            json!({ "$id": format!("gts://{SETTING_TYPE_BASE}"), "type": "object" }),
            None,
            None,
        )
        .expect("the base is a valid root type");
        Holding {
            held: Some(
                GtsTypeSchema::try_new(
                    GtsTypeId::new(&key.to_string()),
                    setting_type_schema(&key, value_type_id),
                    None,
                    Some(Arc::new(base)),
                )
                .expect("a valid derived type schema"),
            ),
        }
    }

    #[tokio::test]
    async fn the_same_type_registered_already_is_idempotent_success() {
        let registrar = TypesRegistryRegistrar::new(holding(BOOL));
        registrar
            .register_setting_type(&key(), BOOL)
            .await
            .expect("a retry reuses the type");
    }

    #[tokio::test]
    async fn a_type_registered_for_another_value_type_is_a_conflict_not_success() {
        // The schema at `gts://K` says the payload conforms to one value type;
        // a declaration about to be inserted names another. Accepting that as
        // idempotent success would persist the drift with no sign.
        let registrar = TypesRegistryRegistrar::new(holding(PORT));
        let err = registrar
            .register_setting_type(&key(), BOOL)
            .await
            .expect_err("drift is refused");
        assert!(
            matches!(&err, DomainError::Conflict { detail }
                if detail.contains(PORT) && detail.contains(BOOL)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_registry_that_cannot_show_what_it_holds_is_unavailable() {
        let registrar = TypesRegistryRegistrar::new(Holding { held: None });
        let err = registrar
            .register_setting_type(&key(), BOOL)
            .await
            .expect_err("nothing to compare against");
        assert!(matches!(err, DomainError::Unavailable { .. }), "{err:?}");
    }
}
