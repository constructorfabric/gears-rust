// Created: 2026-09-06 by Virtuozzo International GmbH
// @cpt-dod:cpt-cf-settings-service-dod-module-contributions-type:p1
//! Registration of a setting's own type in the types registry.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use settings_service_sdk::gts::SETTING_TYPE_BASE;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::{GtsTypeSchema, RegisterResult, TypesRegistryClient};

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

/// The registry as the registrar needs it: register a schema, and read back
/// the one already there.
///
/// A narrower seam than the whole registry client, so the registrar's answer
/// to "already registered" can be exercised without standing up the registry.
#[async_trait]
pub trait TypeSchemaRegistry: Send + Sync {
    /// Register one type schema; the per-schema outcome as the registry
    /// reports it.
    ///
    /// # Errors
    /// The registry's own, when the call itself fails.
    async fn register(&self, schema: Value) -> Result<Vec<RegisterResult>, CanonicalError>;

    /// The schema registered under `type_id`.
    ///
    /// # Errors
    /// The registry's own, including not-found.
    async fn registered(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError>;
}

#[async_trait]
impl TypeSchemaRegistry for Arc<dyn TypesRegistryClient> {
    async fn register(&self, schema: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.register_type_schemas(vec![schema]).await
    }

    async fn registered(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.get_type_schema(type_id).await
    }
}

/// [`SettingTypeRegistrar`] over a [`TypeSchemaRegistry`] — in production,
/// the types registry client.
pub struct TypesRegistryRegistrar<R = Arc<dyn TypesRegistryClient>> {
    types: R,
}

impl<R: TypeSchemaRegistry> TypesRegistryRegistrar<R> {
    /// Register through this registry.
    pub const fn new(types: R) -> Self {
        Self { types }
    }
}

#[async_trait]
impl<R: TypeSchemaRegistry> SettingTypeRegistrar for TypesRegistryRegistrar<R> {
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
        let results = self.types.register(schema).await.map_err(|e| {
            DomainError::dependency_unavailable("types registry", "register the setting type", e)
        })?;
        // One schema went out, so one result comes back. An empty answer or a
        // second result is not a registration of this type, whatever it says.
        let [result] =
            <[RegisterResult; 1]>::try_from(results).map_err(|results| DomainError::Internal {
                diagnostic: format!(
                    "types registry answered {} results for the one schema of `{key}`",
                    results.len()
                ),
            })?;
        match result {
            // Success names the type it registered; only this key's counts.
            RegisterResult::Ok { gts_id } => {
                let own = key.to_string();
                if gts_id.strip_prefix("gts://").unwrap_or(&gts_id) != own {
                    return Err(DomainError::Internal {
                        diagnostic: format!(
                            "types registry reported `{gts_id}` registered for the schema of \
                             `{own}`"
                        ),
                    });
                }
            }
            RegisterResult::Err { error, .. } => {
                return Err(match error {
                    // Idempotent: a retry after a failed insert reuses the type
                    // rather than minting a second one — provided it is the
                    // same type. A schema that narrows its payload to another
                    // value type is drift, not a retry.
                    CanonicalError::AlreadyExists { .. } => {
                        return self.confirm_same_type(key, value_type_id).await;
                    }
                    // The base is registered at this gear's init; its absence
                    // means the process is not the one that started.
                    CanonicalError::FailedPrecondition { .. } => {
                        DomainError::dependency_unavailable(
                            "types registry",
                            &format!(
                                "register `{key}`: the `{SETTING_TYPE_BASE}` base is not registered"
                            ),
                            error,
                        )
                    }
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

impl<R: TypeSchemaRegistry> TypesRegistryRegistrar<R> {
    /// "Already registered" is success only for the same type: the schema the
    /// registry holds for `key` has to narrow its payload to `value_type_id`.
    /// One that names another value type would leave the registered type
    /// identity and the declaration's value shape disagreeing with no sign —
    /// a partial failure retried under a changed contribution does exactly
    /// that — so it is a conflict carrying both sides.
    async fn confirm_same_type(
        &self,
        key: &SettingKey,
        value_type_id: &str,
    ) -> Result<(), DomainError> {
        let held = self.types.registered(&key.to_string()).await.map_err(|e| {
            DomainError::dependency_unavailable(
                "types registry",
                "read back the already-registered setting type",
                e,
            )
        })?;
        let wanted = format!("gts://{value_type_id}");
        let registered = held
            .raw_schema
            .pointer("/properties/payload/$ref")
            .and_then(Value::as_str);
        if registered == Some(wanted.as_str()) {
            return Ok(());
        }
        Err(DomainError::Conflict {
            detail: format!(
                "the type registered for `{key}` narrows its payload to `{}`, not to `{wanted}`; \
                 the declaration's value type and its registered type would disagree",
                registered.unwrap_or("nothing")
            ),
        })
    }
}

#[cfg(test)]
#[path = "setting_type_registrar_tests.rs"]
mod setting_type_registrar_tests;
