use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque UUID identifying one durable workflow run.
///
/// ```
/// use toolkit_durable_execution::RunId;
/// let id: RunId = "67e55044-10b1-426f-9247-bb680e5fe0c8".parse()?;
/// assert_eq!(id.to_string(), "67e55044-10b1-426f-9247-bb680e5fe0c8");
/// # Ok::<(), uuid::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);
impl RunId {
    /// The underlying UUID.
    #[must_use]
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}
impl From<Uuid> for RunId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}
impl From<RunId> for Uuid {
    fn from(id: RunId) -> Self {
        id.0
    }
}
impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::str::FromStr for RunId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// Definition step name, or the encoded runtime instance key of a step.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivityId(pub String);
impl ActivityId {
    /// The identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for ActivityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Distinguishes normal execution from reversal of a confirmed effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ExecutionPhase {
    /// Normal business activity execution.
    Forward,
    /// Reversal of one confirmed forward effect.
    Compensation,
}
/// Stable logical activity identity, independent of lease owners and attempts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActivityInstanceId {
    /// ID of the activity template in the workflow contract.
    pub step: ActivityId,
    /// Present only for per-run list branches.
    pub item_index: Option<u32>,
    /// Forward and reverse effects have distinct identities.
    pub phase: ExecutionPhase,
}
impl ActivityInstanceId {
    /// Canonical storage and delivery key. Template IDs never contain '/'.
    #[must_use]
    pub fn key(&self) -> String {
        match (self.phase, self.item_index) {
            (ExecutionPhase::Forward, None) => self.step.0.clone(),
            (ExecutionPhase::Forward, Some(index)) => format!("f/{}/{index}", self.step.0),
            (ExecutionPhase::Compensation, index) => format!(
                "c/{}/{}",
                self.step.0,
                index.map_or_else(|| "single".into(), |i| i.to_string())
            ),
        }
    }
}

/// Stable identity only: never a bearer token or an authorization decision.
/// Access must be rechecked when performing domain work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOwner {
    /// Original submitting tenant.
    pub tenant_id: Uuid,
    /// Original submitting subject.
    pub subject_id: Uuid,
}

/// Admission options. Explicit idempotency deduplicates one logical request;
/// coalescing merges refreshes into active work. They are distinct mechanisms.
///
/// ```
/// use toolkit_durable_execution::StartOptions;
/// let options = StartOptions::new().idempotency_key("checkout:ord-1");
/// assert_eq!(options.idempotency_key.as_deref(), Some("checkout:ord-1"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StartOptions {
    /// Raw submission contract guard; the typed client always supplies it.
    #[serde(default)]
    pub expected_fingerprint: Option<String>,
    /// Caller key that makes a repeated start return the same run.
    pub idempotency_key: Option<String>,
    /// Owner-scoped key for merging refresh requests into active work.
    pub coalescing_key: Option<String>,
}
impl StartOptions {
    /// No idempotency, coalescing or fingerprint guard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Return the existing run when the same key is submitted again.
    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }
    /// Merge this request into active work started with the same key.
    #[must_use]
    pub fn coalescing_key(mut self, key: impl Into<String>) -> Self {
        self.coalescing_key = Some(key.into());
        self
    }
    /// Reject the start unless the registered contract has this fingerprint.
    #[must_use]
    pub fn expected_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.expected_fingerprint = Some(fingerprint.into());
        self
    }
}

/// Admission metadata returned by the object-safe service; typed callers receive
/// [`crate::StartedRun`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    /// Durable run identifier.
    pub run_id: RunId,
    /// Coalescing generation of this request. Requests merged into the same queued
    /// run or parked successor share it.
    pub requested_generation: u64,
    /// Whether admission merged with already admitted work.
    pub coalesced: bool,
}

/// Payload-free notification; re-read progress after receiving one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEvent {
    /// Structured instance identity; absent for whole-run events.
    pub instance: Option<ActivityInstanceId>,
    /// Monotone per-run committed event number.
    pub sequence: u64,
    /// Durable run identifier.
    pub run_id: RunId,
    /// Encoded instance ID; absent for whole-run events.
    pub activity_id: Option<ActivityId>,
    /// Transition kind.
    pub kind: EventKind,
    /// Execution epoch at the transition.
    pub epoch: u64,
    /// Lifetime invocation number, when the event belongs to an attempt.
    pub attempt: Option<u32>,
    /// Exact recorded transition time.
    pub at: DateTime<Utc>,
    /// Safe machine-readable error code.
    pub error_code: Option<String>,
}
/// Payload-free transition kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// The run entered reverse execution.
    CompensationStarted,
    /// Reversal stopped at an exhausted compensator; manual Retry may continue it.
    CompensationFailed,
    /// All declared reversals finished; forward continuation is prohibited.
    Compensated,
    /// Admitted work is waiting for an eligible claim.
    Queued,
    /// An invocation acquired a live persisted claim.
    Started,
    /// An activity or run committed a successful checkpoint.
    Completed,
    /// An automatic retry deadline was committed.
    RetryScheduled,
    /// Forward execution ended with a safe failure code.
    Failed,
    /// Forward cancellation was durably requested.
    StopRequested,
    /// Forward cleanup completed and the run is paused.
    Stopped,
    /// An explicit Resume advanced the forward execution epoch.
    Resumed,
    /// An explicit Retry advanced the failed phase epoch.
    RetryRequested,
    /// The expected definition is unavailable or incompatible.
    Blocked,
}

/// Epoch-checked commands make delayed or duplicated requests safe: a Retry for an
/// earlier epoch never restarts a newer failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ContinueOptions {
    /// Epoch observed before issuing the command; stale commands are rejected.
    pub expected_epoch: u64,
}
impl ContinueOptions {
    /// Continue the run observed at `expected_epoch`.
    #[must_use]
    pub fn new(expected_epoch: u64) -> Self {
        Self { expected_epoch }
    }
}

/// Epoch-checked, resumable pause of forward work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CancelOptions {
    /// Epoch observed before issuing the command; stale commands are rejected.
    pub expected_epoch: u64,
    /// Safe business reason; exclude credentials and personal data.
    pub reason: Option<String>,
}
impl CancelOptions {
    /// Pause the run observed at `expected_epoch`.
    #[must_use]
    pub fn new(expected_epoch: u64) -> Self {
        Self {
            expected_epoch,
            reason: None,
        }
    }
    /// Record a safe business reason.
    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Definitive stop of forward work; sagas reverse their confirmed effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AbortOptions {
    /// Epoch observed before issuing the command; stale commands are rejected.
    pub expected_epoch: u64,
    /// Safe business reason; exclude credentials and personal data.
    pub reason: Option<String>,
}
impl AbortOptions {
    /// Abort the run observed at `expected_epoch`.
    #[must_use]
    pub fn new(expected_epoch: u64) -> Self {
        Self {
            expected_epoch,
            reason: None,
        }
    }
    /// Record a safe business reason.
    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Whether a control command changed the durable run state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CancelResult {
    /// The command persisted its requested transition.
    Requested,
    /// The run was already in the requested state.
    Unchanged,
}
