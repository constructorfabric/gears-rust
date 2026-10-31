//! Proposed Orders-only `protected_full_tuple_v1` / `line_scope_v1` evidence.
//! Rights inherit their row's scope, policy and observation revision, and their context's
//! order/version/tenant axes. No provider reservation is created or consumed here.
use super::answers::{LimitProvenance, MaxConcurrentActive, Occupancy, OccupancyQuery};
use super::item_keys::valid_text;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::collections::{BTreeMap, BTreeSet};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Distinct legacy wire objects and strict discriminated protected objects.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OccupancyObservation {
    /// Original active-only shape; never protected evidence.
    LegacyActiveV1(Occupancy),
    /// Proposed full-tuple coherent observation.
    ProtectedFullTupleV1(Box<ProtectedOccupancy>),
}
impl Serialize for OccupancyObservation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::LegacyActiveV1(value) => value.serialize(serializer),
            Self::ProtectedFullTupleV1(value) => value.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for OccupancyObservation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value.get("profile") {
            None => serde_json::from_value(value).map(Self::LegacyActiveV1),
            Some(profile) if profile == "protected_full_tuple_v1" => serde_json::from_value(value)
                .map(Box::new)
                .map(Self::ProtectedFullTupleV1),
            Some(_) => return Err(D::Error::custom("unknown occupancy profile")),
        }
        .map_err(D::Error::custom)
    }
}
/// Literal profile discriminator.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectedProfile {
    /// Proposed protected full tuple.
    #[serde(rename = "protected_full_tuple_v1")]
    V1,
}
/// Literal proposed demand unit.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DemandUnit {
    /// One right per distinct line/scope.
    #[serde(rename = "line_scope_v1")]
    LineScopeV1,
}
/// Full tuple; this profile has no per-payer alternative.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedScope {
    /// Paying tenant.
    pub payer_tenant_id: Uuid,
    /// Resource tenant.
    pub resource_tenant_id: Uuid,
    /// Exact overlap scope.
    pub overlap_scope_key: String,
}
/// Explicit versioned policy evidence.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OccupancyPolicyEvidence {
    /// Limit authority, separate from its count scope.
    pub source: LimitProvenance,
    /// Opaque policy reference.
    pub reference: String,
    /// Opaque policy version.
    pub version: String,
}
/// Trusted requested reservation association. Line order is immaterial; duplicates refuse.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnContext {
    /// Exact stored order.
    pub order_id: Uuid,
    /// Exact current stored version.
    pub version: u32,
    /// Paying tenant.
    pub payer_tenant_id: Uuid,
    /// Resource tenant.
    pub resource_tenant_id: Uuid,
    /// Every distinct requested line for this scope.
    pub line_ids: Vec<Uuid>,
}
/// Literal represented right state.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnRightState {
    /// Active; conversion mapping unspecified, so recheck defers.
    Active,
    /// Retained suspension; conversion mapping unspecified.
    RetainedSuspended,
    /// Exact reusable reservation for initial activation.
    OrderReserved,
}
/// One represented own right; order/version/scope/policy/revision are inherited.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnRight {
    /// Proposed local claim identifier, unique across the complete observation.
    pub claim_id: Uuid,
    /// Exact requested stored line.
    pub line_id: Uuid,
    /// Literal category in global counts.
    pub state: OwnRightState,
}
/// Complete membership for exactly one requested context.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnMembership {
    /// Must be true, including for empty rights.
    pub complete: bool,
    /// Exact trusted request echo.
    pub context: OwnContext,
    /// At most one right per requested line/scope.
    pub rights: Vec<OwnRight>,
}
/// The required `own_membership` answer of a protected row.
///
/// Wire form: `null` when there is no reusable context, a complete [`OwnMembership`] object
/// for the recheck. A missing member is refused.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnMembershipAnswer {
    /// No reusable context (`null`).
    None,
    /// The membership for the recheck.
    Present(OwnMembership),
}

impl Serialize for OwnMembershipAnswer {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::None => None,
            Self::Present(membership) => Some(membership),
        }
        .serialize(s)
    }
}

/// Reads `null` as [`OwnMembershipAnswer::None`] and an object as the membership, decoded by
/// [`OwnMembership`]'s own derive (so its `deny_unknown_fields` and error paths are unchanged).
struct OwnMembershipVisitor;

impl<'de> serde::de::Visitor<'de> for OwnMembershipVisitor {
    type Value = OwnMembershipAnswer;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an own-membership object, or null for no reusable context")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(OwnMembershipAnswer::None)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(OwnMembershipAnswer::None)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        OwnMembership::deserialize(d).map(OwnMembershipAnswer::Present)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        OwnMembership::deserialize(serde::de::value::MapAccessDeserializer::new(map))
            .map(OwnMembershipAnswer::Present)
    }
}

