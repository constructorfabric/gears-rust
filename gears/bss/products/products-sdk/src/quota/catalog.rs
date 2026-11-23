//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-contract:p1
//! The approved quota catalog as plan authors and Pricing read it (P-D-272): one immutable
//! definition version per answer, with its current availability, through
//! [`ProductsQuotaCatalogClientV1`]. The fields are the quotas package's (quotas DESIGN
//! §3.3.1.1); the REST endpoints answer the same shape.
use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::gts::{QuotaAccountingPolicyV1, QuotaChangeSupport, QuotaKind, QuotaScopePolicyV1};

/// A positive version, serialized as a decimal string (quotas DESIGN §3.3.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuotaVersion(NonZeroU64);

impl QuotaVersion {
    /// The version as an integer.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The version `value`.
    ///
    /// # Errors
    /// `QuotaVersionError` when `value` is zero.
    pub fn new(value: u64) -> Result<Self, QuotaVersionError> {
        NonZeroU64::new(value).map(Self).ok_or(QuotaVersionError)
    }
}

impl FromStr for QuotaVersion {
    type Err = QuotaVersionError;

    /// Read a canonical decimal from 1: digits only, without a leading zero.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let canonical = !text.is_empty()
            && text.bytes().all(|byte| byte.is_ascii_digit())
            && !text.starts_with('0');
        if !canonical {
            return Err(QuotaVersionError);
        }
        // Digits only, so the one way `parse` can still fail is a value over 64 bits.
        text.parse::<u64>()
            .map_err(|_| QuotaVersionError)
            .and_then(Self::new)
    }
}

impl fmt::Display for QuotaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for QuotaVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for QuotaVersion {
    /// A JSON string only: a JSON number is refused, as the package's contract writes versions as
    /// decimal strings.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A version that is not a canonical decimal integer from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaVersionError;

impl fmt::Display for QuotaVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a version is a canonical decimal integer from 1")
    }
}

impl std::error::Error for QuotaVersionError {}

/// A reference to one immutable version of an owned record: a mapping, a profile or a
/// qualification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaOwnerRef {
    /// The record's id.
    pub id: Uuid,
    /// The record's version.
    pub version: QuotaVersion,
    /// The version's canonical digest, lowercase hex.
    pub digest: String,
}

/// One plan unit a definition allows, with its exact factor and the bounds converted into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaAllowedUnit {
    /// The unit instance.
    pub unit_ref: String,
    /// The unit's registry content digest, kept at approval.
    pub digest: String,
    /// Display symbol; never an identity.
    pub symbol: String,
    /// The unit's factor into the canonical unit: numerator.
    pub numerator: String,
    /// The unit's factor into the canonical unit: denominator.
    pub denominator: String,
    /// The smallest amount in this unit.
    pub min: String,
    /// The granularity of an amount in this unit.
    pub step: String,
    /// The largest amount in this unit; `None` when unbounded.
    pub max: Option<String>,
}

/// How a plan states a quota amount, spelled as the package spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaBasis {
    /// One amount for the whole plan item.
    Fixed,
    /// An amount per purchased plan unit.
    PerPlanUnit,
}

/// Whether a definition version may be sold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAvailabilityState {
    /// Sellable.
    Enabled,
    /// Not sellable; the reason says why.
    Disabled,
    /// No longer sellable, for good.
    Withdrawn,
}

/// A definition version's current availability: the highest revision recorded for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaAvailability {
    /// The state.
    pub state: QuotaAvailabilityState,
    /// The revision of this observation.
    pub revision: QuotaVersion,
    /// When the state was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    /// Why a version is not enabled; `None` exactly when it is enabled.
    pub reason_code: Option<String>,
}

