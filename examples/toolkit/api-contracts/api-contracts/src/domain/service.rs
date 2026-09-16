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
    /// The tenant that created the record, captured from the charging caller's
    /// `ctx.subject_tenant_id()`. Every read path filters on it so one tenant
    /// never sees another's payments (#4740).
    owner_tenant_id: Uuid,
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
        ctx: &SecurityContext,
        req: &ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        let payment_id = Uuid::new_v4();
        let record = PaymentRecord {
            payment_id,
            owner_tenant_id: ctx.subject_tenant_id(),
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
            owner_tenant_id: ctx.subject_tenant_id(),
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

    /// The filtered payments as a plain `Vec`, shared by both streaming
    /// methods. They differ only in their item error type, so the selection
    /// itself has no business being written twice.
    ///
    /// Scoped to `tenant` (the caller's `subject_tenant_id`) first, so a stream
    /// only ever yields the caller's own tenant's payments — the isolation
    /// `charge_v2` already applies to its idempotency store, now on the read
    /// path too (#4740).
    fn snapshot(
        self: &Arc<Self>,
        tenant: Uuid,
        filter: &ListPaymentsFilter,
    ) -> Vec<PaymentSummary> {
        let payments = self.payments.read();
        payments
            .values()
            .filter(|r| r.owner_tenant_id == tenant)
            .filter(|r| filter.status.as_ref().is_none_or(|s| *s == r.status))
            .filter(|r| filter.currency.as_ref().is_none_or(|c| *c == r.currency))
            .map(|r| {
                PaymentSummary::new(r.payment_id, r.amount_cents, r.currency.clone(), r.status)
            })
            .collect()
    }

    /// List payments as a stream, optionally filtered.
    pub fn list_payments(
        self: &Arc<Self>,
        ctx: &SecurityContext,
        filter: &ListPaymentsFilter,
    ) -> api_contracts_sdk::contract::PaymentStream<PaymentSummary> {
        let snapshot = self.snapshot(ctx.subject_tenant_id(), filter);

        Box::pin(async_stream::try_stream! {
            for item in snapshot {
                yield item;
            }
        })
    }

    /// Open a payment feed: validate the filter first, then hand back the
    /// items.
    ///
    /// The validation is the whole point of the separate open. `list_payments`
    /// above has nowhere to report a bad filter except as the stream's first
    /// item, which a consumer only discovers after it already holds a stream it
    /// believes is live. Here a rejected filter is an `Err` before any stream
    /// exists, so "the feed is open" and "the feed is usable" are the same
    /// statement.
    ///
    /// Its error is a `CanonicalError`, like every other method. A fallible
    /// open would ideally hand the caller a *typed variant with payload fields*
    /// (which partitions are `unseeded`, and a `recovery_hint`), but
    /// `CanonicalError` cannot yet carry that as a first-class typed payload —
    /// so the failure is reported through the canonical AIP-193 violation slots
    /// instead (`field_violation` / `precondition_violation`), and the richer
    /// typed form is deferred to a future canonical-error-macro change. See
    /// #4734.
    ///
    /// # Errors
    ///
    /// - A `FailedPrecondition` `CanonicalError` when `filter.status` is
    ///   [`PaymentStatus::Failed`], standing in for event-broker's
    ///   `409 PositionsNotSet`: a feed over partitions with no committed
    ///   position cannot be opened. Each unseeded `(topic, partition)` is
    ///   reported as a precondition violation so the caller still learns *which*
    ///   ones to seed.
    /// - An `InvalidArgument` `CanonicalError` when `filter.currency` is not a
    ///   three-letter ISO 4217 code — the "unacceptable filter" open failure,
    ///   carried as a field violation on `currency`.
    pub fn open_feed(
        self: &Arc<Self>,
        ctx: &SecurityContext,
        filter: &ListPaymentsFilter,
    ) -> Result<api_contracts_sdk::contract::PaymentStream<PaymentSummary>, CanonicalError> {
        // The precondition open failure. Two unseeded partitions rather than
        // one, so a test cannot pass by accident on a single-element collection.
        if filter.status == Some(PaymentStatus::Failed) {
            return Err(PaymentResourceError::failed_precondition()
                .with_precondition_violation(
                    "payments:0",
                    "partition has no committed position; seek it before opening the feed",
                    "POSITIONS_NOT_SET",
                )
                .with_precondition_violation(
                    "payments:3",
                    "partition has no committed position; seek it before opening the feed",
                    "POSITIONS_NOT_SET",
                )
                .create());
        }

        if let Some(currency) = filter.currency.as_deref()
            && !(currency.len() == 3 && currency.chars().all(|c| c.is_ascii_uppercase()))
        {
            return Err(PaymentResourceError::invalid_argument()
                .with_field_violation(
                    "currency",
                    format!("must be a three-letter ISO 4217 code, got `{currency}`"),
                    "INVALID_FILTER",
                )
                .create());
        }

        let snapshot = self.snapshot(ctx.subject_tenant_id(), filter);
        Ok(Box::pin(async_stream::try_stream! {
            for item in snapshot {
                yield item;
            }
        }))
    }
}

impl Default for PaymentDomainService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api_contracts_sdk::models::ChargeRequest;
    use futures_util::StreamExt as _;

    fn tenant_ctx(tenant: Uuid) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(tenant)
            .subject_type("user")
            .build()
            .expect("a fully-specified context builds")
    }

    /// A read path only yields the caller's own tenant's payments: two tenants
    /// charge into one store, and neither `list_payments` nor `open_feed` leaks
    /// the other's records (#4740 #26).
    #[tokio::test]
    async fn read_paths_only_yield_the_callers_own_tenants_payments() {
        let svc = Arc::new(PaymentDomainService::new());
        let ctx_a = tenant_ctx(Uuid::new_v4());
        let ctx_b = tenant_ctx(Uuid::new_v4());

        svc.charge(&ctx_a, &ChargeRequest::new(100, "USD", "a1"))
            .expect("charge a1");
        svc.charge(&ctx_a, &ChargeRequest::new(200, "USD", "a2"))
            .expect("charge a2");
        svc.charge(&ctx_b, &ChargeRequest::new(300, "USD", "b1"))
            .expect("charge b1");

        let a_listed: Vec<_> = svc
            .list_payments(&ctx_a, &ListPaymentsFilter::default())
            .collect()
            .await;
        let b_listed: Vec<_> = svc
            .list_payments(&ctx_b, &ListPaymentsFilter::default())
            .collect()
            .await;
        assert_eq!(a_listed.len(), 2, "tenant A sees only its two payments");
        assert_eq!(b_listed.len(), 1, "tenant B sees only its one payment");

        // `open_feed` reads through the same tenant-scoped snapshot.
        let a_feed: Vec<_> = svc
            .open_feed(&ctx_a, &ListPaymentsFilter::default())
            .expect("open succeeds")
            .collect()
            .await;
        assert_eq!(a_feed.len(), 2, "open_feed is tenant-scoped too");
    }
}
