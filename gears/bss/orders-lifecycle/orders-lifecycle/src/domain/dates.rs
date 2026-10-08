//! The date cascade and its policy provenance (02 §3.2, §4.2; DESIGN 02 §3.7
//! `orders_date_policy`; D-60, D-121).
//!
//! Pure rules only. Draft authoring stores authored values unchanged (S2-09); this module
//! resolves them at Preview, submit and amendment against one policy snapshot and one proposed
//! UTC date, and validates that basis against the engine's single transition timestamp. Nothing
//! here reads current policy for an admitted line: its stored triple and snapshot are final.
use std::collections::BTreeMap;

use bss_orders_lifecycle_sdk::authoring::CalendarDate;
use bss_orders_lifecycle_sdk::catalog::Reason;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime, UtcOffset};
use uuid::Uuid;

use super::transition::{GuardSubject, GuardVerdict};

/// The migration-seeded platform default row (DESIGN 02 §3.7). Its key is immutable and the row
/// can never be deleted, so it is addressable by this identity alone.
pub const PLATFORM_DEFAULT_POLICY_ID: Uuid = Uuid::from_u128(0x121);

/// Which row governed a snapshot: the resource tenant's own row or the platform default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    PlatformDefault,
    ResourceTenant,
}

/// One stored `orders_date_policy` row, as read (storage-neutral).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyRow {
    pub policy_id: Uuid,
    pub resource_tenant_id: Option<Uuid>,
    pub service_activation_required: bool,
    pub acceptance_due_required: bool,
    pub revision: i64,
}

/// Why no effective policy could be snapshotted. Either fails the date guard; neither invents
/// permissive switches (02 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyFault {
    #[error("the platform default date policy is missing")]
    Missing,
    #[error("the effective date policy is invalid")]
    Invalid,
}

/// The effective policy snapshot (D-121): both switches, the source row's scope and identity
/// and its revision. Stored verbatim as `orders_order_line.date_policy_switch_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatePolicySnapshot {
    pub service_activation_required: bool,
    pub acceptance_due_required: bool,
    pub scope: PolicyScope,
    /// The governed resource tenant for a tenant row; `None` for the platform default.
    pub resource_tenant_id: Option<Uuid>,
    pub policy_id: Uuid,
    pub revision: i64,
}
impl DatePolicySnapshot {
    /// Select the effective row for `resource_tenant_id`: its own row if present, else the
    /// platform default. `rows` is whatever the store returned for that tenant and the default.
    ///
    /// # Errors
    /// [`PolicyFault::Missing`] without a default; [`PolicyFault::Invalid`] for a non-positive
    /// revision, a duplicated scope, a default under another identity or a row for another
    /// tenant.
    pub fn effective(resource_tenant_id: Uuid, rows: &[PolicyRow]) -> Result<Self, PolicyFault> {
        let mut default = None;
        let mut tenant = None;
        for row in rows {
            if row.revision <= 0 {
                return Err(PolicyFault::Invalid);
            }
            let slot = match row.resource_tenant_id {
                None if row.policy_id == PLATFORM_DEFAULT_POLICY_ID => &mut default,
                Some(t)
                    if t == resource_tenant_id
                        && !t.is_nil()
                        && row.policy_id != PLATFORM_DEFAULT_POLICY_ID =>
                {
                    &mut tenant
                }
                _ => return Err(PolicyFault::Invalid),
            };
            if slot.replace(*row).is_some() {
                return Err(PolicyFault::Invalid);
            }
        }
        let default = default.ok_or(PolicyFault::Missing)?;
        let (row, scope) = match tenant {
            Some(row) => (row, PolicyScope::ResourceTenant),
            None => (default, PolicyScope::PlatformDefault),
        };
        Ok(Self {
            service_activation_required: row.service_activation_required,
            acceptance_due_required: row.acceptance_due_required,
            scope,
            resource_tenant_id: row.resource_tenant_id,
            policy_id: row.policy_id,
            revision: row.revision,
        })
    }

    /// Re-validate a stored snapshot (frozen inputs and line evidence): scope and tenant agree,
    /// the default keeps its identity and the revision is positive.
    #[must_use]
    pub fn is_coherent(&self) -> bool {
        self.revision > 0
            && match (self.scope, self.resource_tenant_id) {
                (PolicyScope::PlatformDefault, None) => {
                    self.policy_id == PLATFORM_DEFAULT_POLICY_ID
                }
                (PolicyScope::ResourceTenant, Some(t)) => {
                    !t.is_nil() && self.policy_id != PLATFORM_DEFAULT_POLICY_ID
                }
                _ => false,
            }
    }

    /// The `date_policy_switch_state` column value.
    ///
    /// # Panics
    /// Never: the snapshot is plain data.
    #[must_use]
    pub fn switch_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// One line's three calendar fields as supplied: authored draft values, or for an amendment the
/// carried-forward resolved values, which remain values unless the delta changes them (02 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SuppliedDates {
    pub contract_effective: Option<Date>,
    pub service_activation: Option<Date>,
    pub acceptance_due: Option<Date>,
}

