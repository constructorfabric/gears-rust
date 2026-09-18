// Created: 2026-09-06 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-module-contributions-type:p1
//! Registration of a setting's own type in the types registry.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::SETTING_TYPE_BASE;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::domain::contribution::SettingTypeRegistrar;
use crate::domain::error::DomainError;

/// The schema of a concrete setting type.
///
/// Derived from the abstract `setting_type` base through `allOf`, with the
/// open `payload` of the base narrowed to the value type the declaration
/// names. It carries **no** `default`: the Schema Default lives in the
/// declaration's `default_value` alone, and registration must not give it a
/// second home.
#[must_use]
pub fn setting_type_schema(key: &SettingKey, value_type_id: &str) -> Value {
    json!({
        "$id": format!("gts://{key}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": format!(
            "Setting `{}` in category `{}`; values conform to `{value_type_id}`.",
            key.leaf_slug(),
            key.category_slug()
        ),
        "allOf": [{ "$ref": format!("gts://{SETTING_TYPE_BASE}") }],
        "properties": {
            "payload": { "$ref": format!("gts://{value_type_id}") }
        },
        "required": ["payload"]
    })
}

/// [`SettingTypeRegistrar`] over the types registry client.
pub struct TypesRegistryRegistrar {
    types: Arc<dyn TypesRegistryClient>,
}

impl TypesRegistryRegistrar {
    /// Register through this client.
    pub fn new(types: Arc<dyn TypesRegistryClient>) -> Self {
        Self { types }
    }
}

#[async_trait]
impl SettingTypeRegistrar for TypesRegistryRegistrar {
    async fn register_setting_type(
        &self,
        key: &SettingKey,
        value_type_id: &str,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-1
        let schema = setting_type_schema(key, value_type_id);
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-1
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-2
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-3
        // @cpt-begin:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-4
        let results = self
            .types
            .register_type_schemas(vec![schema])
            .await
            .map_err(|e| DomainError::Unavailable {
                detail: format!("types registry: register_type_schemas: {e}"),
            })?;
        for result in results {
            if let RegisterResult::Err { error, .. } = result {
                return Err(match error {
                    // Idempotent: a retry after a failed insert reuses the type
                    // rather than minting a second one.
                    CanonicalError::AlreadyExists { .. } => return Ok(()),
                    // The base is registered at this gear's init; its absence
                    // means the process is not the one that started.
                    CanonicalError::FailedPrecondition { .. } => DomainError::Unavailable {
                        detail: format!(
                            "types registry refused `{key}`: the `{SETTING_TYPE_BASE}` base is \
                             not registered — {error}"
                        ),
                    },
                    other => DomainError::Internal {
                        diagnostic: format!("types registry refused `{key}`: {other}"),
                    },
                });
            }
        }
        Ok(())
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-4
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-3
        // @cpt-end:cpt-cf-settings-service-algo-module-contributions-type:p1:inst-mc-type-2
    }
}

#[cfg(test)]
#[path = "setting_type_registrar_tests.rs"]
mod setting_type_registrar_tests;
