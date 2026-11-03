//! AM's configured platform-root tenant type, published by the `startup` task with
//! AM's declarations, before bootstrap. Equal content submits nothing; permitted
//! `idp_provisioning` changes use normal compatibility checks.
//!
//! The caller must validate the stored root binding before any registry write.

use serde_json::{Value, json};
use toolkit_gts::GTS_ID_URI_PREFIX;

use crate::domain::root_type::{RootTypeConfig, TENANT_TYPE_BASE};

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

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