/// One of the three line date fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateField {
    ContractEffective,
    ServiceActivation,
    AcceptanceDue,
}

/// Where a resolved value came from. Only `TransitionDate` depends on the proposed UTC date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateSource {
    /// The supplied value, retained exactly.
    Supplied,
    /// The contract-effective default: the proposed UTC date of the transition timestamp.
    TransitionDate,
    /// An optional dependent field defaulted to the resolved contract-effective date.
    ContractEffective,
}

/// A resolved field: its value and origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedDate {
    pub value: CalendarDate,
    pub source: DateSource,
}
impl ResolvedDate {
    #[must_use]
    pub fn date(self) -> Date {
        self.value.0
    }
}

/// A line's resolved triple. The acceptance due date is a calendar field, never recorded assent,
/// and the service-activation date carries no billing implication (02 §4.2): neither converts to
/// anything else here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineDates {
    pub contract_effective: ResolvedDate,
    pub service_activation: ResolvedDate,
    pub acceptance_due: ResolvedDate,
}
impl LineDates {
    /// Whether any field defaulted from the proposed transition date (directly, or through a
    /// dependent default of an unauthored contract-effective date).
    #[must_use]
    pub fn uses_transition_date(&self) -> bool {
        self.contract_effective.source == DateSource::TransitionDate
    }
}

/// One line's cascade outcome: the resolved triple, or every policy-required field that was not
/// supplied. A default never satisfies a requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum LineResolution {
    Resolved { dates: LineDates },
    Invalid { missing_required: Vec<DateField> },
}

/// Resolve one line under `policy` and the proposed UTC date (02 §3.2 steps 2-3).
#[must_use]
pub fn resolve_line(
    supplied: SuppliedDates,
    policy: &DatePolicySnapshot,
    proposed_utc_date: Date,
) -> LineResolution {
    let contract_effective = supplied.contract_effective.map_or(
        ResolvedDate {
            value: CalendarDate(proposed_utc_date),
            source: DateSource::TransitionDate,
        },
        supplied_date,
    );
    let mut missing = Vec::new();
    let mut dependent = |value: Option<Date>, required: bool, field: DateField| match value {
        Some(value) => Some(supplied_date(value)),
        None if required => {
            missing.push(field);
            None
        }
        None => Some(ResolvedDate {
            value: contract_effective.value,
            source: DateSource::ContractEffective,
        }),
    };
    let service_activation = dependent(
        supplied.service_activation,
        policy.service_activation_required,
        DateField::ServiceActivation,
    );
    let acceptance_due = dependent(
        supplied.acceptance_due,
        policy.acceptance_due_required,
        DateField::AcceptanceDue,
    );
    match (service_activation, acceptance_due) {
        (Some(service_activation), Some(acceptance_due)) => LineResolution::Resolved {
            dates: LineDates {
                contract_effective,
                service_activation,
                acceptance_due,
            },
        },
        _ => LineResolution::Invalid {
            missing_required: missing,
        },
    }
}

fn supplied_date(value: Date) -> ResolvedDate {
    ResolvedDate {
        value: CalendarDate(value),
        source: DateSource::Supplied,
    }
}

/// The UTC calendar date of an instant.
#[must_use]
pub fn utc_date(instant: OffsetDateTime) -> Date {
    instant.to_offset(UtcOffset::UTC).date()
}

/// The frozen date basis of one Preview/submit/amendment run: the policy snapshot read once, the
/// proposed UTC date chosen before any date-dependent external call and every line's cascade.
/// Dependent predicates and evaluations receive exactly these dates; the engine validates the
/// proposed date against its transition timestamp; admission stores the triple and snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DateBasis {
    format: u8,
    /// The order's resource tenant whose effective policy was snapshotted. A platform-default
    /// snapshot does not name it, so the basis does: it cannot be applied to another order's
    /// lines (02 §4.2: the effective row is the order's `resourceTenantId` row, else the default).
    pub resource_tenant_id: Uuid,
    pub proposed_utc_date: CalendarDate,
    pub policy: DatePolicySnapshot,
    pub lines: BTreeMap<Uuid, LineResolution>,
}

/// A frozen basis that does not decode or is internally inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the frozen date basis is invalid")]
pub struct InvalidBasis;

const BASIS_FORMAT: u8 = 1;

impl DateBasis {
    /// Resolve every line against one snapshot and one proposed UTC date (02 §3.2 steps 1-3).
    #[must_use]
    pub fn resolve(
        resource_tenant_id: Uuid,
        policy: DatePolicySnapshot,
        proposed_utc_date: Date,
        lines: impl IntoIterator<Item = (Uuid, SuppliedDates)>,
    ) -> Self {
        let lines = lines
            .into_iter()
            .map(|(id, supplied)| (id, resolve_line(supplied, &policy, proposed_utc_date)))
            .collect();
        Self {
            format: BASIS_FORMAT,
            resource_tenant_id,
            proposed_utc_date: CalendarDate(proposed_utc_date),
            policy,
            lines,
        }
    }

