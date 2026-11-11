//! Wire shapes of the activation read.

use uuid::Uuid;

use crate::api::rest::setting_dto::rfc3339;
use crate::domain::activation::{ActivationEntry, ActivationItem, ActivationRead};

/// One instance's entry of an item: a row of the current change, or a silent
/// instance the change left out of it. It carries no value of any kind.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ActivationInstanceDto {
    /// The consumer instance. Shown only to a caller whose own tenant is the
    /// root tenant; absent — never masked — for every other caller.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
    /// The instance's informational label, at most 128 characters. Absent when
    /// the instance has none, and for every caller whose own tenant is not the
    /// root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_label: Option<String>,
    /// `awaiting`, `succeeded`, `failed` or `silent`. Treat the set as open: a
    /// client meets an unknown status as "not known to have activated".
    pub status: String,
    /// `true` on a silent instance the change left out because it was silent:
    /// it is counted in the item's `silent_excluded` only. Present on every
    /// entry, `false` on the entry of a row.
    pub excluded: bool,
    /// When the settling report arrived, RFC 3339; present on a `succeeded` or
    /// `failed` entry only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
    /// When the instance was first called silent, RFC 3339: on a `silent` entry
    /// and on one a late report settled, so that the confirmation shows it came
    /// late.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silent_since: Option<String>,
    /// The consumer's failure detail; present on a `failed` entry only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One subscriber's account of its current change at the tenant. It carries no
/// value of any kind: no expected value, applied value or hash.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ActivationItemDto {
    /// The consumer identity.
    pub subscriber: String,
    /// The change the account answers: the id of its audit record in the
    /// history.
    pub change_ref: Uuid,
    /// `awaiting`, `succeeded`, `failed` or `incomplete`, derived at the read —
    /// `failed` over `incomplete` over `awaiting` over `succeeded` — and never
    /// stored. Treat the set as open.
    pub outcome: String,
    /// `M`: the number of instances the change expected, one per row.
    pub expected: u32,
    /// `N`: the instances that confirmed.
    pub confirmed: u32,
    /// The instances that failed.
    pub failed: u32,
    /// The expected instances that are `silent`.
    pub silent: u32,
    /// The silent instances the change left out, counted in none of the other
    /// counts.
    pub silent_excluded: u32,
    /// The rows of the change, then the excluded entries, each part ordered by
    /// status (`awaiting`, `succeeded`, `failed`, `silent`) and then by
    /// `instance_id` — the same order for every caller, whether or not the id
    /// is shown. At most 64 entries.
    pub instances: Vec<ActivationInstanceDto>,
}

/// The activation of a setting at one tenant: a single resource, not a page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct ActivationDto {
    /// The tenant the answer is for, a bare lowercase UUID string.
    pub tenant: String,
    /// One item per subscriber of the setting at the tenant whose current
    /// change has rows, ordered by subscriber; empty when nobody subscribed, no
    /// change has affected the subscriptions yet, or the subscribers registered
    /// only since the last change.
    pub items: Vec<ActivationItemDto>,
}

/// A count as the wire carries it; a count of entries is bounded by the
/// registration cap, so the saturation is never reached.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl From<ActivationEntry> for ActivationInstanceDto {
    fn from(entry: ActivationEntry) -> Self {
        Self {
            instance_id: entry.instance_id,
            instance_label: entry.instance_label,
            status: entry.status.as_str().to_owned(),
            excluded: entry.excluded,
            reported_at: entry.reported_at.map(rfc3339),
            silent_since: entry.silent_since.map(rfc3339),
            detail: entry.detail,
        }
    }
}

impl From<ActivationItem> for ActivationItemDto {
    fn from(item: ActivationItem) -> Self {
        Self {
            subscriber: item.subscriber,
            change_ref: item.change_ref,
            outcome: item.outcome.as_str().to_owned(),
            expected: count(item.expected),
            confirmed: count(item.confirmed),
            failed: count(item.failed),
            silent: count(item.silent),
            silent_excluded: count(item.silent_excluded),
            instances: item.instances.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<ActivationRead> for ActivationDto {
    fn from(read: ActivationRead) -> Self {
        Self {
            tenant: read.tenant.as_hyphenated().to_string(),
            items: read.items.into_iter().map(Into::into).collect(),
        }
    }
}