/// `deserialize_any`, not `deserialize_option` (as for [`MaxConcurrentActive`]): a serde derive
/// treats a missing member of an `Option` as `None`, but a missing member reaching
/// `deserialize_any` is an error. So the type itself refuses an absent member, with no
/// per-field attribute; only an explicit `null` is [`OwnMembershipAnswer::None`].
impl<'de> Deserialize<'de> for OwnMembershipAnswer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(OwnMembershipVisitor)
    }
}
/// Strict proposed protected wire object. Nullable members must be explicitly present.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedOccupancy {
    /// Literal discriminator.
    pub profile: ProtectedProfile,
    /// Literal demand unit.
    pub demand_unit: DemandUnit,
    /// One coherent revision across all rows.
    pub observation_revision: String,
    /// Exact requested full tuple.
    pub scope: ProtectedScope,
    /// Explicit limit policy binding.
    pub policy: OccupancyPolicyEvidence,
    /// Literal active rights.
    pub active_count: u32,
    /// Literal retained suspended rights.
    pub retained_suspended_count: u32,
    /// Literal order reservations.
    pub order_reserved_count: u32,
    /// Required number or explicit null.
    pub max_concurrent_active: MaxConcurrentActive,
    /// Required null for no reusable context; complete object for recheck. The type keeps the
    /// member required: a missing member is refused, `null` is accepted.
    pub own_membership: OwnMembershipAnswer,
}
/// Why a protected occupancy batch was refused. Each variant is one refusal class of
/// [`validate_batch`]; [`Self::token`] is the Orders-authored `upstream_code` it reports.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProtectedInvalid {
    /// The row count differs from the expected keys, or a key repeats.
    #[error("incomplete or duplicate occupancy rows")]
    IncompleteRows,
    /// A protected row is mixed with a legacy one.
    #[error("mixed occupancy profiles")]
    MixedProfiles,
    /// A row's overlap key is not one the request asked for.
    #[error("unexpected occupancy key")]
    UnexpectedKey,
    /// A row's query, scope, policy or revision does not bind to the trusted request.
    #[error("invalid occupancy binding")]
    InvalidBinding,
    /// The rows do not share one observation revision.
    #[error("mixed occupancy revisions")]
    MixedRevisions,
    /// The own membership context is incomplete, foreign or does not echo the request.
    #[error("invalid own membership context")]
    InvalidOwnContext,
    /// An own right is nil, reused, foreign, duplicated or in an unsupported state.
    #[error("invalid or unsupported own right")]
    InvalidOwnRight,
    /// More rights are represented than the category count holds.
    #[error("represented rights exceed category count")]
    RightsExceedCount,
    /// Own membership is missing where requested or present where not.
    #[error("missing or unsolicited own membership")]
    MembershipMismatch,
    /// Two rows name the same overlap key.
    #[error("duplicate occupancy row")]
    DuplicateRow,
    /// A count, right or demand overflowed or underflowed.
    #[error("occupancy arithmetic overflow")]
    Overflow,
}
impl ProtectedInvalid {
    /// The stable Orders-authored `upstream_code` for this refusal.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::IncompleteRows => "protected-incomplete-rows",
            Self::MixedProfiles => "protected-mixed-profiles",
            Self::UnexpectedKey => "protected-unexpected-key",
            Self::InvalidBinding => "protected-invalid-binding",
            Self::MixedRevisions => "protected-mixed-revisions",
            Self::InvalidOwnContext => "protected-invalid-own-context",
            Self::InvalidOwnRight => "protected-invalid-own-right",
            Self::RightsExceedCount => "protected-rights-exceed-count",
            Self::MembershipMismatch => "protected-membership-mismatch",
            Self::DuplicateRow => "protected-duplicate-row",
            Self::Overflow => "protected-overflow",
        }
    }
}

/// The outcome of a valid batch: homogeneous legacy rows keep their original arithmetic;
/// protected rows are validated per overlap key.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectedBatch {
    /// No protected row: the legacy active-only arithmetic applies.
    Legacy,
    /// Every row is protected and coherent, keyed by overlap scope.
    Protected(BTreeMap<String, ValidatedProtected>),
}

/// Checked projection used by assessment and recheck only after batch validation.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProtected {
    total: u64,
    new: u64,
    limit: MaxConcurrentActive,
}
impl ValidatedProtected {
    /// Exact reserved conversion needs no new slot even above a lowered limit.
    /// # Errors
    /// Returns [`ProtectedInvalid::Overflow`] on arithmetic overflow.
    pub fn admits(&self) -> Result<bool, ProtectedInvalid> {
        if self.new == 0 {
            return Ok(true);
        }
        let demand = self
            .total
            .checked_add(self.new)
            .ok_or(ProtectedInvalid::Overflow)?;
        Ok(match self.limit {
            MaxConcurrentActive::Unlimited => true,
            MaxConcurrentActive::Limit(limit) => demand <= u64::from(limit),
        })
    }
}

fn context_matches(expected: &OwnContext, actual: &OwnContext, lines: &BTreeSet<Uuid>) -> bool {
    expected.line_ids.len() == lines.len()
        && actual.line_ids.len() == lines.len()
        && expected.line_ids.iter().copied().collect::<BTreeSet<_>>() == *lines
        && expected.order_id == actual.order_id
        && !actual.order_id.is_nil()
        && expected.version == actual.version
        && actual.version != 0
        && expected.payer_tenant_id == actual.payer_tenant_id
        && expected.resource_tenant_id == actual.resource_tenant_id
        && actual.line_ids.iter().copied().collect::<BTreeSet<_>>() == *lines
}

