//! Deterministic fixed-stage scheduling. Persist this state under repository CAS:
//! all claims/checkpoints are transitions, never in-memory ownership decisions.
use crate::domain::error::DomainError;
use crate::domain::persisted::ActivityStatus;
use chrono::{DateTime, Utc};
use durable_execution_sdk::DefinitionError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[toolkit_macros::domain_model]
pub struct Step {
    pub id: String,
    pub stage: u32,
    pub status: ActivityStatus,
    pub fence: u64,
    pub attempts: u32,
    pub budget_attempts: u32,
    pub lease_until: Option<DateTime<Utc>>,
    pub due_at: DateTime<Utc>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub history: Vec<Attempt>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[toolkit_macros::domain_model]
pub struct Attempt {
    pub number: u32,
    #[serde(default)]
    pub epoch: u64,
    pub fence: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: ActivityStatus,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[toolkit_macros::domain_model]
pub struct Claim {
    pub step: usize,
    pub fence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[toolkit_macros::domain_model]
pub struct StageJournal {
    pub steps: Vec<Step>,
    pub stop_requested: bool,
    pub epoch: u64,
    #[serde(default)]
    pub(crate) last_resume: Option<bool>,
}

impl StageJournal {
    pub fn new(stages: Vec<Vec<String>>, now: DateTime<Utc>) -> Result<Self, DomainError> {
        if stages.is_empty() || stages.iter().any(Vec::is_empty) {
            return Err(DefinitionError::new("empty stage").into());
        }
        let mut names = std::collections::HashSet::new();
        let mut steps = Vec::new();
        for (stage, ids) in stages.into_iter().enumerate() {
            for id in ids {
                if id.is_empty() || !names.insert(id.clone()) {
                    return Err(DefinitionError::new("duplicate or empty step").into());
                }
                steps.push(Step {
                    id,
                    stage: u32::try_from(stage)
                        .map_err(|_| DomainError::Internal("stage index out of range"))?,
                    status: ActivityStatus::Pending,
                    fence: 0,
                    attempts: 0,
                    budget_attempts: 0,
                    lease_until: None,
                    due_at: now,
                    result: None,
                    error: None,
                    history: Vec::new(),
                });
            }
        }
        Ok(Self {
            steps,
            stop_requested: false,
            epoch: 0,
            last_resume: None,
        })
    }

