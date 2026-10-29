//! `SourceLossTracker` (feature 04 §3.4; ADR-0007; DESIGN §3.7 `bss_rating__source_loss`,
//! [12 §4.6]).
//!
//! A source loss is recorded whenever the feed cannot prove that nothing was lost: the collector
//! refused the cursor because retention removed entries after it, an entry could not be
//! identified, or a widened scope exposed a tenant whose older history is past retention. One row
//! per loss, against the source scope (its configured tenant scope × its meters), never per
//! enumerated tenant. It is opened in the transaction that detects it, under the checkpoint lock
//! and cursor CAS, and nothing here ever resolves it: only reconciliation evidence or an audited
//! operator resolution does (feature 11).
//!
//! [12 §4.6]: ../../../../docs/DESIGN.md#contract-12-4-6

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use time::{Date, OffsetDateTime};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::usage::feed::{FeedCursor, MeterId};
use crate::domain::usage::source::{SourceId, TenantScope};

/// Why a source lost entries.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LossKind {
    /// `InvalidArgument(CursorBeyondRetention)`: retention removed entries after the cursor.
    CursorBeyondRetention,
    /// An entry whose `id` or `tenant_id` could not be read.
    UnidentifiableEntry,
    /// A widened scope exposed a tenant whose history before the retention floor is gone.
    ScopeWidenedBeyondRetention,
}

impl LossKind {
    /// Every kind, in a stable order.
    pub const ALL: [Self; 3] = [
        Self::CursorBeyondRetention,
        Self::UnidentifiableEntry,
        Self::ScopeWidenedBeyondRetention,
    ];

    /// The stored name and the `kind` label of `rating_source_loss_open`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CursorBeyondRetention => "cursor_beyond_retention",
            Self::UnidentifiableEntry => "unidentifiable_entry",
            Self::ScopeWidenedBeyondRetention => "scope_widened_beyond_retention",
        }
    }

    /// The kind a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

/// The lifecycle of a loss (feature 04 §4.2); this feature only opens losses.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LossState {
    /// Nothing proven yet.
    Open,
    /// Reconciliation proved some tainted days.
    Narrowed,
    /// An `accepted_loss` awaits a second operator.
    PendingApproval,
    /// Repaired or accepted.
    Resolved,
}

impl LossState {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Narrowed => "narrowed",
            Self::PendingApproval => "pending_approval",
            Self::Resolved => "resolved",
        }
    }

    /// The state a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Open,
            Self::Narrowed,
            Self::PendingApproval,
            Self::Resolved,
        ]
        .into_iter()
        .find(|state| state.as_str() == name)
    }
}

/// An inclusive, non-empty range of UTC days, a day being the UTC day of an entry's `window_end`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DayRange {
    first: Date,
    last: Date,
}

impl DayRange {
    /// `first..=last`, or `None` when `last` precedes `first`.
    #[must_use]
    pub fn new(first: Date, last: Date) -> Option<Self> {
        (first <= last).then_some(Self { first, last })
    }

    /// The first day.
    #[must_use]
    pub const fn first(self) -> Date {
        self.first
    }

    /// The last day.
    #[must_use]
    pub const fn last(self) -> Date {
        self.last
    }
}

/// The `(meter, day)` set a loss has not proven: per meter, its day ranges. Ranges keep a long
/// outage compact (a stuck reader over 100 meters is still one range per meter).
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaintedDays(BTreeMap<MeterId, Vec<DayRange>>);

impl TaintedDays {
    /// No tainted day.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// `first..=last` for every one of `meters`; nothing when `last < first`.
    #[must_use]
    pub fn range<'a>(
        meters: impl IntoIterator<Item = &'a MeterId>,
        first: Date,
        last: Date,
    ) -> Self {
        let Some(range) = DayRange::new(first, last) else {
            return Self::none();
        };
        Self(
            meters
                .into_iter()
                .map(|meter| (meter.clone(), vec![range]))
                .collect(),
        )
    }

    /// The ranges of each meter.
    #[must_use]
    pub const fn ranges(&self) -> &BTreeMap<MeterId, Vec<DayRange>> {
        &self.0
    }

    /// The stored form: `[{gts_type, first, last}]`, days as `YYYY-MM-DD`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Array(
            self.0
                .iter()
                .flat_map(|(meter, ranges)| {
                    ranges.iter().map(move |range| {
                        json!({
                            "gts_type": meter.as_str(),
                            "first": day_text(range.first),
                            "last": day_text(range.last),
                        })
                    })
                })
                .collect(),
        )
    }

    /// The stored form read back.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidArgument`] when it is not what [`Self::to_json`] writes.
    pub fn from_json(value: &Value) -> Result<Self, DomainError> {
        let malformed = || DomainError::InvalidArgument {
            field: "tainted_days",
            detail: "not a list of meter day ranges".to_owned(),
            reason: bss_rating_sdk::errors::reason::INVALID_REQUEST,
        };
        let mut days: BTreeMap<MeterId, Vec<DayRange>> = BTreeMap::new();
        for range in value.as_array().ok_or_else(malformed)? {
            let field = |name: &str| {
                range
                    .get(name)
                    .and_then(Value::as_str)
                    .ok_or_else(malformed)
            };
            let meter = MeterId::parse(field("gts_type")?)?;
            let first = parse_day(field("first")?).ok_or_else(malformed)?;
            let last = parse_day(field("last")?).ok_or_else(malformed)?;
            let range = DayRange::new(first, last).ok_or_else(malformed)?;
            days.entry(meter).or_default().push(range);
        }
        Ok(Self(days))
    }
}

