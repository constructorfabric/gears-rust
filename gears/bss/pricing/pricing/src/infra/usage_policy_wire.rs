//! Closed policy wire/storage adapters; SDK value types remain infrastructure-free.
use bss_pricing_sdk::terms as sdk;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum Timezone {
    #[serde(rename = "UTC")]
    Utc,
}
impl From<Timezone> for sdk::Timezone {
    fn from(value: Timezone) -> Self {
        match value {
            Timezone::Utc => Self::Utc,
        }
    }
}
impl From<sdk::Timezone> for Timezone {
    fn from(value: sdk::Timezone) -> Self {
        match value {
            sdk::Timezone::Utc => Self::Utc,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum AggregationScope {
    #[serde(rename = "subscription_line")]
    SubscriptionLine,
    #[serde(rename = "resource")]
    Resource,
}
impl From<AggregationScope> for sdk::AggregationScope {
    fn from(value: AggregationScope) -> Self {
        match value {
            AggregationScope::SubscriptionLine => Self::SubscriptionLine,
            AggregationScope::Resource => Self::Resource,
        }
    }
}
impl From<sdk::AggregationScope> for AggregationScope {
    fn from(value: sdk::AggregationScope) -> Self {
        match value {
            sdk::AggregationScope::SubscriptionLine => Self::SubscriptionLine,
            sdk::AggregationScope::Resource => Self::Resource,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum Reset {
    #[serde(rename = "rating_window_start")]
    RatingWindowStart,
}
impl From<Reset> for sdk::Reset {
    fn from(value: Reset) -> Self {
        match value {
            Reset::RatingWindowStart => Self::RatingWindowStart,
        }
    }
}
impl From<sdk::Reset> for Reset {
    fn from(value: sdk::Reset) -> Self {
        match value {
            sdk::Reset::RatingWindowStart => Self::RatingWindowStart,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum Fold {
    #[serde(rename = "SUM")]
    Sum,
}
impl From<Fold> for sdk::Fold {
    fn from(value: Fold) -> Self {
        match value {
            Fold::Sum => Self::Sum,
        }
    }
}
impl From<sdk::Fold> for Fold {
    fn from(value: sdk::Fold) -> Self {
        match value {
            sdk::Fold::Sum => Self::Sum,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum PartialWindow {
    #[serde(rename = "actual_quantity_full_thresholds")]
    ActualQuantityFullThresholds,
}
impl From<PartialWindow> for sdk::PartialWindow {
    fn from(value: PartialWindow) -> Self {
        match value {
            PartialWindow::ActualQuantityFullThresholds => Self::ActualQuantityFullThresholds,
        }
    }
}
impl From<sdk::PartialWindow> for PartialWindow {
    fn from(value: sdk::PartialWindow) -> Self {
        match value {
            sdk::PartialWindow::ActualQuantityFullThresholds => Self::ActualQuantityFullThresholds,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RatingWindow {
    BillingCycle,
    CalendarHour { timezone: Timezone },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MeterRef {
    pub usage_type_id: String,
    pub version: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QuantitySemantics {
    pub meter: MeterRef,
    pub unit: String,
    pub fold: Fold,
    pub accrual_policy_version: String,
}
/// Complete policy content: the five rating rules (D-514).
///
/// Server-assigned identity is not part of this object. Author input uses
/// [`UsageRatingPolicyRequest`]. A row stored before D-514 still reads: its `quantity_semantics`
/// contributes `fold` and is then dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRatingPolicyInput {
    pub rating_window: RatingWindow,
    pub aggregation_scope: AggregationScope,
    pub reset: Reset,
    pub partial_window: PartialWindow,
    pub fold: Fold,
}
impl<'de> Deserialize<'de> for UsageRatingPolicyInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stored {
            rating_window: RatingWindow,
            aggregation_scope: AggregationScope,
            reset: Reset,
            partial_window: PartialWindow,
            #[serde(default)]
            fold: Option<Fold>,
            #[serde(default)]
            quantity_semantics: Option<QuantitySemantics>,
        }
        let stored = Stored::deserialize(deserializer)?;
        let fold = match (
            stored.fold,
            stored.quantity_semantics.as_ref().map(|q| q.fold),
        ) {
            (Some(top), Some(inner)) if top != inner => {
                return Err(serde::de::Error::custom(
                    "fold disagrees with quantity_semantics.fold",
                ));
            }
            (Some(top), _) => top,
            (None, Some(inner)) => inner,
            (None, None) => Fold::Sum,
        };
        Ok(Self {
            rating_window: stored.rating_window,
            aggregation_scope: stored.aggregation_scope,
            reset: stored.reset,
            partial_window: stored.partial_window,
            fold,
        })
    }
}
impl From<&UsageRatingPolicyInput> for sdk::UsageRatingPolicyInput {
    fn from(p: &UsageRatingPolicyInput) -> Self {
        Self {
            rating_window: match p.rating_window.clone() {
                RatingWindow::BillingCycle => sdk::RatingWindow::BillingCycle,
                RatingWindow::CalendarHour { timezone } => sdk::RatingWindow::CalendarHour {
                    timezone: timezone.into(),
                },
            },
            aggregation_scope: p.aggregation_scope.into(),
            reset: p.reset.into(),
            partial_window: p.partial_window.into(),
            fold: p.fold.into(),
        }
    }
}
impl From<&sdk::UsageRatingPolicyInput> for UsageRatingPolicyInput {
    fn from(p: &sdk::UsageRatingPolicyInput) -> Self {
        Self {
            rating_window: match p.rating_window.clone() {
                sdk::RatingWindow::BillingCycle => RatingWindow::BillingCycle,
                sdk::RatingWindow::CalendarHour { timezone } => RatingWindow::CalendarHour {
                    timezone: timezone.into(),
                },
            },
            aggregation_scope: p.aggregation_scope.into(),
            reset: p.reset.into(),
            partial_window: p.partial_window.into(),
            fold: p.fold.into(),
        }
    }
}
fn default_fold() -> Fold {
    Fold::Sum
}
fn default_reset() -> Reset {
    Reset::RatingWindowStart
}
fn default_partial_window() -> PartialWindow {
    PartialWindow::ActualQuantityFullThresholds
}
/// `null` is the same as a missing field: the author named no choice (D-513).
fn fold_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Fold, D::Error> {
    Ok(Option::<Fold>::deserialize(deserializer)?.unwrap_or(Fold::Sum))
}
fn reset_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Reset, D::Error> {
    Ok(Option::<Reset>::deserialize(deserializer)?.unwrap_or(Reset::RatingWindowStart))
}
fn partial_window_field<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<PartialWindow, D::Error> {
    Ok(Option::<PartialWindow>::deserialize(deserializer)?
        .unwrap_or(PartialWindow::ActualQuantityFullThresholds))
}
/// Quantity semantics on author input. `fold` defaults to `SUM` when absent or null (D-513).
/// Deprecated: the deploy-3 copy is verified and dropped (D-514).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(deprecated)]
#[serde(deny_unknown_fields)]
pub struct QuantitySemanticsRequest {
    pub meter: MeterRef,
    pub unit: String,
    /// Absent or null defaults to `SUM` (D-513).
    #[serde(default = "default_fold", deserialize_with = "fold_field")]
    #[schema(default = "SUM")]
    pub fold: Fold,
    pub accrual_policy_version: String,
}
/// Author input for a usage policy (D-513, D-514).
///
/// `fold`, `reset` and `partial_window` default when absent or null. `quantity_semantics` is the
/// deploy-3 body: when present it is verified against the SKU and the provider, then dropped.
/// It is not stored. Stored and served policies use [`UsageRatingPolicyInput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRatingPolicyRequest {
    pub rating_window: RatingWindow,
    pub aggregation_scope: AggregationScope,
    /// Absent or null defaults to `rating_window_start` (D-513).
    #[serde(default = "default_reset", deserialize_with = "reset_field")]
    #[schema(default = "rating_window_start")]
    pub reset: Reset,
    /// Deploy-3 meter copy. Deprecated: verified, then dropped (D-514).
    #[serde(default)]
    #[schema(deprecated)]
    pub quantity_semantics: Option<QuantitySemanticsRequest>,
    /// Absent or null defaults to `actual_quantity_full_thresholds` (D-513).
    #[serde(
        default = "default_partial_window",
        deserialize_with = "partial_window_field"
    )]
    #[schema(default = "actual_quantity_full_thresholds")]
    pub partial_window: PartialWindow,
    /// Absent or null defers to `quantity_semantics.fold`, or `SUM` when that is absent too (D-513).
    #[serde(default)]
    pub fold: Option<Fold>,
}
impl UsageRatingPolicyRequest {
    /// The five rating rules. A fold named twice and disagreeing is `METER_POLICY_MISMATCH`.
    /// # Errors
    /// The two folds disagree.
    pub fn rules(&self) -> Result<UsageRatingPolicyInput, crate::domain::RuleError> {
        let nested = self.quantity_semantics.as_ref().map(|q| q.fold);
        let fold = match (self.fold, nested) {
            (Some(top), Some(inner)) if top != inner => {
                return Err(crate::domain::RuleError::new("METER_POLICY_MISMATCH"));
            }
            (Some(top), _) => top,
            (None, Some(inner)) => inner,
            (None, None) => Fold::Sum,
        };
        Ok(UsageRatingPolicyInput {
            rating_window: self.rating_window.clone(),
            aggregation_scope: self.aggregation_scope,
            reset: self.reset,
            partial_window: self.partial_window,
            fold,
        })
    }
}
impl From<&UsageRatingPolicyRequest> for UsageRatingPolicyInput {
    fn from(p: &UsageRatingPolicyRequest) -> Self {
        p.rules().unwrap_or(UsageRatingPolicyInput {
            rating_window: p.rating_window.clone(),
            aggregation_scope: p.aggregation_scope,
            reset: p.reset,
            partial_window: p.partial_window,
            fold: Fold::Sum,
        })
    }
}
impl From<UsageRatingPolicyRequest> for UsageRatingPolicyInput {
    fn from(p: UsageRatingPolicyRequest) -> Self {
        Self::from(&p)
    }
}
/// Materialized immutable policy. Version is an exact decimal string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRatingPolicy {
    pub policy_id: Uuid,
    pub version: String,
    pub digest: String,
    pub content: UsageRatingPolicyInput,
}
/// Lowercase canonical SHA-256 storage text.
#[must_use]
pub fn digest_text(digest: bss_pricing_sdk::Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 15)]));
    }
    text
}
/// The inverse of [`digest_text`]: 64 lowercase hex digits, or `None`.
#[must_use]
pub fn parse_digest_text(text: &str) -> Option<bss_pricing_sdk::Digest> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl UsageRatingPolicy {
    /// Integrity-checked SDK projection of a stored policy.
    /// # Errors
    /// Invalid identity or content digest is a corrupt stored row.
    pub fn typed(&self) -> Result<sdk::UsageRatingPolicy, crate::infra::storage::RepoError> {
        let content: sdk::UsageRatingPolicyInput = (&self.content).into();
        let digest = bss_pricing_sdk::digest::policy_digest(&content);
        let version = self.version.parse::<u64>().ok().filter(|v| *v > 0);
        if self.digest != digest_text(digest) || version.is_none() {
            return Err(crate::infra::storage::RepoError::CorruptRow(
                "policy identity".into(),
            ));
        }
        Ok(sdk::UsageRatingPolicy {
            policy_id: self.policy_id,
            version: version.ok_or_else(|| {
                crate::infra::storage::RepoError::CorruptRow("policy version".into())
            })?,
            digest,
            content,
        })
    }
}

/// Captured exact provider declaration, persisted with entry reference work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterEvidence {
    pub meter: MeterRef,
    pub canonical_unit: String,
    pub fold: Fold,
    pub accrual_policy_version: String,
    pub source_integrated: bool,
    #[serde(with = "evidence_digest")]
    pub digest: [u8; 32],
}
impl From<bss_pricing_sdk::meter_semantics::MeterSemantics> for MeterEvidence {
    fn from(e: bss_pricing_sdk::meter_semantics::MeterSemantics) -> Self {
        Self {
            meter: MeterRef {
                usage_type_id: e.meter.usage_type_id,
                version: e.meter.version,
            },
            canonical_unit: e.canonical_unit,
            fold: e.fold.into(),
            accrual_policy_version: e.accrual_policy_version,
            source_integrated: e.source_integrated,
            digest: e.digest,
        }
    }
}
impl From<&MeterEvidence> for bss_pricing_sdk::meter_semantics::MeterSemantics {
    fn from(e: &MeterEvidence) -> Self {
        Self {
            meter: sdk::MeterRef {
                usage_type_id: e.meter.usage_type_id.clone(),
                version: e.meter.version.clone(),
            },
            canonical_unit: e.canonical_unit.clone(),
            fold: e.fold.into(),
            accrual_policy_version: e.accrual_policy_version.clone(),
            source_integrated: e.source_integrated,
            digest: e.digest,
        }
    }
}

/// The provider digest uses the same lowercase hexadecimal boundary representation as policy digests.
mod evidence_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub(super) fn serialize<S: Serializer>(
        digest: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::digest_text(*digest))
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        super::parse_digest_text(&text)
            .ok_or_else(|| D::Error::custom("expected 64 lowercase hexadecimal characters"))
    }
}
