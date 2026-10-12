//! Fixtures the merge and door tests share.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cmp::Ordering;

use bss_approvals_sdk::{InboxUnit, Order, SortKey, UnitState};
use time::OffsetDateTime;
use uuid::Uuid;

/// # Panics
/// When `secs` is not a Unix timestamp `time` can represent.
#[must_use]
pub fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).expect("timestamp")
}

/// # Panics
/// When the instant is not representable.
#[must_use]
pub fn at_micros(secs: i64, micros: i64) -> OffsetDateTime {
    at(secs) + time::Duration::microseconds(micros)
}

#[must_use]
pub fn unit(source: &str, secs: i64, id: u128) -> InboxUnit {
    unit_at(source, at(secs), id)
}

#[must_use]
pub fn unit_at(source: &str, submitted_at: OffsetDateTime, id: u128) -> InboxUnit {
    InboxUnit {
        id: Uuid::from_u128(id),
        source: source.to_owned(),
        kind: bss_approvals_sdk::InboxKind::Prices,
        ref_type: "price_book".to_owned(),
        ref_id: Uuid::from_u128(9),
        state: UnitState::Pending,
        generation: 1,
        quorum_required: 1,
        common_effective_date: None,
        submitted_by: Uuid::from_u128(3),
        submitted_at,
        submit_note: None,
        decided_at: None,
        decided_note: None,
        snapshot: serde_json::json!({}),
        decisions: Vec::new(),
        caller_can_approve: true,
        caller_can_reject: true,
        caller_can_withdraw: false,
        subject_live: None,
        impact: None,
    }
}

/// The units strictly after `after`, in `order`, at most `limit`, and whether any remain.
#[must_use]
pub fn page_after(
    units: &[InboxUnit],
    order: Order,
    limit: u32,
    after: Option<SortKey>,
) -> (Vec<InboxUnit>, bool) {
    let mut rows: Vec<InboxUnit> = units
        .iter()
        .filter(|unit| strictly_after(unit, after, order))
        .cloned()
        .collect();
    rows.sort_by(|left, right| cmp_order(left, right, order));
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    (rows, has_more)
}

fn strictly_after(unit: &InboxUnit, after: Option<SortKey>, order: Order) -> bool {
    let Some(after) = after else {
        return true;
    };
    let key = (unit.submitted_at, unit.id);
    let bound = (after.submitted_at, after.id);
    match order {
        Order::Asc => key > bound,
        Order::Desc => key < bound,
    }
}

fn cmp_order(left: &InboxUnit, right: &InboxUnit, order: Order) -> Ordering {
    let left_key = (left.submitted_at, left.id);
    let right_key = (right.submitted_at, right.id);
    match order {
        Order::Asc => left_key.cmp(&right_key),
        Order::Desc => right_key.cmp(&left_key),
    }
}

/// # Panics
/// When the test caller cannot be built.
#[must_use]
pub fn caller() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_type("user")
        .subject_tenant_id(Uuid::from_u128(2))
        .build()
        .expect("caller")
}
