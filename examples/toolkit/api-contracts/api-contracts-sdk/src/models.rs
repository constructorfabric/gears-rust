//! Domain models for the [`PaymentApi`](crate::contract::PaymentApi) contract.
//!
//! All public structs and enums are `#[non_exhaustive]` so adding fields or
//! variants in a future release is non-breaking. Construct values via the
//! `::new(...)` constructors and mutate via the public fields.
//!
//! When the `grpc-client` feature is enabled, each type derives
//! [`toolkit::ProtoBridge`], which auto-generates `From`/`Into` between the
//! Rust DTO and the corresponding prost-generated stub message.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request to charge a payment.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::ChargeRequest")
)]
#[non_exhaustive]
pub struct ChargeRequest {
    /// Amount in smallest currency unit (e.g., cents).
    pub amount_cents: i64,
    /// ISO 4217 currency code (e.g., "USD").
    pub currency: String,
    /// Human-readable description.
    pub description: String,
}

impl ChargeRequest {
    /// Build a new charge request.
    #[must_use]
    pub fn new(
        amount_cents: i64,
        currency: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            amount_cents,
            currency: currency.into(),
            description: description.into(),
        }
    }
}

/// Response from a successful charge.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::ChargeResponse")
)]
#[non_exhaustive]
pub struct ChargeResponse {
    /// Unique payment identifier.
    #[cfg_attr(feature = "grpc-client", proto_bridge(via_string))]
    pub payment_id: Uuid,
    /// Current status of the payment.
    pub status: PaymentStatus,
}

impl ChargeResponse {
    /// Build a new charge response.
    #[must_use]
    pub const fn new(payment_id: Uuid, status: PaymentStatus) -> Self {
        Self { payment_id, status }
    }
}

/// Current status of a payment.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema,
)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::PaymentStatus")
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PaymentStatus {
    /// Payment is pending processing.
    #[default]
    Pending,
    /// Payment completed successfully.
    Completed,
    /// Payment failed.
    Failed,
}

/// A payment invoice.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::Invoice")
)]
#[allow(
    clippy::struct_field_names,
    reason = "invoice_id is the canonical domain identifier"
)]
#[non_exhaustive]
pub struct Invoice {
    /// Unique invoice identifier.
    #[cfg_attr(feature = "grpc-client", proto_bridge(via_string))]
    pub invoice_id: Uuid,
    /// Associated payment identifier.
    #[cfg_attr(feature = "grpc-client", proto_bridge(via_string))]
    pub payment_id: Uuid,
    /// Amount in smallest currency unit.
    pub amount_cents: i64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Invoice description.
    pub description: String,
    /// Current payment status.
    pub status: PaymentStatus,
}

impl Invoice {
    /// Build a new invoice.
    #[must_use]
    pub fn new(
        invoice_id: Uuid,
        payment_id: Uuid,
        amount_cents: i64,
        currency: impl Into<String>,
        description: impl Into<String>,
        status: PaymentStatus,
    ) -> Self {
        Self {
            invoice_id,
            payment_id,
            amount_cents,
            currency: currency.into(),
            description: description.into(),
            status,
        }
    }
}

/// Summary of a payment for streaming list responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::PaymentSummary")
)]
#[non_exhaustive]
pub struct PaymentSummary {
    /// Unique payment identifier.
    #[cfg_attr(feature = "grpc-client", proto_bridge(via_string))]
    pub payment_id: Uuid,
    /// Amount in smallest currency unit.
    pub amount_cents: i64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Current payment status.
    pub status: PaymentStatus,
}

impl PaymentSummary {
    /// Build a new payment summary.
    #[must_use]
    pub fn new(
        payment_id: Uuid,
        amount_cents: i64,
        currency: impl Into<String>,
        status: PaymentStatus,
    ) -> Self {
        Self {
            payment_id,
            amount_cents,
            currency: currency.into(),
            status,
        }
    }
}

/// Filter criteria for listing payments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[cfg_attr(feature = "grpc-client", derive(toolkit::ProtoBridge))]
#[cfg_attr(
    feature = "grpc-client",
    proto_bridge(stub = "crate::grpc::stubs::ListPaymentsFilter")
)]
#[non_exhaustive]
pub struct ListPaymentsFilter {
    /// Filter by payment status.
    pub status: Option<PaymentStatus>,
    /// Filter by currency code.
    pub currency: Option<String>,
}

impl ListPaymentsFilter {
    /// Build a filter from optional status and currency.
    #[must_use]
    pub fn new(status: Option<PaymentStatus>, currency: Option<String>) -> Self {
        Self { status, currency }
    }
}

// Marker impls so these DTOs are accepted by `toolkit::OperationBuilder`.
// `RequestApiDto` / `ResponseApiDto` are tag traits with no required methods;
// the regular `Serialize` / `Deserialize` / `ToSchema` derives above carry the
// real behavior. The `api_dto!` attribute macro is the usual ergonomic path,
// but it conflicts with `#[non_exhaustive]` + the explicit `#[derive(...)]`
// list we keep for `proto_bridge` / `JsonSchema` support.
mod _api_dto_markers {
    use super::{ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary};
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for ChargeRequest {}
    impl ResponseApiDto for ChargeResponse {}
    impl ResponseApiDto for Invoice {}
    impl ResponseApiDto for PaymentSummary {}
    impl RequestApiDto for ListPaymentsFilter {}
}
