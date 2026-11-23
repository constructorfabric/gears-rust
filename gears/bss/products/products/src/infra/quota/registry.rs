//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! types-registry behind the quota intake's [`QuotaRegistry`] port (P-D-268), through the new
//! `TypesRegistryApi` as the caller.
//!
//! The client is read from the `ClientHub` on every call, never kept: a remote registry is wired
//! after `init`, and an embedded registry in `legacy_only` mode has none, which is an outage (503
//! `QUOTA_REGISTRY_UNAVAILABLE`), never "not registered". `NotFound` and a deleted entity are
//! "not registered"; every other failure is an outage, logged at warn without the payload.
use std::sync::Arc;

use serde_json::Value;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsInstanceId, GtsTypeId, LifecycleStatus, Origin, Projection,
    ReadOptions, TypesRegistryApi, TypesRegistryApiExt, TypesRegistryError,
};

use crate::domain::quota::ports::{
    QuotaRegistry, RegistryInstance, RegistryTypeSchema, RegistryUnavailable, RegistryVerdict,
};

/// The log target of every registry outage the intake meets.
const LOG_TARGET: &str = "bss_products.quota.registry";

/// The quota intake's view of types-registry, resolved from the hub on every call.
pub struct TypesRegistryQuotaRegistry {
    hub: Arc<ClientHub>,
}

impl TypesRegistryQuotaRegistry {
    /// An adapter over whatever `TypesRegistryApi` the hub holds at each call.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    fn api(&self) -> Result<Arc<dyn TypesRegistryApi>, RegistryUnavailable> {
        self.hub.get::<dyn TypesRegistryApi>().map_err(|_| {
            outage(
                "client",
                "",
                "types-registry is not available to this gear".to_owned(),
            )
        })
    }
}

/// An outage, logged once where it is met.
fn outage(what: &str, id: &str, text: String) -> RegistryUnavailable {
    tracing::warn!(
        target: LOG_TARGET,
        what,
        id,
        error = %text,
        "bss-products: a quota registry read failed"
    );
    RegistryUnavailable(text)
}

/// A read that failed: `Ok` for "not registered", else the outage.
fn not_found_or_outage(what: &str, id: &str, e: CanonicalError) -> Result<(), RegistryUnavailable> {
    match TypesRegistryError::from(e) {
        TypesRegistryError::NotFound { .. } => Ok(()),
        other => Err(outage(what, id, other.to_string())),
    }
}

fn options(fields: &[EntityField]) -> ReadOptions {
    ReadOptions {
        projection: Projection::Select(FieldSelection::with(fields)),
        ..ReadOptions::default()
    }
}

/// The managed resource version of an entity, which every registry entity carries.
fn resource_version(
    origin: Option<Origin>,
    what: &str,
    id: &str,
) -> Result<u64, RegistryUnavailable> {
    match origin {
        Some(Origin::Managed {
            resource_version, ..
        }) => Ok(resource_version),
        _ => Err(outage(what, id, format!("{id} has no managed origin"))),
    }
}

fn content(document: Option<Value>, what: &str, id: &str) -> Result<Value, RegistryUnavailable> {
    document.ok_or_else(|| outage(what, id, format!("{id} answered no content")))
}

#[async_trait::async_trait]
impl QuotaRegistry for TypesRegistryQuotaRegistry {
    async fn instance(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryInstance>, RegistryUnavailable> {
        let api = self.api()?;
        // Nothing is registered under text that is not an instance id.
        let Ok(gts_id) = GtsInstanceId::try_new(id) else {
            return Ok(None);
        };
        let read = api
            .get_instance(
                ctx,
                &gts_id,
                options(&[EntityField::Content, EntityField::Origin]),
            )
            .await;
        let instance = match read {
            Ok(instance) => instance,
            Err(e) => return not_found_or_outage("instance", id, e).map(|()| None),
        };
        if instance.lifecycle_status == LifecycleStatus::Deleted {
            return Ok(None);
        }
        Ok(Some(RegistryInstance {
            gts_id: id.to_owned(),
            uuid: instance.uuid,
            type_id: instance.type_id.as_ref().to_owned(),
            resource_version: resource_version(instance.origin, "instance", id)?,
            content: content(instance.content, "instance", id)?,
        }))
    }

    async fn type_schema(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryTypeSchema>, RegistryUnavailable> {
        let api = self.api()?;
        let Ok(type_id) = GtsTypeId::try_new(id) else {
            return Ok(None);
        };
        let read = api
            .get_type_schema(
                ctx,
                &type_id,
                options(&[EntityField::Content, EntityField::Origin]),
            )
            .await;
        let schema = match read {
            Ok(schema) => schema,
            Err(e) => return not_found_or_outage("type schema", id, e).map(|()| None),
        };
        if schema.lifecycle_status == LifecycleStatus::Deleted {
            return Ok(None);
        }
        let content = content(schema.content, "type schema", id)?;
        let is_abstract = content.get("x-gts-abstract") == Some(&Value::Bool(true));
        Ok(Some(RegistryTypeSchema {
            gts_id: id.to_owned(),
            uuid: schema.type_uuid,
            resource_version: resource_version(schema.origin, "type schema", id)?,
            content,
            is_abstract,
        }))
    }

    async fn validate(
        &self,
        ctx: &SecurityContext,
        type_id: &str,
        payload: &Value,
    ) -> Result<RegistryVerdict, RegistryUnavailable> {
        let api = self.api()?;
        let Ok(gts_type) = GtsTypeId::try_new(type_id) else {
            return Ok(RegistryVerdict::UnknownSchema);
        };
        let read = api
            .get_type_schema(
                ctx,
                &gts_type,
                options(&[EntityField::Content, EntityField::ResolvedSchema]),
            )
            .await;
        let schema = match read {
            Ok(schema) => schema,
            Err(e) => {
                return not_found_or_outage("schema", type_id, e)
                    .map(|()| RegistryVerdict::UnknownSchema);
            }
        };
        if schema.lifecycle_status == LifecycleStatus::Deleted {
            return Ok(RegistryVerdict::UnknownSchema);
        }
        // A live schema that does not compile rejects the records that name it; it is no outage.
        let validator = match schema.compile_payload_validator() {
            Ok(validator) => validator,
            Err(e) => {
                return Ok(RegistryVerdict::Uncompilable(format!(
                    "{type_id} does not compile: {e}"
                )));
            }
        };
        Ok(match validator.validate(payload) {
            Ok(()) => RegistryVerdict::Valid,
            Err(e) => RegistryVerdict::Invalid(e.to_string()),
        })
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
