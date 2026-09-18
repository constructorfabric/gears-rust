//! Registration of Account Management's configured platform-root tenant type.
//!
//! This intentionally uses the existing process-local [`TypesRegistryClient`].
//! During gear initialization the client stages the schema in Types Registry's
//! configuration catalogue. Types Registry's existing `post_init` ready
//! transition then performs full GTS semantic validation before any stateful
//! gear starts.
//!
//! This path does not provide persistence or cross-replica coordination. Those
//! guarantees belong to the Types Registry P0 consumer migration tracked by
//! #4627.

use serde_json::{Value, json};
use toolkit_gts::GTS_ID_URI_PREFIX;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::domain::root_type::RootTypeConfig;

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

/// Register the configured root-type definition in the process-local registry.
///
/// Registration is create-if-absent and idempotent for an identical document.
/// The existing client rejects a second document under the same identifier, so
/// definition drift is startup-fatal. Full semantic validation is completed by
/// Types Registry's ready transition after every gear has finished `init()`.
pub async fn register_root_type(
    registry: &dyn TypesRegistryClient,
    cfg: &RootTypeConfig,
) -> anyhow::Result<()> {
    let expected_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    let desired = desired_root_schema(cfg)?;
    let mut results = registry
        .register_type_schemas(vec![desired])
        .await
        .map_err(|error| anyhow::anyhow!("root tenant type registration failed: {error}"))?;

    if results.len() != 1 {
        anyhow::bail!(
            "root tenant type registration returned {} results for one schema",
            results.len()
        );
    }

    match results.remove(0) {
        RegisterResult::Ok { gts_id } if gts_id == expected_id => Ok(()),
        RegisterResult::Ok { gts_id } => anyhow::bail!(
            "root tenant type registration returned unexpected id {gts_id}; expected {expected_id}"
        ),
        RegisterResult::Err { gts_id, error } => {
            let attempted = gts_id.as_deref().unwrap_or(expected_id);
            anyhow::bail!(
                "root tenant type {attempted} conflicts with the Account Management-owned definition or is invalid: {error}"
            )
        }
    }
}

/// Build the single AM-owned concrete root schema.
pub fn desired_root_schema(cfg: &RootTypeConfig) -> anyhow::Result<Value> {
    let type_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    let parent = types_registry_sdk::GtsTypeSchema::derive_parent_type_id(type_id)
        .ok_or_else(|| anyhow::anyhow!("root tenant type {type_id} has no concrete parent"))?;
    Ok(json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{type_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "description": "Platform-root tenant type owned by Account Management (no parents).",
        "type": "object",
        "allOf": [{ "$ref": format!("{GTS_ID_URI_PREFIX}{parent}") }],
        "x-gts-traits": {
            "allowed_parent_types": [],
            "idp_provisioning": cfg.idp_provisioning,
        }
    }))
}

#[cfg(test)]
#[path = "root_type_tests.rs"]
mod tests;
