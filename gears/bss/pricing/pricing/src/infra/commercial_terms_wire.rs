//! Strict invoice-snapshot adapter; no endpoint, default policy lookup or persistence.
use bss_pricing_sdk::{
    acceptance::{CommercialReason as R, UnsupportedCommercialValue},
    terms::{BillingTerms, TermsSource},
};
use serde::Deserialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TermsWire {
    schema_version: String,
    cycle: String,
    anchor: String,
    anchor_at: String,
    timezone: String,
    source: SourceWire,
    digest: String,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceWire {
    ExplicitOrder,
    SellerPolicy { id: Uuid, version: String },
}
fn unsupported(field: &'static str, value: impl Into<String>) -> UnsupportedCommercialValue {
    UnsupportedCommercialValue {
        field,
        value: value.into(),
        reason: R::UnsupportedTerms,
    }
}
fn positive(value: &str, field: &'static str) -> Result<u64, UnsupportedCommercialValue> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && n.to_string() == value)
        .ok_or_else(|| unsupported(field, value))
}
/// Decode the original snapshot bytes, preserving duplicate/unknown-field rejection.
/// Missing snapshots are explicit `MissingBillingTerms`; unsupported scalar text is retained.
/// Shape and schema are checked here; the pure validator recomputes the supplied digest.
/// # Errors
/// Typed unsupported values; never guesses a missing cycle, timezone, anchor or source.
pub fn decode_billing_terms(raw: &str) -> Result<BillingTerms, UnsupportedCommercialValue> {
    let wire: Option<TermsWire> =
        serde_json::from_str(raw).map_err(|e| unsupported("billing_terms", e.to_string()))?;
    let t = wire.ok_or_else(|| UnsupportedCommercialValue {
        field: "billing_terms",
        value: "null".into(),
        reason: R::MissingBillingTerms,
    })?;
    if t.schema_version != "1" {
        return Err(unsupported("schema_version", t.schema_version));
    }
    let source = match t.source {
        SourceWire::ExplicitOrder => TermsSource::ExplicitOrder,
        SourceWire::SellerPolicy { id, version } => {
            if id.is_nil() {
                return Err(unsupported("source.id", id.to_string()));
            }
            TermsSource::SellerPolicy {
                id,
                version: positive(&version, "source.version")?,
            }
        }
    };
    let Some(digest) = crate::infra::usage_policy_wire::parse_digest_text(&t.digest) else {
        return Err(unsupported("digest", t.digest));
    };
    Ok(BillingTerms {
        schema_version: 1,
        cycle: t.cycle.parse()?,
        anchor: t.anchor.parse()?,
        anchor_at: OffsetDateTime::parse(&t.anchor_at, &Rfc3339)
            .map_err(|_| unsupported("anchor_at", &t.anchor_at))?,
        timezone: t.timezone.parse()?,
        source,
        digest,
    })
}