/// One immutable definition version with its current availability (quotas DESIGN §3.3.1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaDefinition {
    /// The definition's id, shared by all its versions.
    pub definition_id: Uuid,
    /// The version.
    pub version: QuotaVersion,
    /// The canonical digest of the version's content, lowercase hex.
    pub definition_digest: String,
    /// The SKU the definition was approved for.
    pub sku_id: Uuid,
    /// The SKU version it was approved against; it applies to that version and every later one.
    pub sku_version: QuotaVersion,
    /// The metric.
    pub metric_ref: String,
    /// The metric's registry content digest, kept at approval.
    pub metric_digest: String,
    /// The metric's kind.
    pub quota_kind: QuotaKind,
    /// The metric's display name in `locale`.
    pub display_name: String,
    /// The metric's description in `locale`.
    pub description: String,
    /// The locale tag the texts are served in: the requested one, or English.
    pub locale: String,
    /// The canonical unit of the metric.
    pub canonical_unit_ref: String,
    /// The plan units a plan may state amounts in.
    pub allowed_units: Vec<QuotaAllowedUnit>,
    /// The type of the subject the quota applies to.
    pub subject_type_ref: String,
    /// The types of the resources the quota counts.
    pub resource_type_refs: Vec<String>,
    /// The enforcement mapping version.
    pub mapping_ref: QuotaOwnerRef,
    /// The qualified profile version.
    pub profile_ref: QuotaOwnerRef,
    /// `None` until qualification evidence is recorded (P-D-271).
    pub qualification_ref: Option<QuotaOwnerRef>,
    /// The accounting policy.
    pub accounting_policy_ref: String,
    /// The accounting policy's registry content digest, kept at approval.
    pub accounting_policy_digest: String,
    /// The accounting policy's content, as kept at approval.
    pub accounting_policy: QuotaAccountingPolicyV1,
    /// The scope policy.
    pub scope_policy_ref: String,
    /// The scope policy's registry content digest, kept at approval.
    pub scope_policy_digest: String,
    /// The scope policy's content, as kept at approval.
    pub scope_policy: QuotaScopePolicyV1,
    /// Which changes of an installed quota the adapter supports.
    pub change_support: QuotaChangeSupport,
    /// The reviewed conditions a plan author shows the buyer, in `locale`.
    pub profile_conditions: String,
    /// The bases a plan may state amounts on.
    pub supported_bases: Vec<QuotaBasis>,
    /// The current availability.
    pub availability: QuotaAvailability,
}

/// Which availability states a list keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuotaAvailabilityFilter {
    /// Only enabled versions (the default).
    #[default]
    Enabled,
    /// Every version, each with its availability and reason.
    All,
}

/// A list of the definition versions applicable to one SKU version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDefinitionListRequest {
    /// The SKU.
    pub sku_id: Uuid,
    /// The SKU version; the list answers versions approved at it or at an earlier one.
    pub sku_version: QuotaVersion,
    /// Only definitions for this subject type.
    pub subject_type_ref: Option<String>,
    /// Only definitions counting this resource type.
    pub resource_type_ref: Option<String>,
    /// Only definitions of this qualified profile.
    pub profile_id: Option<Uuid>,
    /// The locale to serve texts in; English when absent or not held.
    pub locale: Option<String>,
    /// Which availability states to keep.
    pub availability: QuotaAvailabilityFilter,
    /// 1 … 200, default 50.
    pub limit: Option<u32>,
    /// A cursor from a previous page in the same tenant, with the same filters and
    /// availability; any other cursor is refused (400).
    pub cursor: Option<String>,
}

/// One page of definition versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaDefinitionPage {
    /// The versions, by `definition_id`, then `version`.
    pub items: Vec<QuotaDefinition>,
    /// The cursor of the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// The approved quota catalog for plan authoring (P-D-272). Pricing calls it as its caller.
#[async_trait]
pub trait ProductsQuotaCatalogClientV1: Send + Sync {
    /// The definition versions applicable to one SKU version, in the caller's tenant.
    ///
    /// # Errors
    /// 400 for a bad request, including a cursor from another tenant or one issued under other
    /// filters or another availability; 403 / 503 from the PDP; 500 for a stored row that does
    /// not read.
    async fn list_quota_definitions(
        &self,
        ctx: &SecurityContext,
        request: QuotaDefinitionListRequest,
    ) -> Result<QuotaDefinitionPage, CanonicalError>;

    /// One exact definition version with its current availability.
    ///
    /// # Errors
    /// 404 for an unknown version or another tenant's; otherwise as `list_quota_definitions`.
    async fn get_quota_definition(
        &self,
        ctx: &SecurityContext,
        definition_id: Uuid,
        version: QuotaVersion,
        locale: Option<&str>,
    ) -> Result<QuotaDefinition, CanonicalError>;
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
