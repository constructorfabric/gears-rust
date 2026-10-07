//! The time-stable facts of a plan's list row (D-484).
//!
//! A stored state would go stale at midnight: a scheduled revision reads published from its date
//! (D-447). The summary stores only facts that do not depend on the day. `selling` and `change`
//! are derived from it and the request's day, in SQL and again in Rust from the same rows.
use crate::domain::plan::RevisionState;
use crate::infra::storage::RepoError;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

/// One revision as the summary reads it. `currency` is its book's, when the book was read with it.
#[derive(Debug, Clone)]
pub struct RevisionFact {
    pub id: Uuid,
    pub state: String,
    pub available_from: Option<Date>,
    pub updated_at: OffsetDateTime,
    pub book_id: Uuid,
    pub currency: Option<String>,
}

/// The columns `000020` stores on `pricing_plan`. `last_activity_at` is the latest `updated_at`
/// of the plan and every revision, superseded ones included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub work_revision_id: Option<Uuid>,
    pub work_state: Option<String>,
    pub scheduled_revision_id: Option<Uuid>,
    pub scheduled_from: Option<Date>,
    pub published_revision_id: Option<Uuid>,
    pub current_book_id: Option<Uuid>,
    pub current_currency: Option<String>,
    pub last_activity_at: OffsetDateTime,
}

/// Recompute the summary from the plan's `updated_at` and its revisions.
///
/// The current book is `work ?? scheduled ?? published` (D-460's current revision whether or not
/// the scheduled one is due). A superseded revision changes only `last_activity_at`.
/// # Errors
/// `CorruptRow` when a plan holds two revisions of one open kind, a state outside the closed set,
/// or a scheduled revision with no date (the column pair would not hold).
pub fn summarize(
    plan_updated_at: OffsetDateTime,
    revisions: &[RevisionFact],
) -> Result<Summary, RepoError> {
    let mut last = plan_updated_at;
    let mut work: Option<&RevisionFact> = None;
    let mut scheduled: Option<&RevisionFact> = None;
    let mut published: Option<&RevisionFact> = None;
    for revision in revisions {
        last = last.max(revision.updated_at);
        let state: RevisionState = revision.state.parse().map_err(|_| {
            RepoError::CorruptRow(format!(
                "plan revision {} state {:?}",
                revision.id, revision.state
            ))
        })?;
        if state == RevisionState::Superseded {
            continue;
        }
        let slot = match state {
            RevisionState::Draft | RevisionState::Pending => &mut work,
            RevisionState::Scheduled => &mut scheduled,
            RevisionState::Published => &mut published,
            RevisionState::Superseded => continue,
        };
        if slot.is_some() {
            return Err(RepoError::CorruptRow(format!(
                "plan holds two {} revisions",
                state.as_str()
            )));
        }
        if state == RevisionState::Scheduled && revision.available_from.is_none() {
            return Err(RepoError::CorruptRow(format!(
                "scheduled plan revision {} has no date",
                revision.id
            )));
        }
        *slot = Some(revision);
    }
    let current = work.or(scheduled).or(published);
    Ok(Summary {
        work_revision_id: work.map(|r| r.id),
        work_state: work.map(|r| r.state.clone()),
        scheduled_revision_id: scheduled.map(|r| r.id),
        scheduled_from: scheduled.and_then(|r| r.available_from),
        published_revision_id: published.map(|r| r.id),
        current_book_id: current.map(|r| r.book_id),
        current_currency: current.and_then(|r| r.currency.clone()),
        last_activity_at: last,
    })
}

/// `selling` from the stored summary and the request's day (D-484). Never null: a draft-only plan
/// and a plan with no revisions are false.
#[must_use]
pub fn selling(summary: &Summary, today: Date) -> bool {
    summary.published_revision_id.is_some()
        || summary.scheduled_from.is_some_and(|from| from <= today)
}

