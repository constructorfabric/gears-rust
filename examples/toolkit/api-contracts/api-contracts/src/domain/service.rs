//! In-memory mock implementation of `PaymentApi` domain logic.

use std::collections::HashMap;
use std::sync::Arc;

use api_contracts_sdk::error::PaymentResourceError;
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, ChargeV2Request, ChargeV2Response, Invoice, ListPaymentsFilter,
    PaymentStatus, PaymentSummary,
};
use parking_lot::RwLock;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// In-memory payment record.
struct PaymentRecord {
    payment_id: Uuid,
    amount_cents: i64,
    currency: String,
    description: String,
    status: PaymentStatus,
}

/// Idempotency-store key for v2 charges.
///
/// Scoped by the **caller's identity**, not by the raw client-supplied string:
/// `(subject_tenant_id, subject_id, idempotency_key)`. Keying on the bare key
/// would let one tenant replay another tenant's key and be handed their
/// `ChargeV2Response` — see the contract docs on
/// [`ChargeV2Request::idempotency_key`](api_contracts_sdk::models::ChargeV2Request).
type IdempotencyKey = (Uuid, Uuid, String);

/// In-memory `PaymentApi` implementation for the proof of concept.
///
/// One domain store backs **both** contract versions (v1 `PaymentApi` and v2
/// `PaymentApiV2`): a major version changes the wire contract, not the
/// business data, so a payment charged through either version is visible to
/// both.
#[domain_model]
pub struct PaymentDomainService {
    payments: RwLock<HashMap<Uuid, PaymentRecord>>,
    /// v2 idempotent-charge dedupe: caller-scoped key → the payment it created.
    idempotency: RwLock<HashMap<IdempotencyKey, Uuid>>,
}

impl PaymentDomainService {
    /// Create an empty domain service.
    #[must_use]
    pub fn new() -> Self {
        Self {
            payments: RwLock::new(HashMap::new()),
            idempotency: RwLock::new(HashMap::new()),
        }
    }

    /// Charge a payment — creates a new pending payment record.
    ///
    /// # Errors
    ///
    /// Returns `CanonicalError` on invalid input.
    #[allow(clippy::unnecessary_wraps, reason = "real impl would be fallible")]
    pub fn charge(
        &self,
        _ctx: &SecurityContext,
        req: &ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        let payment_id = Uuid::new_v4();
        let record = PaymentRecord {
            payment_id,
            amount_cents: req.amount_cents,
            currency: req.currency.clone(),
            description: req.description.clone(),
            status: PaymentStatus::Pending,
        };
        self.payments.write().insert(payment_id, record);
        Ok(ChargeResponse::new(payment_id, PaymentStatus::Pending))
    }

    /// Charge a payment — **v2**: an idempotent write keyed by
    /// `idempotency_key`.
    ///
    /// Replaying the same key from the same caller returns the original
    /// outcome instead of charging twice, which is what makes the v2 REST
    /// method safe to mark `#[retryable]`. The dedupe key is scoped by the
    /// caller's `(tenant, subject)` taken from `ctx`, so one tenant can never
    /// replay another tenant's key.
    ///
    /// # Errors
    ///
    /// Returns `CanonicalError` on invalid input.
    #[allow(clippy::unnecessary_wraps, reason = "real impl would be fallible")]
    pub fn charge_v2(
        &self,
        ctx: &SecurityContext,
        req: &ChargeV2Request,
    ) -> Result<ChargeV2Response, CanonicalError> {
        let key: IdempotencyKey = (
            ctx.subject_tenant_id(),
            ctx.subject_id(),
            req.idempotency_key.clone(),
        );

        // Replay: return the original payment without creating a second one.
        if let Some(&payment_id) = self.idempotency.read().get(&key) {
            return Ok(ChargeV2Response::new(
                payment_id,
                PaymentStatus::Pending,
                req.amount_minor,
                req.currency.clone(),
            ));
        }

        let payment_id = Uuid::new_v4();
        let record = PaymentRecord {
            payment_id,
            // v2 renamed the field; the stored amount is the same minor unit.
            amount_cents: req.amount_minor,
            currency: req.currency.clone(),
            // v2 dropped `description` from the request payload.
            description: format!("v2 charge {}", req.idempotency_key),
            status: PaymentStatus::Pending,
        };
        self.payments.write().insert(payment_id, record);
        self.idempotency.write().insert(key, payment_id);

        Ok(ChargeV2Response::new(
            payment_id,
            PaymentStatus::Pending,
            req.amount_minor,
            req.currency.clone(),
        ))
    }

    /// Get an invoice by payment ID.
    ///
    /// # Errors
    ///
    /// Returns `CanonicalError::NotFound` if the payment does not exist.
    pub fn get_invoice(
        &self,
        _ctx: &SecurityContext,
        invoice_id: &str,
    ) -> Result<Invoice, CanonicalError> {
        let id = Uuid::parse_str(invoice_id).map_err(|_| {
            PaymentResourceError::not_found(format!("invalid invoice ID: {invoice_id}"))
                .with_resource(invoice_id)
                .create()
        })?;

        let payments = self.payments.read();
        let record = payments.get(&id).ok_or_else(|| {
            PaymentResourceError::not_found(format!("invoice not found: {invoice_id}"))
                .with_resource(invoice_id)
                .create()
        })?;

        Ok(Invoice::new(
            record.payment_id,
            record.payment_id,
            record.amount_cents,
            record.currency.clone(),
            record.description.clone(),
            record.status,
        ))
    }

    /// List payments as a stream, optionally filtered.
    pub fn list_payments(
        self: &Arc<Self>,
        _ctx: &SecurityContext,
        filter: &ListPaymentsFilter,
    ) -> api_contracts_sdk::contract::PaymentStream<PaymentSummary> {
        let snapshot: Vec<PaymentSummary> = {
            let payments = self.payments.read();
            payments
                .values()
                .filter(|r| filter.status.as_ref().is_none_or(|s| *s == r.status))
                .filter(|r| filter.currency.as_ref().is_none_or(|c| *c == r.currency))
                .map(|r| {
                    PaymentSummary::new(r.payment_id, r.amount_cents, r.currency.clone(), r.status)
                })
                .collect()
        };

        Box::pin(async_stream::try_stream! {
            for item in snapshot {
                yield item;
            }
        })
    }
}

impl Default for PaymentDomainService {
    fn default() -> Self {
        Self::new()
    }
}
