//! Versioned receipt storage. Decoding never recalculates an issued digest or defaults a field.
mod scalars;
mod v1;
use crate::infra::storage::RepoError;
use bss_pricing_sdk::acceptance::{AcceptanceReceipt, HeldBindings};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "schema_version", deny_unknown_fields)]
enum AcceptanceWire {
    #[serde(rename = "1")]
    V1 { receipt: v1::AcceptanceReceipt },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "schema_version", deny_unknown_fields)]
enum HoldWire {
    #[serde(rename = "1")]
    V1 { receipt: v1::HeldBindings },
}
fn invalid(error: impl std::fmt::Display) -> RepoError {
    RepoError::CorruptRow(format!("commercial receipt: {error}"))
}
/// Encode an issued schema-1 acceptance without recomputing any digest.
/// # Errors
/// Unsupported `BillingTerms` version or unrepresentable scalar.
pub fn encode_acceptance(receipt: &AcceptanceReceipt) -> Result<String, RepoError> {
    if receipt.query.billing_terms.schema_version != 1 {
        return Err(invalid("unsupported BillingTerms schema"));
    }
    serde_json::to_string(&AcceptanceWire::V1 {
        receipt: receipt.clone().into(),
    })
    .map_err(invalid)
}
/// Decode the issued version, preserving its snapshot and digests.
/// # Errors
/// Unsupported receipt/BillingTerms versions, duplicate/unknown fields or invalid scalars.
pub fn decode_acceptance(raw: &str) -> Result<AcceptanceReceipt, RepoError> {
    let AcceptanceWire::V1 { receipt } = serde_json::from_str(raw).map_err(invalid)?;
    let receipt: AcceptanceReceipt = receipt.into();
    if receipt.query.billing_terms.schema_version != 1 {
        return Err(invalid("unsupported BillingTerms schema"));
    }
    Ok(receipt)
}
/// Encode the original held bindings and activation time.
/// # Errors
/// Unrepresentable exact scalar.
pub fn encode_hold(receipt: &HeldBindings) -> Result<String, RepoError> {
    serde_json::to_string(&HoldWire::V1 {
        receipt: receipt.clone().into(),
    })
    .map_err(invalid)
}
/// Decode the original held bindings; live eligibility is a separate operation.
/// # Errors
/// Unsupported schema, duplicate/unknown fields or invalid exact scalar.
pub fn decode_hold(raw: &str) -> Result<HeldBindings, RepoError> {
    let HoldWire::V1 { receipt } = serde_json::from_str(raw).map_err(invalid)?;
    Ok(receipt.into())
}
/// Lossless UTC nanosecond relational timestamp, identical on both backends.
/// # Errors
/// Instants outside the four-digit positive-year profile.
pub fn timestamp(at: time::OffsetDateTime) -> Result<String, RepoError> {
    scalars::instant_text(at).map_err(invalid)
}