/// Validate the complete protected batch before any row can pass. Homogeneous legacy returns
/// [`ProtectedBatch::Legacy`] and preserves its original arithmetic. Expected queries come exclusively from trusted
/// basket/stored identities, never the returned observations.
/// # Errors
/// Refuses incomplete, mixed, conflicting, foreign or unsupported protected evidence.
pub fn validate_batch(
    expected: &BTreeMap<String, (OccupancyQuery, BTreeSet<Uuid>)>,
    rows: &[(OccupancyQuery, OccupancyObservation)],
) -> Result<ProtectedBatch, ProtectedInvalid> {
    if !rows
        .iter()
        .any(|(_, o)| matches!(o, OccupancyObservation::ProtectedFullTupleV1(_)))
    {
        return Ok(ProtectedBatch::Legacy);
    }
    if rows.len() != expected.len() {
        return Err(ProtectedInvalid::IncompleteRows);
    }
    let mut revision: Option<&str> = None;
    let mut claims = BTreeSet::new();
    let mut result = BTreeMap::new();
    for (query, observation) in rows {
        let OccupancyObservation::ProtectedFullTupleV1(o) = observation else {
            return Err(ProtectedInvalid::MixedProfiles);
        };
        let Some((want, lines)) = expected.get(&query.overlap_scope_key) else {
            return Err(ProtectedInvalid::UnexpectedKey);
        };
        let context_echo = match (&want.own_context, &query.own_context) {
            (None, None) => true,
            (Some(want), Some(actual)) => context_matches(want, actual, lines),
            _ => false,
        };
        if !context_echo
            || want.payer_tenant_id != query.payer_tenant_id
            || want.resource_tenant_id != query.resource_tenant_id
            || query.payer_tenant_id.is_nil()
            || query.resource_tenant_id.is_none_or(|id| id.is_nil())
            || !valid_text(&query.overlap_scope_key)
            || o.scope.payer_tenant_id != want.payer_tenant_id
            || Some(o.scope.resource_tenant_id) != want.resource_tenant_id
            || o.scope.overlap_scope_key != want.overlap_scope_key
            || !valid_text(&o.observation_revision)
            || !valid_text(&o.policy.reference)
            || !valid_text(&o.policy.version)
            || lines.iter().any(Uuid::is_nil)
        {
            return Err(ProtectedInvalid::InvalidBinding);
        }
        if revision.is_some_and(|r| r != o.observation_revision) {
            return Err(ProtectedInvalid::MixedRevisions);
        }
        revision = Some(&o.observation_revision);
        let mut represented = BTreeSet::new();
        match (&want.own_context, &o.own_membership) {
            (None, OwnMembershipAnswer::None) => {}
            (Some(context), OwnMembershipAnswer::Present(membership)) => {
                if !membership.complete
                    || membership.context.line_ids.len() > lines.len()
                    || membership.rights.len() > lines.len()
                    || !context_matches(context, &membership.context, lines)
                    || context.payer_tenant_id != want.payer_tenant_id
                    || Some(context.resource_tenant_id) != want.resource_tenant_id
                {
                    return Err(ProtectedInvalid::InvalidOwnContext);
                }
                for right in &membership.rights {
                    if right.claim_id.is_nil()
                        || !claims.insert(right.claim_id)
                        || !lines.contains(&right.line_id)
                        || !represented.insert(right.line_id)
                        || right.state != OwnRightState::OrderReserved
                    {
                        return Err(ProtectedInvalid::InvalidOwnRight);
                    }
                }
                if u64::try_from(represented.len()).map_err(|_| ProtectedInvalid::Overflow)?
                    > u64::from(o.order_reserved_count)
                {
                    return Err(ProtectedInvalid::RightsExceedCount);
                }
            }
            _ => return Err(ProtectedInvalid::MembershipMismatch),
        }
        let total = u64::from(o.active_count)
            .checked_add(u64::from(o.retained_suspended_count))
            .and_then(|n| n.checked_add(u64::from(o.order_reserved_count)))
            .ok_or(ProtectedInvalid::Overflow)?;
        let new = u64::try_from(lines.len())
            .map_err(|_| ProtectedInvalid::Overflow)?
            .checked_sub(u64::try_from(represented.len()).map_err(|_| ProtectedInvalid::Overflow)?)
            .ok_or(ProtectedInvalid::Overflow)?;
        if result
            .insert(
                query.overlap_scope_key.clone(),
                ValidatedProtected {
                    total,
                    new,
                    limit: o.max_concurrent_active,
                },
            )
            .is_some()
        {
            return Err(ProtectedInvalid::DuplicateRow);
        }
    }
    Ok(ProtectedBatch::Protected(result))
}

impl From<Occupancy> for OccupancyObservation {
    fn from(value: Occupancy) -> Self {
        Self::LegacyActiveV1(value)
    }
}
