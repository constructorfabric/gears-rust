//! In-memory mock implementation of `PaymentService` domain logic.

use std::collections::HashMap;
use std::sync::Arc;

use api_contracts_sdk::error::PaymentResourceError;
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentStatus, PaymentSummary,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use parking_lot::RwLock;
use uuid::Uuid;

/// In-memory payment record.
struct PaymentRecord {
    payment_id: Uuid,
    amount_cents: i64,
    currency: String,
    description: String,
    status: PaymentStatus,
}

/// In-memory `PaymentService` implementation for the proof of concept.
pub struct PaymentDomainService {
    payments: RwLock<HashMap<Uuid, PaymentRecord>>,
}

impl PaymentDomainService {
    /// Create an empty domain service.
    #[must_use]
    pub fn new() -> Self {
        Self {
            payments: RwLock::new(HashMap::new()),
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