    /// Every line's missing required fields, for the gate's all-failures report (03 §4.2
    /// predicate 8: `date-cascade-invalid`). Empty when every line resolves.
    #[must_use]
    pub fn failures(&self) -> Vec<(Uuid, DateField)> {
        self.lines
            .iter()
            .flat_map(|(id, r)| match r {
                LineResolution::Resolved { .. } => Vec::new(),
                LineResolution::Invalid { missing_required } => {
                    missing_required.iter().map(|f| (*id, *f)).collect()
                }
            })
            .collect()
    }

    /// The resolved triple of one line; `None` for an unknown or invalid line.
    #[must_use]
    pub fn line(&self, line_id: Uuid) -> Option<&LineDates> {
        match self.lines.get(&line_id)? {
            LineResolution::Resolved { dates } => Some(dates),
            LineResolution::Invalid { .. } => None,
        }
    }

    /// The stale-basis check under the aggregate lock (02 §4.2; 03 step 15): the proposed UTC
    /// date must equal `UTC-date(t)`. Every assessment of the run was evaluated at that date, so
    /// a changed UTC day refuses even where every line date was supplied; the engine never
    /// recomputes dates while keeping totals computed for the previous date.
    #[must_use]
    pub fn verdict_at(&self, transition_time: OffsetDateTime) -> GuardVerdict {
        if self.proposed_utc_date.0 == utc_date(transition_time) {
            GuardVerdict::Pass
        } else {
            GuardVerdict::Fail(Reason::DateCascadeInvalid)
        }
    }

    /// The `capture.date-basis` guard predicate bound by submit and amendment (S3-12, S4).
    /// Missing required dates are not refused here: they belong to the composite gate's
    /// all-failures report, registered after this guard.
    pub fn guard(
        self: std::sync::Arc<Self>,
    ) -> impl Fn(&GuardSubject<'_>) -> GuardVerdict + Send + Sync + 'static {
        move |subject: &GuardSubject<'_>| self.verdict_at(subject.transition_time)
    }

    /// The D-188 frozen `commercial_attempt.date_policy_basis`.
    ///
    /// # Panics
    /// Never: the basis is plain data.
    #[must_use]
    pub fn frozen(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Restore a frozen basis exactly; a recovered execution never re-reads current policy.
    ///
    /// # Errors
    /// [`InvalidBasis`] for an unknown format, an incoherent snapshot, a tenant snapshot for
    /// another resource tenant, or a line whose resolution
    /// does not follow from its own supplied values and the snapshot.
    pub fn from_frozen(value: &serde_json::Value) -> Result<Self, InvalidBasis> {
        let basis: Self = serde_json::from_value(value.clone()).map_err(|_| InvalidBasis)?;
        if basis.format != BASIS_FORMAT
            || basis.resource_tenant_id.is_nil()
            || !basis.policy.is_coherent()
            || (basis.policy.scope == PolicyScope::ResourceTenant
                && basis.policy.resource_tenant_id != Some(basis.resource_tenant_id))
        {
            return Err(InvalidBasis);
        }
        for resolution in basis.lines.values() {
            let coherent = match resolution {
                LineResolution::Resolved { dates } => {
                    dates_coherent(dates, &basis.policy, basis.proposed_utc_date.0)
                }
                LineResolution::Invalid { missing_required } => {
                    !missing_required.is_empty()
                        && missing_required.windows(2).all(|w| w[0] < w[1])
                        && missing_required.iter().all(|f| match f {
                            DateField::ContractEffective => false,
                            DateField::ServiceActivation => {
                                basis.policy.service_activation_required
                            }
                            DateField::AcceptanceDue => basis.policy.acceptance_due_required,
                        })
                }
            };
            if !coherent {
                return Err(InvalidBasis);
            }
        }
        Ok(basis)
    }
}

/// The `capture.date-basis` predicate when no basis exists because the effective policy is
/// missing or invalid: the date guard fails rather than inventing permissive switches.
pub fn unresolvable_policy_guard() -> impl Fn(&GuardSubject<'_>) -> GuardVerdict + Send + Sync {
    |_: &GuardSubject<'_>| GuardVerdict::Fail(Reason::DateCascadeInvalid)
}

/// A resolved triple re-derives from its supplied values under the snapshot and proposed date.
fn dates_coherent(dates: &LineDates, policy: &DatePolicySnapshot, proposed: Date) -> bool {
    let supplied = |r: &ResolvedDate| (r.source == DateSource::Supplied).then_some(r.date());
    let again = resolve_line(
        SuppliedDates {
            contract_effective: supplied(&dates.contract_effective),
            service_activation: supplied(&dates.service_activation),
            acceptance_due: supplied(&dates.acceptance_due),
        },
        policy,
        proposed,
    );
    again == LineResolution::Resolved { dates: *dates }
}

#[cfg(test)]
#[path = "dates_tests.rs"]
mod tests;
