//! Reconcile AM's configured platform-root tenant type during init, before bootstrap.
//! Equal content submits nothing; permitted `idp_provisioning` changes use normal
//! compatibility checks. Refusal fails startup with the identifier.
//!
//! The caller must validate the stored root binding before any registry write.

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_gts::GTS_ID_URI_PREFIX;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PublisherContext, publish_startup_entities,
};

use crate::domain::root_type::{RootTypeConfig, TENANT_TYPE_BASE};

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

/// Reconcile the configured root-type definition and wait for its admission.
///
/// # Errors
/// An invalid configuration, a refused or unsettled admission (the error
/// names the identifier and its outcome), the deadline or cancellation.
pub async fn register_root_type(
    registry: &dyn PlatformTypesRegistryApi,
    cfg: &RootTypeConfig,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let id = GtsId::try_new(cfg.validated_id().map_err(anyhow::Error::msg)?)?;
    let desired = desired_root_schema(cfg)?;
    let publisher = PublisherContext {
        name: "account-management".to_owned(),
        version: env!("CARGO_PKG_VERSION").parse()?,
    };
    publish_startup_entities(registry, &publisher, &[(id, desired)], cancel).await?;
    Ok(())
}

/// Build the single AM-owned concrete root schema.
pub fn desired_root_schema(cfg: &RootTypeConfig) -> anyhow::Result<Value> {
    // The configuration guarantees a type directly derived from the envelope.
    let type_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    Ok(json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{type_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "description": "Platform-root tenant type (no parents).",
        "type": "object",
        "allOf": [{ "$ref": format!("{GTS_ID_URI_PREFIX}{TENANT_TYPE_BASE}") }],
        "x-gts-traits": {
            "allowed_parent_types": [],
            "idp_provisioning": cfg.idp_provisioning,
        }
    }))
}

#[cfg(test)]
#[path = "root_type_tests.rs"]
mod tests;
