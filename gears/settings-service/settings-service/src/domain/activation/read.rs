// @cpt-dod:cpt-cf-settings-service-dod-activation-read-instances:p1
// @cpt-dod:cpt-cf-settings-service-dod-activation-read-identities:p1
//! What the activation read shows: per subscriber, the instances of the
//! current change and how they stand.
//!
//! The outcome is derived here, at the read, over the rows that exist when it
//! runs, and is never stored: a forgotten, unsubscribed, evicted or purged
//! instance leaves the rows and the next read derives over those that remain.
//! Nothing here carries a value, an expected or applied value, or a hash.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use toolkit_macros::domain_model;
use uuid::Uuid;

use super::{ActivationStatus, Subscription};

/// The derived outcome of a subscriber's current change: `failed` over
/// `incomplete` over `awaiting` over `succeeded`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// Some row is still outstanding and none is `failed` or `silent`.
    Awaiting,
    /// Every expected instance confirmed.
    Succeeded,
    /// Some row is `failed`, whatever the others are.
    Failed,
    /// No row is `failed` and some is `silent`: the wait ended unanswered.
    Incomplete,
}

impl ChangeOutcome {
    /// The wire spelling, permanent once shipped.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Awaiting => "awaiting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Incomplete => "incomplete",
        }
    }
}

/// One `activation_state` row as the read needs it, with the label of its
/// subscription: the state of the instance and nothing of its content.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRow {
    /// The consumer identity.
    pub subscriber: String,
    /// The consumer instance.
    pub instance_id: Uuid,
    /// The instance's label, absent when it has none or its subscription is.
    pub instance_label: Option<String>,
    /// The change the row answers: its audit record's id.
    pub change_ref: Uuid,
    /// Where the row stands.
    pub status: ActivationStatus,
    /// When the instance was first called silent; kept when a late report
    /// settled the row.
    pub silent_since: Option<time::OffsetDateTime>,
    /// When the settling report arrived.
    pub reported_at: Option<time::OffsetDateTime>,
    /// The consumer's failure detail.
    pub detail: Option<String>,
}

/// One instance's entry of an item: a row of the current change, or a silent
/// instance the change left out.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationEntry {
    /// The instance; `None` once the reader is not shown identities.
    pub instance_id: Option<Uuid>,
    /// The instance's label; `None` when it has none or the reader is not
    /// shown identities.
    pub instance_label: Option<String>,
    /// Where the instance stands.
    pub status: ActivationStatus,
    /// Whether the entry is a silent instance the change left out, counted in
    /// none of `expected`, `confirmed`, `failed` and `silent`.
    pub excluded: bool,
    /// When the settling report arrived; present on a `succeeded` or `failed`
    /// row only.
    pub reported_at: Option<time::OffsetDateTime>,
    /// When the instance was first called silent: on a `silent` entry and on a
    /// row a late report settled.
    pub silent_since: Option<time::OffsetDateTime>,
    /// The consumer's failure detail; present on a `failed` row only.
    pub detail: Option<String>,
}

/// One subscriber's account of its current change at `(key, tenant)`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationItem {
    /// The consumer identity.
    pub subscriber: String,
    /// The change the rows answer: its audit record's id.
    pub change_ref: Uuid,
    /// The outcome, derived at the read.
    pub outcome: ChangeOutcome,
    /// `M`: the number of rows of the change.
    pub expected: usize,
    /// `N`: the rows `succeeded`.
    pub confirmed: usize,
    /// The rows `failed`.
    pub failed: usize,
    /// The rows `silent`.
    pub silent: usize,
    /// The entries with `excluded: true`.
    pub silent_excluded: usize,
    /// The rows of the change, then the excluded entries, each part by status
    /// and then by instance id.
    pub instances: Vec<ActivationEntry>,
}

/// What the activation read answers: the tenant as normalised and one item per
/// subscriber with rows, in subscriber order.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRead {
    /// The tenant the read answers for.
    pub tenant: Uuid,
    /// The items.
    pub items: Vec<ActivationItem>,
}

/// The position of a status in the order entries are listed by.
const fn rank(status: ActivationStatus) -> u8 {
    match status {
        ActivationStatus::Awaiting => 0,
        ActivationStatus::Succeeded => 1,
        ActivationStatus::Failed => 2,
        ActivationStatus::Silent => 3,
    }
}

/// An entry as it is ordered: the identity is decided on whoever reads, so the
/// key carries it beside the entry.
struct Keyed {
    identity: String,
    entry: ActivationEntry,
}

fn by_identity(a: &Keyed, b: &Keyed) -> Ordering {
    rank(a.entry.status)
        .cmp(&rank(b.entry.status))
        .then_with(|| a.identity.cmp(&b.identity))
}

/// The entry of a row: `reported_at` only for a settled one, `detail` only for
/// a failed one, `silent_since` whenever the row carries it.
fn entry_of_row(row: ActivationRow) -> Keyed {
    // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-11
    let settled = matches!(
        row.status,
        ActivationStatus::Succeeded | ActivationStatus::Failed
    );
    let failed = row.status == ActivationStatus::Failed;
    Keyed {
        identity: row.instance_id.to_string(),
        entry: ActivationEntry {
            instance_id: Some(row.instance_id),
            instance_label: row.instance_label,
            status: row.status,
            excluded: false,
            reported_at: row.reported_at.filter(|_| settled),
            silent_since: row.silent_since,
            detail: row.detail.filter(|_| failed),
        },
    }
    // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-11
}