    /// Only the first incomplete stage can run. A failed sibling does not
    /// prevent the remaining independent activities in that stage from executing.
    #[must_use]
    pub fn ready(&self, now: DateTime<Utc>) -> Vec<usize> {
        if self.stop_requested {
            return Vec::new();
        }
        let Some(stage) = self
            .steps
            .iter()
            .find(|s| s.status != ActivityStatus::Succeeded)
            .map(|s| s.stage)
        else {
            return Vec::new();
        };
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.stage == stage
                    && matches!(
                        s.status,
                        ActivityStatus::Pending | ActivityStatus::RetryWait
                    )
                    && s.due_at <= now
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn claim(
        &mut self,
        step: usize,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Claim, DomainError> {
        if !self.ready(now).contains(&step) || lease.is_zero() {
            return Err(DomainError::InvalidState("step is not ready to claim"));
        }
        let until = deadline(now, lease)?;
        let s = &mut self.steps[step];
        let fence = s
            .fence
            .checked_add(1)
            .ok_or(DomainError::Internal("fence overflow"))?;
        let attempts = s
            .attempts
            .checked_add(1)
            .ok_or(DomainError::Internal("attempt overflow"))?;
        let budget = s
            .budget_attempts
            .checked_add(1)
            .ok_or(DomainError::Internal("attempt budget overflow"))?;
        s.fence = fence;
        s.attempts = attempts;
        s.budget_attempts = budget;
        s.status = ActivityStatus::Running;
        s.lease_until = Some(until);
        s.error = None;
        s.history.push(Attempt {
            number: attempts,
            epoch: self.epoch,
            fence,
            started_at: now,
            finished_at: None,
            status: ActivityStatus::Running,
            error: None,
        });
        Ok(Claim { step, fence })
    }

    #[must_use]
    pub fn owns(&self, claim: Claim, now: DateTime<Utc>) -> bool {
        self.steps.get(claim.step).is_some_and(|s| {
            s.status == ActivityStatus::Running
                && s.fence == claim.fence
                && s.lease_until.is_some_and(|t| t > now)
        })
    }

    pub fn heartbeat(
        &mut self,
        claim: Claim,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<(), DomainError> {
        if !self.owns(claim, now) || lease.is_zero() {
            return Err(DomainError::LeaseLost);
        }
        self.steps[claim.step].lease_until = Some(deadline(now, lease)?);
        Ok(())
    }

    pub fn complete(
        &mut self,
        claim: Claim,
        result: Value,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if !self.owns(claim, now) {
            return Err(DomainError::LeaseLost);
        }
        // A completed effect remains checkpointed even if Stop raced with it.
        // Stop prevents new claims; it does not erase confirmed external work.
        let s = &mut self.steps[claim.step];
        s.result = Some(result);
        finish(s, ActivityStatus::Succeeded, None, now);
        Ok(())
    }

    pub fn fail(
        &mut self,
        claim: Claim,
        code: &str,
        retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if !self.owns(claim, now) {
            return Err(DomainError::LeaseLost);
        }
        let code = durable_execution_sdk::ActivityError::permanent(code).code();
        let status = if self.stop_requested {
            ActivityStatus::Cancelled
        } else if retry_at.is_some() {
            ActivityStatus::RetryWait
        } else {
            ActivityStatus::Failed
        };
        let s = &mut self.steps[claim.step];
        if let Some(at) = retry_at {
            s.due_at = at.max(now);
        }
        finish(s, status, Some(code), now);
        Ok(())
    }

    /// Active activities must acknowledge cancellation or lose their leases.
    /// Until then `settled()` remains false and Resume is rejected.
    pub fn stop(&mut self) {
        self.stop_requested = true;
        for s in &mut self.steps {
            if matches!(
                s.status,
                ActivityStatus::Pending | ActivityStatus::RetryWait
            ) {
                s.status = ActivityStatus::Cancelled;
            }
        }
    }

    pub fn recover(&mut self, now: DateTime<Utc>) {
        for s in &mut self.steps {
            if s.status == ActivityStatus::Running && s.lease_until.is_none_or(|t| t <= now) {
                let status = if self.stop_requested {
                    ActivityStatus::Cancelled
                } else {
                    ActivityStatus::Pending
                };
                finish(s, status, Some("lease_expired".into()), now);
                s.due_at = now;
            }
        }
    }

    #[must_use]
    pub fn settled(&self) -> bool {
        !self
            .steps
            .iter()
            .any(|s| s.status == ActivityStatus::Running)
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.steps
            .iter()
            .all(|s| s.status == ActivityStatus::Succeeded)
    }

    pub fn continue_execution(
        &mut self,
        expected_epoch: u64,
        resume: bool,
        now: DateTime<Utc>,
    ) -> Result<bool, DomainError> {
        // The caller must persist this transition under CAS; a duplicate command
        // must not reset the budget of a more recent execution.
        if self.epoch
            == expected_epoch
                .checked_add(1)
                .ok_or(DomainError::Internal("epoch overflow"))?
            && self.last_resume == Some(resume)
        {
            return Ok(false);
        }
        if self.epoch != expected_epoch {
            return Err(DomainError::ConcurrentUpdate);
        }
        if !self.settled()
            || self.succeeded()
            || resume != self.stop_requested
            || (!resume
                && (!self.ready(now).is_empty()
                    || self
                        .steps
                        .iter()
                        .any(|s| s.status == ActivityStatus::RetryWait)))
            || (!resume
                && !self
                    .steps
                    .iter()
                    .any(|s| s.status == ActivityStatus::Failed))
        {
            return Err(DomainError::InvalidState(
                "stage journal cannot continue from its current state",
            ));
        }
        for s in &mut self.steps {
            if s.status != ActivityStatus::Succeeded {
                s.status = ActivityStatus::Pending;
                s.budget_attempts = 0;
                s.error = None;
                s.due_at = now;
            }
        }
        self.epoch += 1;
        self.last_resume = Some(resume);
        self.stop_requested = false;
        Ok(true)
    }
}

fn finish(s: &mut Step, status: ActivityStatus, error: Option<String>, now: DateTime<Utc>) {
    s.status = status;
    s.lease_until = None;
    s.error.clone_from(&error);
    if let Some(attempt) = s.history.last_mut() {
        attempt.status = status;
        attempt.finished_at = Some(now);
        attempt.error = error;
    }
}

fn deadline(now: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, DomainError> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|d| now.checked_add_signed(d))
        .ok_or(DomainError::Internal("duration out of range"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/parallel_tests.rs"]
mod tests;
