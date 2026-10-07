use crate::{ActivityId, DefinitionError, ExecutionOwner, RunId};
use async_trait::async_trait;
use aws_lc_rs::digest::{SHA256, digest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_secs: u64,
    /// Maximum base backoff, at most one day. Additive jitter can add 20%.
    pub max_delay_secs: u64,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            initial_delay_secs: 15,
            max_delay_secs: 300,
        }
    }
}
impl RetryPolicy {
    /// `jitter_sample` is an injected 0..=1000 sample; jitter only adds time,
    /// so a provider's Retry-After is never shortened.
    #[must_use]
    #[expect(
        clippy::integer_division,
        reason = "Millisecond jitter deliberately rounds down."
    )]
    pub fn delay(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
        jitter_sample: u16,
    ) -> Duration {
        let factor = 1u64
            .checked_shl(attempt.saturating_sub(1).min(63))
            .unwrap_or(u64::MAX);
        let base = self
            .initial_delay_secs
            .saturating_mul(factor)
            .min(self.max_delay_secs);
        let millis = base.saturating_mul(1000);
        let jitter = millis.saturating_mul(u64::from(jitter_sample.min(1000))) / 5000;
        Duration::from_millis(millis.saturating_add(jitter)).max(retry_after.unwrap_or_default())
    }
}

/// Result payloads are persisted, so return identifiers/counts rather than
/// credentials or copies of domain objects containing private data.
#[async_trait]
pub trait ErasedActivity: Send + Sync {
    async fn execute(
        &self,
        context: ActivityContext,
        input: ActivityInput,
    ) -> Result<Value, ActivityError>;
}
#[derive(Clone, Debug)]
pub struct ActivityContext {
    pub run_id: RunId,
    pub activity_id: ActivityId,
    pub attempt: u32,
    pub execution_epoch: u64,
    pub idempotency_key: String,
    pub owner: ExecutionOwner,
    pub deadline: DateTime<Utc>,
    pub cancellation: CancellationToken,
}
#[derive(Clone)]
pub struct ActivityInput {
    pub run_input: Value,
    pub previous_results: BTreeMap<String, Value>,
}

/// Contains safe machine-readable codes only. Keep underlying error detail in
/// redacted operational diagnostics owned by the implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityError {
    Retryable {
        code: String,
        retry_after: Option<Duration>,
    },
    Permanent {
        code: String,
    },
    Cancelled,
}
impl ActivityError {
    #[must_use]
    pub fn retryable(code: &str) -> Self {
        Self::Retryable {
            code: safe_code(code),
            retry_after: None,
        }
    }
    #[must_use]
    pub fn permanent(code: &str) -> Self {
        Self::Permanent {
            code: safe_code(code),
        }
    }
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Retryable { code, .. } | Self::Permanent { code } => safe_code(code),
            Self::Cancelled => "cancelled".into(),
        }
    }
}
fn safe_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code.to_owned()
    } else {
        "activity_failed".into()
    }
}