fn day_text(day: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        day.year(),
        u8::from(day.month()),
        day.day()
    )
}

fn parse_day(text: &str) -> Option<Date> {
    let mut parts = text.splitn(3, '-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let month = time::Month::try_from(month.parse::<u8>().ok()?).ok()?;
    Date::from_calendar_date(year.parse().ok()?, month, day.parse().ok()?).ok()
}

/// `instant − floor`, saturating at the earliest representable instant.
fn minus(instant: OffsetDateTime, floor: Duration) -> OffsetDateTime {
    time::Duration::try_from(floor)
        .ok()
        .and_then(|floor| instant.checked_sub(floor))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The taint of a `cursor_beyond_retention` loss: every meter of the source, the days from
/// `day(last_good_at − F)` to `day(detected_at − F)`. Retention keeps an entry at least `F` from
/// its `window_end`, so an entry removed by `detected_at` ends at or before `detected_at − F`, and
/// one the collector still held at `last_good_at` ends at or after `last_good_at − F`.
#[must_use]
pub fn retention_taint(
    meters: &[MeterId],
    last_good_at: OffsetDateTime,
    detected_at: OffsetDateTime,
    floor: Duration,
) -> TaintedDays {
    TaintedDays::range(
        meters,
        minus(last_good_at, floor).date(),
        minus(detected_at, floor).date(),
    )
}

/// The taint of an `unidentifiable_entry` loss: every day the feed still holds, `day(detected_at −
/// F)` to `day(detected_at)`, since the entry's own period cannot be trusted.
#[must_use]
pub fn unidentifiable_taint(
    meters: &[MeterId],
    detected_at: OffsetDateTime,
    floor: Duration,
) -> TaintedDays {
    TaintedDays::range(meters, minus(detected_at, floor).date(), detected_at.date())
}

/// A loss to open.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSourceLoss {
    /// Its id.
    pub loss_id: Uuid,
    /// The tenant the row belongs to: always the owner root of the source's configured scope
    /// ([`TenantScope::owner`]), whatever the kind. A `scope_widened_beyond_retention` loss names
    /// the newly visible tenants in `tenant_scope`, not here.
    pub tenant_id: Uuid,
    /// The source.
    pub source_id: SourceId,
    /// The scope it is against: the source's configured scope, or, for a
    /// `scope_widened_beyond_retention` loss, the tenants the widening exposed.
    pub tenant_scope: TenantScope,
    /// Its kind.
    pub kind: LossKind,
    /// The source's meters.
    pub meters: Vec<MeterId>,
    /// When it was detected.
    pub detected_at: OffsetDateTime,
    /// The checkpoint's cursor when it was detected.
    pub last_good_cursor: Option<FeedCursor>,
    /// When that cursor was committed.
    pub last_good_at: Option<OffsetDateTime>,
    /// The cursor the detecting transaction wrote; `None` restarts at `Oldest`.
    pub restart_cursor: Option<FeedCursor>,
    /// The days it has not proven.
    pub tainted: TaintedDays,
}

/// A stored loss.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLoss {
    /// Its id.
    pub loss_id: Uuid,
    /// The tenant the row belongs to.
    pub tenant_id: Uuid,
    /// The source.
    pub source_id: SourceId,
    /// Its kind.
    pub kind: LossKind,
    /// Its state.
    pub state: LossState,
    /// The cursor it was detected at.
    pub last_good_cursor: Option<FeedCursor>,
    /// The days it has not proven.
    pub tainted: TaintedDays,
}

#[cfg(test)]
#[path = "loss_tests.rs"]
mod tests;