/// The entry of a silent instance the change left out.
fn entry_of_excluded(subscription: &Subscription) -> Keyed {
    Keyed {
        identity: subscription.instance_id.to_string(),
        entry: ActivationEntry {
            instance_id: Some(subscription.instance_id),
            instance_label: subscription.instance_label.clone(),
            status: ActivationStatus::Silent,
            excluded: true,
            reported_at: None,
            silent_since: subscription.silent_since,
            detail: None,
        },
    }
}

/// The outcome and the counts of one item's entries.
///
/// `Counts` are `expected`, `confirmed`, `failed`, `silent` and
/// `silent_excluded`.
fn derive(entries: &[ActivationEntry]) -> (ChangeOutcome, [usize; 5]) {
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-1
    let count = |status: ActivationStatus| {
        entries
            .iter()
            .filter(|e| !e.excluded && e.status == status)
            .count()
    };
    let (awaiting, confirmed, failed, silent) = (
        count(ActivationStatus::Awaiting),
        count(ActivationStatus::Succeeded),
        count(ActivationStatus::Failed),
        count(ActivationStatus::Silent),
    );
    let expected = entries.iter().filter(|e| !e.excluded).count();
    let silent_excluded = entries.iter().filter(|e| e.excluded).count();
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-1
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-2
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-3
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-4
    let outcome = if failed > 0 {
        ChangeOutcome::Failed
    } else if silent > 0 {
        ChangeOutcome::Incomplete
    } else if awaiting > 0 {
        ChangeOutcome::Awaiting
    } else {
        ChangeOutcome::Succeeded
    };
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-4
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-3
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-outcome:p1:inst-ar-outcome-2
    (
        outcome,
        [expected, confirmed, failed, silent, silent_excluded],
    )
}

/// One item per subscriber that has rows, in ascending subscriber order, from
/// the `rows` of `(key, tenant)` and its `silent` subscriptions.
///
/// A subscriber's current change is the newest by `change_ref` among its rows
/// (a `UUIDv7`) and only its rows are counted. Each silent subscription of a
/// subscriber that has an item and has no row of the current change is added
/// as an excluded entry; a subscriber with no row has no item.
#[must_use]
pub fn build_items(rows: Vec<ActivationRow>, silent: &[Subscription]) -> Vec<ActivationItem> {
    let mut by_subscriber: BTreeMap<String, Vec<ActivationRow>> = BTreeMap::new();
    for row in rows {
        by_subscriber
            .entry(row.subscriber.clone())
            .or_default()
            .push(row);
    }
    by_subscriber
        .into_iter()
        .filter_map(|(subscriber, rows)| {
            let change_ref = rows.iter().map(|row| row.change_ref).max()?;
            let current: Vec<ActivationRow> = rows
                .into_iter()
                .filter(|row| row.change_ref == change_ref)
                .collect();
            let with_row: HashSet<Uuid> = current.iter().map(|row| row.instance_id).collect();
            let mut own: Vec<Keyed> = current.into_iter().map(entry_of_row).collect();
            let mut left_out: Vec<Keyed> = silent
                .iter()
                .filter(|s| s.subscriber == subscriber && !with_row.contains(&s.instance_id))
                .map(entry_of_excluded)
                .collect();
            // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-15
            // The rows first, then the excluded entries, each part by status
            // and then by the identity's lowercase string.
            own.sort_by(by_identity);
            left_out.sort_by(by_identity);
            let instances: Vec<ActivationEntry> =
                own.into_iter().chain(left_out).map(|k| k.entry).collect();
            // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-15
            // @cpt-begin:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-14
            let (outcome, [expected, confirmed, failed, silent, silent_excluded]) =
                derive(&instances);
            // @cpt-end:cpt-cf-settings-service-flow-activation-read-status:p1:inst-ar-status-14
            Some(ActivationItem {
                subscriber,
                change_ref,
                outcome,
                expected,
                confirmed,
                failed,
                silent,
                silent_excluded,
                instances,
            })
        })
        .collect()
}

impl ActivationRead {
    /// Apply identity disclosure for a reader whose own tenant is the root
    /// (`platform`) or is not: every entry keeps `instance_id` and
    /// `instance_label` for the first and loses both — absent, never masked —
    /// for the second. Status, `excluded`, times, `detail`, counts, outcome and
    /// order are untouched, since they were decided on the identity before.
    #[must_use]
    pub fn disclosed_to(mut self, platform: bool) -> Self {
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-1
        if platform {
            return self;
        }
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-1
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-2
        for entry in self
            .items
            .iter_mut()
            .flat_map(|item| item.instances.iter_mut())
        {
            entry.instance_id = None;
            entry.instance_label = None;
        }
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-2
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-3
        self
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-identity:p1:inst-ar-identity-3
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;