#[derive(Clone)]
pub struct ActivityDefinition {
    pub id: ActivityId,
    pub timeout: Duration,
    pub retry: RetryPolicy,
    pub handler: Arc<dyn ErasedActivity>,
}
#[derive(Clone)]
pub struct ExecutionDefinition {
    pub name: String,
    pub activities: Vec<ActivityDefinition>,
    /// Contiguous groups of step IDs executed in parallel. Unlisted steps
    /// remain sequential. Arbitrary dependencies and nested groups are unsupported.
    pub parallel_groups: Vec<Vec<ActivityId>>,
    pub flow: Option<crate::contracts::DataFlowSpec>,
}
/// Serializable execution contract. Safe to register on API processes without
/// constructing worker dependencies or executable handlers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySpec {
    pub id: ActivityId,
    pub timeout: Duration,
    pub retry: RetryPolicy,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionContract {
    pub name: String,
    pub activities: Vec<ActivitySpec>,
    pub parallel_groups: Vec<Vec<ActivityId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<crate::contracts::DataFlowSpec>,
}
impl ExecutionDefinition {
    #[must_use]
    pub fn contract(&self) -> ExecutionContract {
        ExecutionContract {
            name: self.name.clone(),
            activities: self
                .activities
                .iter()
                .map(|a| ActivitySpec {
                    id: a.id.clone(),
                    timeout: a.timeout,
                    retry: a.retry.clone(),
                })
                .collect(),
            parallel_groups: self.parallel_groups.clone(),
            flow: self.flow.clone(),
        }
    }
    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        self.contract().validate()
    }
    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn fingerprint(&self) -> Result<String, DefinitionError> {
        self.contract().fingerprint()
    }
    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn stages(&self) -> Result<Vec<Vec<String>>, DefinitionError> {
        self.contract().stages()
    }
}
impl ExecutionContract {
    /// Validate the versioned identifier without constructing activities.
    /// # Errors
    /// Returns [`DefinitionError`] for an oversized or malformed name.
    pub fn validate_name(name: &str) -> Result<(), DefinitionError> {
        let invalid = |s: &str| DefinitionError::new(s);
        if name.is_empty()
            || name.len() > 160
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
        {
            return Err(invalid("name must be a bounded lowercase identifier"));
        }
        let version = name.rsplit_once(".v").map(|(_, v)| v);
        if !version.is_some_and(|v| {
            !v.is_empty()
                && v.bytes().all(|b| b.is_ascii_digit())
                && v.parse::<u64>().is_ok_and(|n| n > 0)
        }) {
            return Err(invalid(
                "name must end in .v followed by a positive version",
            ));
        }
        Ok(())
    }
    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        let invalid = |s: &str| DefinitionError::new(s);
        Self::validate_name(&self.name)?;
        if self.activities.is_empty() || self.activities.len() > 128 {
            return Err(invalid("requires 1..=128 activities"));
        }
        self.stages()?;
        if let Some(flow) = &self.flow {
            flow.validate(self)?;
        }
        let mut names = HashSet::new();
        for activity in &self.activities {
            let id = &activity.id.0;
            if id.is_empty()
                || id.len() > 80
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
            {
                return Err(invalid("invalid activity id"));
            }
            if !names.insert(id) {
                return Err(invalid("duplicate activity id"));
            }
            if activity.timeout.is_zero() || activity.timeout > Duration::from_hours(24) {
                return Err(invalid("activity timeout must be within one day"));
            }
            if activity.retry.max_attempts == 0
                || activity.retry.initial_delay_secs == 0
                || activity.retry.max_delay_secs < activity.retry.initial_delay_secs
                || activity.retry.max_delay_secs > 86_400
            {
                return Err(invalid("invalid retry policy"));
            }
        }
        Ok(())
    }
    /// Hashes the persisted step contract, not machine-dependent handler addresses.
    /// Changes to handler semantics must use a new definition version.
    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn fingerprint(&self) -> Result<String, DefinitionError> {
        self.validate()?;
        let steps: Vec<_> = self
            .activities
            .iter()
            .map(|a| (&a.id.0, a.timeout.as_millis(), &a.retry))
            .collect();
        // Keep existing millisecond-aligned fingerprints stable. A separate
        // encoding preserves the full Duration for higher-precision contracts.
        let bytes = if let Some(flow) = &self.flow {
            let precise_steps: Vec<_> = self
                .activities
                .iter()
                .map(|a| (&a.id.0, a.timeout.as_nanos(), &a.retry))
                .collect();
            serde_json::to_vec(&(
                "durable-flow-v1",
                &self.name,
                precise_steps,
                &self.parallel_groups,
                flow,
            ))
        } else if self
            .activities
            .iter()
            .any(|a| !a.timeout.subsec_nanos().is_multiple_of(1_000_000))
        {
            let precise_steps: Vec<_> = self
                .activities
                .iter()
                .map(|a| (&a.id.0, a.timeout.as_nanos(), &a.retry))
                .collect();
            serde_json::to_vec(&(
                "durable-contract-nanos-v1",
                &self.name,
                &precise_steps,
                &self.parallel_groups,
            ))
        } else if self.parallel_groups.is_empty() {
            serde_json::to_vec(&(&self.name, &steps))
        } else {
            serde_json::to_vec(&(&self.name, &steps, &self.parallel_groups))
        }
        .map_err(|_| DefinitionError::new("cannot encode definition"))?;
        Ok(hex::encode(digest(&SHA256, &bytes).as_ref()))
    }

    /// # Errors
    /// Returns [`DefinitionError`] for invalid names, activity policies or groups.
    pub fn stages(&self) -> Result<Vec<Vec<String>>, DefinitionError> {
        let invalid =
            || DefinitionError::new("parallel groups must contain distinct contiguous step IDs");
        let mut groups = BTreeMap::new();
        let mut seen = HashSet::new();
        for group in &self.parallel_groups {
            if group.len() < 2 {
                return Err(invalid());
            }
            let start = self
                .activities
                .iter()
                .position(|a| a.id == group[0])
                .ok_or_else(invalid)?;
            for (offset, id) in group.iter().enumerate() {
                if self
                    .activities
                    .get(start + offset)
                    .is_none_or(|a| a.id != *id)
                    || !seen.insert(id.0.clone())
                {
                    return Err(invalid());
                }
            }
            groups.insert(start, group.len());
        }
        let mut stages = Vec::new();
        let mut i = 0;
        while i < self.activities.len() {
            let count = groups.get(&i).copied().unwrap_or(1);
            stages.push(
                self.activities[i..i + count]
                    .iter()
                    .map(|a| a.id.0.clone())
                    .collect(),
            );
            i += count;
        }
        Ok(stages)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/activity_tests.rs"]
mod tests;
