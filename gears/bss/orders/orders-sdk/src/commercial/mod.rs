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
    /// An unsupported schema or value, including a value the encoder refuses to write (an
    /// unrepresentable scalar) and a term the invoice periods cannot express exactly.
    #[error("unsupported commercial encoding or value")]
    Unsupported,
    /// Required evidence or input is absent (a missing term intent or billing cycle, an empty
    /// required member).
    #[error("incomplete commercial evidence")]
    Incomplete,
    #[error("commercial evidence mismatch")]
    Mismatch,
    /// The stored bytes do not decode; the cause is the decoder's category (D-220), never its
    /// message, so a caller can log it without stored values (D-44).
    #[error("invalid commercial encoding ({0})")]
    Malformed(MalformedKind),
    #[error("commercial evidence exceeds supported limits")]
    TooLarge,
}
/// Why stored commercial bytes failed to decode: the `serde_json` error category, owned by the
/// SDK so the codec error stays `Copy` and free of decoder internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedKind {
    /// Not well-formed JSON.
    Syntax,
    /// Well-formed JSON of the wrong shape: an unknown, missing or duplicate member, a wrong
    /// type, or a value a strict scalar refuses.
    Data,
    /// The input ended inside a value.
    Eof,
    /// The reader failed.
    Io,
}
impl MalformedKind {
    fn of(error: &serde_json::Error) -> Self {
        match error.classify() {
            serde_json::error::Category::Syntax => Self::Syntax,
            serde_json::error::Category::Data => Self::Data,
            serde_json::error::Category::Eof => Self::Eof,
            serde_json::error::Category::Io => Self::Io,
        }
    }
}
impl std::fmt::Display for MalformedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax => "syntax",
            Self::Data => "data",
            Self::Eof => "eof",
            Self::Io => "io",
        })
    }
}
/// The committed selection a pin is verified against (D-220): the selected row's acceptance
/// identity, its two issuer digests and the evaluated bindings, named so neither digest can be
/// passed in the other's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionEvidence<'a> {
    pub acceptance_id: Uuid,
    pub request_digest: Digest,
    pub terms_digest: Digest,
    pub evaluated_bindings: &'a [AcceptedBinding],
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
        let pin = Self(
            serde_json::from_slice(bytes)
                .map_err(|e| CodecError::Malformed(MalformedKind::of(&e)))?,
        );
        pin.validate()?;
        Ok(pin)
    }
    /// Encode exact decimal strings and canonical UTC instants, retaining array order.
    /// # Errors
    /// A value the encoder refuses to write (for example a date outside the four-digit year
    /// encoding) is `Unsupported`; an encoded size beyond the complete-version ceiling is
    /// `TooLarge`.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        // Writing to a `Vec` cannot fail on I/O, so a serializer error is always a refused value.
        let encoded = serde_json::to_vec(&self.0).map_err(|_| CodecError::Unsupported)?;
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
        selected: SelectionEvidence<'_>,
    ) -> Result<(), CodecError> {
        let SelectionEvidence {
            acceptance_id,
            request_digest,
            terms_digest,
            evaluated_bindings,
        } = selected;
        let receipt = &self.0.receipt;
        if receipt.query != *frozen
            || receipt.acceptance_id != acceptance_id
            || receipt.request_digest != request_digest
            || receipt.terms_digest != terms_digest
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
                || b.price.currency.code() != q.market.currency
                // One currency at one scale across the receipt (bss_money `ensure_same`).
                || r.bindings[0].price.currency.ensure_same(&b.price.currency).is_err()
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
