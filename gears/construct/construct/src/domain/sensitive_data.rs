//! The sensitive-data checks: one check per sensitive-data kind over the
//! values a plan would store, run before anything is stored.
//!
//! This part builds the interface and the runner. No real check exists yet:
//! until the owner of the checks is settled, a deployment runs none, so every
//! value is allowed. Admission enforces whatever verdicts the checks return.

use std::ops::Range;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;
use toolkit_macros::domain_model;

use super::error::DomainError;
use super::plan::{Plan, PlanValue, ValueRef};
use super::record_intake::{DropEvent, DropReason, DropStage, IntakeEvents};

/// The special categories of a person's profile.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SensitiveDataKind {
    PersonalIds,
    Health,
    Finance,
    Biometrics,
    Credentials,
    Contacts,
    Location,
    Politics,
    Sex,
    OtherProtected,
}

/// What Admission does with a text piece in which a check found content.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Drop the whole value the piece belongs to, and keep the rest of the
    /// plan.
    Block,
    /// Remove the found content from the piece; the value stays valid JSON.
    Redact,
}

impl SensitiveDataKind {
    pub const ALL: [Self; 10] = [
        Self::PersonalIds,
        Self::Health,
        Self::Finance,
        Self::Biometrics,
        Self::Credentials,
        Self::Contacts,
        Self::Location,
        Self::Politics,
        Self::Sex,
        Self::OtherProtected,
    ];

    /// The action the DESIGN gives each sensitive-data kind.
    #[must_use]
    pub fn action(self) -> Action {
        // @cpt-begin:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-action
        match self {
            Self::PersonalIds | Self::Finance | Self::Contacts | Self::Location => Action::Redact,
            Self::Health
            | Self::Biometrics
            | Self::Credentials
            | Self::Politics
            | Self::Sex
            | Self::OtherProtected => Action::Block,
        }
        // @cpt-end:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-action
    }

    /// The name of the kind in audit events.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::PersonalIds => "personal_ids",
            Self::Health => "health",
            Self::Finance => "finance",
            Self::Biometrics => "biometrics",
            Self::Credentials => "credentials",
            Self::Contacts => "contacts",
            Self::Location => "location",
            Self::Politics => "politics",
            Self::Sex => "sex",
            Self::OtherProtected => "other_protected",
        }
    }
}

/// Content of one sensitive-data kind found in one text piece of a value: a
/// string, or a number as its text. `at` names the piece. For a kind that
/// redacts, `spans` are the byte ranges of the found content in that piece's
/// text, and every redacting finding must name at least one. A finding in a
/// number, or one Admission cannot apply exactly, blocks the whole value.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub at: ValueRef,
    pub spans: Vec<Range<usize>>,
}

/// The answer of one check for the text pieces of a plan. A piece without a
/// [`Found`] is allowed; a value is stored only if none of its pieces is
/// blocked.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: SensitiveDataKind,
    pub found: Vec<Found>,
}

impl Verdict {
    #[must_use]
    pub fn allow(kind: SensitiveDataKind) -> Self {
        Self {
            kind,
            found: Vec::new(),
        }
    }
}

/// One check for one sensitive-data kind: a pattern check in code, a model
/// check, or both. It sees the text pieces of the values a plan would store
/// ([`PlanValue`]), never the keys of an object. A model check gets the pieces
/// as a tool call result, never inside its prompt.
#[async_trait]
pub trait SensitiveDataCheck: Send + Sync {
    fn kind(&self) -> SensitiveDataKind;

    /// The verdict for `values`: one [`Found`] for each piece with content of
    /// this check's kind, naming the piece by its [`PlanValue::at`].
    ///
    /// # Errors
    ///
    /// When the check cannot answer, for example because its model call failed
    /// or hit a cap. The record is then dropped.
    async fn check(&self, values: &[PlanValue]) -> Result<Verdict, DomainError>;
}

/// What running the checks on a plan gave.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// Every check answered: one verdict per check.
    Verdicts(Vec<Verdict>),
    /// A check failed or answered about something the plan does not hold. The
    /// drop event is written for a plan built from a record.
    Dropped,
}

/// Runs every check on the values of a plan, in parallel. There is no switch to
/// turn them off; a deployment chooses only which checks exist.
///
/// @cpt-dod:cpt-cf-construct-dod-sensitive-data-checks-interface:p1
#[domain_model]
pub struct SensitiveDataChecks {
    checks: Vec<Arc<dyn SensitiveDataCheck>>,
    events: Arc<dyn IntakeEvents>,
}

impl SensitiveDataChecks {
    #[must_use]
    pub fn new(checks: Vec<Arc<dyn SensitiveDataCheck>>, events: Arc<dyn IntakeEvents>) -> Self {
        Self { checks, events }
    }

    /// The verdicts of every check on the values `plan` would store.
    #[tracing::instrument(skip_all, fields(tenant_id = %plan.tenant_id, subject_id = %plan.subject_id))]
    pub async fn run(&self, plan: &Plan) -> Checked {
        // @cpt-begin:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-values
        let values = plan.values();
        // @cpt-end:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-values

        // @cpt-begin:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-parallel
        let answers = join_all(self.checks.iter().map(|check| {
            let values = &values;
            async move { (check.kind(), check.check(values).await) }
        }))
        .await;
        // @cpt-end:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-parallel

        let mut verdicts = Vec::with_capacity(answers.len());
        for (kind, answer) in answers {
            // @cpt-begin:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-failed
            let verdict = match answer {
                Ok(verdict) if verdict.kind == kind && refers_to(&verdict, &values) => verdict,
                Ok(_) => {
                    tracing::warn!(
                        kind = kind.code(),
                        "a sensitive-data check answered for another kind or a value the plan does not hold"
                    );
                    return self.drop(plan);
                }
                Err(err) => {
                    tracing::warn!(kind = kind.code(), error = %err, "a sensitive-data check failed");
                    return self.drop(plan);
                }
            };
            // @cpt-end:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-failed
            verdicts.push(verdict);
        }
        // @cpt-begin:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-return
        Checked::Verdicts(verdicts)
        // @cpt-end:cpt-cf-construct-algo-sensitive-data-checks-run:p1:inst-run-return
    }

    fn drop(&self, plan: &Plan) -> Checked {
        if let Some(event) = DropEvent::for_plan(plan, DropStage::Checks, DropReason::CheckFailed) {
            self.events.dropped(event);
        }
        Checked::Dropped
    }
}

/// Whether every finding names a text piece of the plan. A span is checked by
/// Admission, which blocks a value whose span it cannot apply.
fn refers_to(verdict: &Verdict, values: &[PlanValue]) -> bool {
    verdict
        .found
        .iter()
        .all(|found| values.iter().any(|value| value.at == found.at))
}