/// `change` from the stored summary and the request's day: the work state, else `scheduled` while
/// that revision's date is still ahead, else `none`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanChange {
    None,
    Draft,
    Pending,
    Scheduled,
}
impl PlanChange {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Draft => "draft",
            Self::Pending => "pending",
            Self::Scheduled => "scheduled",
        }
    }
}
#[must_use]
pub fn change(summary: &Summary, today: Date) -> PlanChange {
    match summary.work_state.as_deref() {
        Some("draft") => PlanChange::Draft,
        Some("pending") => PlanChange::Pending,
        _ if summary.scheduled_from.is_some_and(|from| from > today) => PlanChange::Scheduled,
        _ => PlanChange::None,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "fixture dates and summaries are well formed"
)]
mod tests {
    use super::*;

    fn at(day: i32) -> OffsetDateTime {
        Date::from_calendar_date(2026, time::Month::January, u8::try_from(day).unwrap())
            .unwrap()
            .midnight()
            .assume_utc()
    }
    fn day(n: u8) -> Date {
        Date::from_calendar_date(2026, time::Month::June, n).unwrap()
    }
    fn rev(n: u128, state: &str, from: Option<Date>, updated: OffsetDateTime) -> RevisionFact {
        RevisionFact {
            id: Uuid::from_u128(n),
            state: state.to_owned(),
            available_from: from,
            updated_at: updated,
            book_id: Uuid::from_u128(0xB000 + n),
            currency: Some("EUR".to_owned()),
        }
    }

    #[test]
    fn a_plan_with_no_revisions_is_not_selling_and_not_changing() {
        let summary = summarize(at(1), &[]).unwrap();
        assert_eq!(summary.last_activity_at, at(1));
        assert!(summary.work_revision_id.is_none());
        assert!(summary.published_revision_id.is_none());
        assert!(!selling(&summary, day(1)));
        assert_eq!(change(&summary, day(1)), PlanChange::None);
    }

    #[test]
    fn a_draft_beside_a_published_revision_is_current() {
        let draft = rev(1, "draft", None, at(3));
        let published = rev(2, "published", None, at(2));
        let summary = summarize(at(1), &[draft.clone(), published]).unwrap();
        assert_eq!(summary.work_revision_id, Some(draft.id));
        assert_eq!(summary.work_state.as_deref(), Some("draft"));
        assert_eq!(summary.published_revision_id, Some(Uuid::from_u128(2)));
        assert_eq!(summary.current_book_id, Some(draft.book_id));
        assert_eq!(summary.last_activity_at, at(3));
        assert!(selling(&summary, day(1)));
        assert_eq!(change(&summary, day(1)), PlanChange::Draft);
    }

    #[test]
    fn a_future_scheduled_revision_waits_and_a_due_one_is_selling() {
        let scheduled = rev(3, "scheduled", Some(day(15)), at(4));
        let summary = summarize(at(1), &[scheduled]).unwrap();
        assert_eq!(summary.scheduled_from, Some(day(15)));
        assert_eq!(summary.current_book_id, Some(Uuid::from_u128(0xB003)));
        assert!(!selling(&summary, day(14)));
        assert_eq!(change(&summary, day(14)), PlanChange::Scheduled);
        assert!(selling(&summary, day(15)));
        assert_eq!(change(&summary, day(15)), PlanChange::None);
    }

    #[test]
    fn superseded_history_moves_only_the_activity() {
        let old = rev(1, "superseded", None, at(9));
        let published = rev(2, "published", None, at(2));
        let summary = summarize(at(1), &[old, published.clone()]).unwrap();
        assert!(summary.work_revision_id.is_none());
        assert!(summary.scheduled_revision_id.is_none());
        assert_eq!(summary.published_revision_id, Some(published.id));
        assert_eq!(summary.current_book_id, Some(published.book_id));
        assert_eq!(summary.last_activity_at, at(9));
    }
}
