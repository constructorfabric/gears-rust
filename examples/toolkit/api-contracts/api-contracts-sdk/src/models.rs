//! Domain models for the [`PaymentApi`](crate::contract::PaymentApi) contract.
//!
//! All public structs and enums are `#[non_exhaustive]` so adding fields or
//! variants in a future release is non-breaking. Construct values via the
//! `::new(...)` constructors and mutate via the public fields.
//!
//! When the `grpc-client` feature is enabled, each **v1** type derives
//! [`toolkit::ProtoBridge`], which auto-generates `From`/`Into` between the
//! Rust DTO and the corresponding prost-generated stub message. The v2 DTOs at
//! the bottom of this file are deliberately REST-only and carry no
//! `proto_bridge` attributes — v2 has no gRPC projection in this example.

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

// ---------------------------------------------------------------------------
// v2 models (ADR-0007 parallel versioned contract)
// ---------------------------------------------------------------------------
//
// These exist because v2 makes a **breaking** change to the charge payload that
// cannot ship inside v1: `amount_cents` is renamed to `amount_minor` (a rename
// breaks every existing caller) and `idempotency_key` is a **new required
// field** (an old caller's payload would no longer validate). Additive changes
// — a new optional field, a new enum variant — would have stayed in v1 instead.
//
// No `proto_bridge` attributes: v2 is REST-only in this example (adding a gRPC
// projection would need a second `.proto`, `include_proto!`, and per-DTO stubs).

/// Request to charge a payment — **v2** shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ChargeV2Request {
    /// Amount in the currency's minor unit (renamed from v1 `amount_cents`).
    pub amount_minor: i64,
    /// ISO 4217 currency code (e.g., "USD").
    pub currency: String,
    /// Caller-supplied key that makes a retried charge safe. Required in v2.
    ///
    /// Unique only **within the caller's own security context**: implementations
    /// MUST key their dedupe store on `(tenant, subject, idempotency_key)` taken
    /// from the request's `SecurityContext`, never on this raw string alone —
    /// otherwise one tenant can replay another tenant's key and be handed their
    /// `ChargeV2Response`.
    pub idempotency_key: String,
}

impl ChargeV2Request {
    /// Build a new v2 charge request.
    #[must_use]
    pub fn new(
        amount_minor: i64,
        currency: impl Into<String>,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            amount_minor,
            currency: currency.into(),
            idempotency_key: idempotency_key.into(),
        }
    }
}

/// Response from a successful charge — **v2** shape.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[non_exhaustive]
pub struct ChargeV2Response {
    /// Unique payment identifier.
    pub payment_id: Uuid,
    /// Current status of the payment.
    pub status: PaymentStatus,
    /// Amount actually authorized, echoed back in the currency's minor unit.
    pub amount_minor: i64,
    /// ISO 4217 currency code the charge was authorized in.
    pub currency: String,
}

impl ChargeV2Response {
    /// Build a new v2 charge response.
    #[must_use]
    pub fn new(
        payment_id: Uuid,
        status: PaymentStatus,
        amount_minor: i64,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            payment_id,
            status,
            amount_minor,
            currency: currency.into(),
        }
    }
}

// Marker impls so these DTOs are accepted by `toolkit::OperationBuilder`.
// `RequestApiDto` / `ResponseApiDto` are tag traits with no required methods;
// the regular `Serialize` / `Deserialize` / `ToSchema` derives above carry the
// real behavior. The `api_dto!` attribute macro is the usual ergonomic path,
// but it conflicts with `#[non_exhaustive]` + the explicit `#[derive(...)]`
// list we keep for `proto_bridge` / `JsonSchema` support.
mod _api_dto_markers {
    use super::{
        ChargeRequest, ChargeResponse, ChargeV2Request, ChargeV2Response, Invoice,
        ListPaymentsFilter, PaymentSummary,
    };
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for ChargeRequest {}
    impl ResponseApiDto for ChargeResponse {}
    impl ResponseApiDto for Invoice {}
    impl ResponseApiDto for PaymentSummary {}
    impl RequestApiDto for ListPaymentsFilter {}

    // v2 payloads (`Invoice` is reused from v1 and already marked above).
    impl RequestApiDto for ChargeV2Request {}
    impl ResponseApiDto for ChargeV2Response {}
}
