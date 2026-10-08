//! Orders-owned schema-2 receipt codec. Integrity checks never grant live sale authority.
//! Provider-owned assessment/terms/Rating APIs remain proposals in the S1-04 ledger.
pub(crate) mod scalars;
pub mod term;
mod wire;
use crate::models::OrderVersion;
use bss_pricing_sdk::{
    Digest,
    acceptance::{AcceptanceReceipt, NewSaleQuery, Term},
    digest::{
        billing_terms_digest, policy_digest, request_digest, selected_bindings_digest, terms_digest,
    },
    read::{
        AcceptedBinding, BindingSelection, ChargeKind, PriceState, ResolvedBindings, ResolvedCell,
    },
};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

/// Complete immutable version limit; a single pin must also fit inside it.
pub const MAX_VERSION_BYTES: usize = 1_048_576;
/// D-192 per-line limit before deduplication.
pub const MAX_BINDINGS: usize = 1_000;

/// Local codec categories. Runtime adapters map them to the established Orders reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("unsupported commercial encoding or value")]
    Unsupported,
    #[error("incomplete commercial evidence")]
    Incomplete,
    #[error("commercial evidence mismatch")]
    Mismatch,
    #[error("invalid commercial encoding")]
    Malformed,
    #[error("commercial evidence exceeds supported limits")]
    TooLarge,
}
/// Exact committed row identity; allocated/reserved candidate identity alone is not authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedVersionRef {
    pub order_id: Uuid,
    pub order_version: OrderVersion,
    pub line_id: Uuid,
}
/// Independently observed Orders assessment context, separate from issuer timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssessmentRef {
    pub assessment_id: Uuid,
    pub assessed_at: OffsetDateTime,
    pub resolve_date: Date,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    assessment_id: Uuid,
    #[serde(with = "scalars::instant")]
    assessed_at: OffsetDateTime,
    #[serde(with = "scalars::date")]
    resolve_date: Date,
    accepted_version_ref: AcceptedVersionRef,
    #[serde(with = "wire::AcceptanceReceiptDef")]
    receipt: AcceptanceReceipt,
    #[serde(with = "scalars::instant")]
    activation_deadline: OffsetDateTime,
}
/// Validated immutable evidence. Construct/decode through checked methods only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderPin(Document);
impl OrderPin {
    /// Wrap an issued native receipt without changing issuer digests or terms.
    /// # Errors
    /// Refuses incomplete, inconsistent, oversized or unrepresentable evidence.
    pub fn new(
        assessment: AssessmentRef,
        selected: AcceptedVersionRef,
        receipt: AcceptanceReceipt,
    ) -> Result<Self, CodecError> {
        let activation_deadline = receipt.hold_until;
        let pin = Self(Document {
            schema_version: 2,
            assessment_id: assessment.assessment_id,
            assessed_at: assessment.assessed_at,
            resolve_date: assessment.resolve_date,
            accepted_version_ref: selected,
            receipt,
            activation_deadline,
        });
        pin.validate()?;
        pin.encode()?;
        Ok(pin)
    }
    /// Decode historical evidence without consulting today's catalog, policy or clock.
    /// # Errors
    /// Refuses unknown schemas, missing/unknown/duplicate fields, mismatches and limits.
    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.len() > MAX_VERSION_BYTES {
            return Err(CodecError::TooLarge);
        }
        let pin = Self(serde_json::from_slice(bytes).map_err(|_| CodecError::Malformed)?);
        pin.validate()?;
        Ok(pin)
    }
    /// Encode exact decimal strings and canonical UTC instants, retaining array order.
    /// # Errors
    /// Refuses invalid scalars or encoded size beyond the complete-version ceiling.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        let encoded = serde_json::to_vec(&self.0).map_err(|_| CodecError::Malformed)?;
        if encoded.len() > MAX_VERSION_BYTES {
            return Err(CodecError::TooLarge);
        }
        Ok(encoded)
    }
    #[must_use]
    pub fn receipt(&self) -> &AcceptanceReceipt {
        &self.0.receipt
    }
    #[must_use]
    pub fn activation_deadline(&self) -> OffsetDateTime {
        self.0.activation_deadline
    }
    /// Bind to frozen attempt input and independently selected row columns before use.
    /// The caller must separately prove this is the committed selection, never a reserved attempt.
    /// # Errors
    /// Any changed query, selected receipt identity or original digest refuses.
    pub fn verify_selection(
        &self,
        frozen: &NewSaleQuery,
        acceptance_id: Uuid,
        request: Digest,
        terms: Digest,
        evaluated_bindings: &[AcceptedBinding],
    ) -> Result<(), CodecError> {
        let receipt = &self.0.receipt;
        if receipt.query != *frozen
            || receipt.acceptance_id != acceptance_id
            || receipt.request_digest != request
            || receipt.terms_digest != terms
            || receipt.bindings != evaluated_bindings
        {
            return Err(CodecError::Mismatch);
        }
        Ok(())
    }
    fn validate(&self) -> Result<(), CodecError> {
        let d = &self.0;
        let r = &d.receipt;
        let q = &r.query;
        if d.schema_version != 2 || q.billing_terms.schema_version != 1 {
            return Err(CodecError::Unsupported);
        }
        if r.bindings.len() > MAX_BINDINGS || q.selections.len() > MAX_BINDINGS {
            return Err(CodecError::TooLarge);
        }
        if r.bindings.is_empty() || r.bindings.len() != q.selections.len() {
            return Err(CodecError::Incomplete);
        }
        if d.accepted_version_ref.order_id != q.order_id
            || d.accepted_version_ref.line_id != q.line_id
            || u64::try_from(i64::from(d.accepted_version_ref.order_version)).ok()
                != Some(q.order_version)
            || d.activation_deadline != r.hold_until
            || r.hold_until <= r.accepted_at
        {
            return Err(CodecError::Mismatch);
        }
        if [
            d.assessment_id,
            r.acceptance_id,
            q.order_id,
            q.line_id,
            q.plan_id,
            q.plan_revision_id,
            q.tenant_axes.seller_tenant_id,
            q.tenant_axes.payer_tenant_id,
            q.tenant_axes.resource_tenant_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || q.hold_policy_version == 0
            || q.quantity <= rust_decimal::Decimal::ZERO
            || matches!(q.term, Term::FixedPeriods { count: 0 })
        {
            return Err(CodecError::Incomplete);
        }
        let mut items = std::collections::BTreeSet::new();
        for b in &r.bindings {
            if !items.insert(b.item_id)
                || b.item_id.is_nil()
                || b.sku_id.is_nil()
                || b.price.price_id.is_nil()
                || b.price_book_entry_id.is_nil()
                || !matches!(b.price.state, PriceState::Approved)
                || b.price.currency != q.market.currency
            {
                return Err(CodecError::Mismatch);
            }
            if [
                b.invoice.gl_code.as_str(),
                b.invoice.tax_category.as_str(),
                b.invoice.template.as_str(),
            ]
            .iter()
            .any(|s| s.trim().is_empty())
            {
                return Err(CodecError::Incomplete);
            }
            if b.kind == ChargeKind::Usage
                && (b.meter.as_ref().is_none_or(|m| {
                    m.usage_type_id.trim().is_empty() || m.version.trim().is_empty()
                }) || b.usage_rating_policy.is_none())
            {
                return Err(CodecError::Incomplete);
            }
            if let Some(policy) = &b.usage_rating_policy
                && policy_digest(&policy.content) != policy.digest
            {
                return Err(CodecError::Mismatch);
            }
        }
        let resolved = ResolvedBindings {
            plan_id: q.plan_id,
            revision_id: q.plan_revision_id,
            cells: r
                .bindings
                .iter()
                .map(|b| ResolvedCell {
                    selection: BindingSelection {
                        item_id: b.item_id,
                        dimension_value: b.dimension_value.clone(),
                    },
                    binding: Some(b.clone()),
                })
                .collect(),
        };
        if selected_bindings_digest(&resolved, &q.selections).map_err(|_| CodecError::Mismatch)?
            != q.resolved_bindings_digest
            || billing_terms_digest(&q.billing_terms) != q.billing_terms.digest
            || request_digest(q) != r.request_digest
            || terms_digest(q, &r.bindings) != r.terms_digest
        {
            return Err(CodecError::Mismatch);
        }
        Ok(())
    }
}
