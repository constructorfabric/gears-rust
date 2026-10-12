use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use chrono::{DateTime, Utc};
use github_mirror_sdk::{
    Branch, CheckRun, Comment, Commit, CommitComment, CommitFile, CommitStatus, Contributor,
    Deployment, Issue, IssueEvent, IssueReaction, IssueTimelineEvent, Label, Milestone,
    MirrorStatus, PullRequest, PullRequestCommit, PullRequestFile, Release, Repo, Review,
    ReviewComment, ReviewThread, SyncSummary, Tag, WorkflowJob, WorkflowRun,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use toolkit_macros::domain_model;
use toolkit_odata::{CursorV1, ODataQuery, Page, PageInfo, SortDir};
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::ports::github::{FetchOptions, GithubPort};
use super::repo::{
    ActiveSyncRecord, ActiveSyncRepository, BranchRecord, BranchRepository, CheckRunRecord,
    CheckRunRepository, CommentRecord, CommentRepository, CommitCommentRecord,
    CommitCommentRepository, CommitFileRecord, CommitFileRepository, CommitRecord,
    CommitRepository, CommitStatusRecord, CommitStatusRepository, ContributorRecord,
    ContributorRepository, DeploymentRecord, DeploymentRepository, EntityFingerprintRepository,
    IssueEventRecord, IssueEventRepository, IssueReactionRecord, IssueReactionRepository,
    IssueRecord, IssueRepository, IssueTimelineEventRecord, IssueTimelineRepository, LabelRecord,
    LabelRepository, ListingFilter, MilestoneRecord, MilestoneRepository, PageWindow,
    PullRequestCommitRecord, PullRequestCommitRepository, PullRequestFileRecord,
    PullRequestFileRepository, PullRequestRecord, PullRequestRepository, ReleaseRecord,
    ReleaseRepository, RepoRecord, RepoRepository, RepoRunStatus, RepoSyncStatusRecord,
    RepoSyncStatusRepository, ReviewCommentRecord, ReviewCommentRepository, ReviewRecord,
    ReviewRepository, ReviewThreadRecord, ReviewThreadRepository, SessionStatus, SyncSessionRecord,
    SyncSessionRepository, SyncWatermarkRepository, SyncWriter, TagRecord, TagRepository,
    WorkflowJobRecord, WorkflowJobRepository, WorkflowRunRecord, WorkflowRunRepository,
};
use super::scope::ScopeConfig;
use super::sync::{
    ChangeGate, Family, MirrorWorker, RepoPhaseRunner, RunState, SweepWatermark, TaskFailure,
    TaskKind, Worker,
};
use super::validate::{repo_full_name, validate_commit_sha, validate_owner, validate_repo_path};

/// The gear's name, taken from the `#[toolkit::gear]` attribute so the
/// literal exists in exactly one place.
pub const GEAR_NAME: &str = crate::gear::GithubMirrorGear::MODULE_NAME;

const DEFAULT_LIST_LIMIT: u64 = 50;
/// The most rows one keyset page may hold, the same bound `PageWindow` puts on
/// the offset-addressed listings.
const MAX_LIST_LIMIT: u64 = PageWindow::MAX_LIMIT;

/// The page size a list request asked for.
///
/// # Errors
/// `Validation` when the caller asks for more than [`MAX_LIST_LIMIT`] rows, or
/// for none at all, rather than quietly serving a different number.
fn list_limit(query: &ODataQuery) -> Result<u64, DomainError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if limit == 0 || limit > MAX_LIST_LIMIT {
        return Err(DomainError::Validation {
            field: "limit".to_owned(),
            message: format!("a page holds between 1 and {MAX_LIST_LIMIT} rows"),
        });
    }
    Ok(limit)
}
const SESSIONS_ORDER: &str = "-created_at,-id";
const REPO_STATUS_ORDER: &str = "+repository";

/// The current instant as RFC3339 text.
///
/// Session and run-status rows still store their timestamps as text (their
/// tables predate the typed columns), so they format here; `extracted_at` and
/// the contributor window are real timestamps and never pass through this.
fn cursor_keys<'a>(
    query: &'a ODataQuery,
    order: &str,
    key_count: usize,
) -> Result<Option<&'a [String]>, DomainError> {
    let Some(cursor) = query.cursor.as_ref() else {
        return Ok(None);
    };
    if cursor.s != order || cursor.k.len() != key_count || cursor.d != "fwd" {
        return Err(invalid_cursor());
    }
    Ok(Some(cursor.k.as_slice()))
}

fn invalid_cursor() -> DomainError {
    DomainError::Validation {
        field: "cursor".to_owned(),
        message: "the cursor does not belong to this listing".to_owned(),
    }
}

fn encode_cursor(
    order: &str,
    direction: SortDir,
    keys: Vec<String>,
) -> Result<String, DomainError> {
    CursorV1 {
        k: keys,
        o: direction,
        s: order.to_owned(),
        f: None,
        d: "fwd".to_owned(),
    }
    .encode()
    .map_err(|e| DomainError::Internal(format!("encoding a page cursor failed: {e}")))
}

fn keyset_page<T>(
    mut rows: Vec<T>,
    limit: u64,
    cursor_after: impl Fn(&T) -> Result<String, DomainError>,
) -> Result<Page<T>, DomainError> {
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let next_cursor = if rows.len() > page_len {
        rows.truncate(page_len);
        rows.last().map(cursor_after).transpose()?
    } else {
        None
    };
    Ok(Page::new(
        rows,
        PageInfo {
            next_cursor,
            prev_cursor: None,
            limit,
        },
    ))
}

struct EnqueueScopes {
    sync: AccessScope,
    session: AccessScope,
    repo_status: AccessScope,
}

/// The error a run that passed its deadline ends on, once it has been told to
/// stop and has wound down. Whatever the run itself reported on the way out is
/// logged rather than served: the deadline is the reason the caller needs.
fn past_deadline(
    job: &SyncJob,
    deadline: std::time::Duration,
    stopped_with: Option<DomainError>,
) -> DomainError {
    let minutes = deadline.as_secs().div_euclid(60);
    tracing::warn!(
        session_id = %job.session_id,
        repository = %format!("{}/{}", job.owner, job.name),
        deadline_minutes = minutes,
        stopped_with = ?stopped_with.map(|e| crate::redact::redacted(&e.to_string())),
        "sync passed its deadline and was stopped"
    );
    DomainError::internal(format!(
        "the sync of {}/{} ran past its deadline of {minutes} minutes and was stopped; the next \
         sync carries on from what it had already stored",
        job.owner, job.name
    ))
}

/// Whether the process that was running `session` is gone.
///
/// The session's own heartbeat is the evidence: a live run re-stamps
/// `updated_at` every couple of seconds, so a row that has not moved for
/// [`ABANDONED_AFTER_SECS`] has nobody behind it. A stamp that cannot be read
/// counts as live, so an unreadable row never costs another process its lock.
fn abandoned(session: &SyncSessionRecord, now: DateTime<Utc>) -> bool {
    let last_seen = session
        .updated_at
        .as_deref()
        .or(session.started_at.as_deref())
        .unwrap_or(&session.created_at);
    silent_too_long(last_seen, now)
}

fn silent_too_long(stamp: &str, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(stamp)
        .is_ok_and(|at| (now - at.with_timezone(&Utc)).num_seconds() > ABANDONED_AFTER_SECS)
}

fn report_session_closed_meanwhile(session_id: Uuid) {
    tracing::warn!(
        session_id = %session_id,
        "the session was closed while this run was still going; leaving it as it is"
    );
}

async fn heartbeat(
    sessions: Arc<dyn SyncSessionRepository>,
    scope: AccessScope,
    session_id: Uuid,
    percent: Arc<AtomicU8>,
    stop: CancellationToken,
) {
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS)) => {}
        }
        let progress_percent = i32::from(percent.load(Ordering::Relaxed));
        if let Err(e) = sessions
            .record_heartbeat(&scope, session_id, progress_percent, &now_rfc3339())
            .await
        {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "sync heartbeat could not persist progress"
            );
        }
    }
}

fn stored_summary_json(session_id: Uuid, summary: &SyncSummary) -> Option<String> {
    match serde_json::to_string(summary) {
        Ok(json) => Some(json),
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "sync summary could not be stored as JSON"
            );
            None
        }
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Release the per-repo sync lock, logging a failed release rather than
/// turning it into the sync's outcome: the guard's `Drop` has already queued
/// a best-effort release, and the sync succeeded or failed on its own merits.
async fn release_sync_lock(lock: toolkit_db::DbLockGuard, owner: &str, name: &str) {
    if let Err(e) = lock.release().await {
        tracing::warn!(owner, name, error = %e, "sync advisory lock release failed");
    }
}

/// Database provider handle the service uses for transactions and queries.
pub(crate) type DbProvider = toolkit_db::DBProvider<toolkit_db::DbError>;

/// Authorization resource type `github_mirror.repo` (the mirrored repo).
pub(crate) const REPO_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.repo",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.issue` (the mirrored issue).
pub(crate) const ISSUE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.issue",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.pull_request` (the mirrored pull request).
pub(crate) const PULL_REQUEST_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.pull_request",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.commit` (the mirrored commit).
pub(crate) const COMMIT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.commit",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.comment` (the mirrored comment).
pub(crate) const COMMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.comment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.review_comment` (the mirrored review comment).
pub(crate) const REVIEW_COMMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.review_comment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.review` (the mirrored review).
pub(crate) const REVIEW_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.review",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.label` (the mirrored label).
pub(crate) const LABEL_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.label",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.milestone` (the mirrored milestone).
pub(crate) const MILESTONE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.milestone",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.release` (the mirrored release).
pub(crate) const RELEASE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.release",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.branch` (the mirrored branch).
pub(crate) const BRANCH_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.branch",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.contributor` (the mirrored contributor).
pub(crate) const CONTRIBUTOR_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.contributor",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.workflow_run` (the mirrored workflow run).
pub(crate) const WORKFLOW_RUN_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.workflow_run",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.pull_request_file` (the mirrored pull request file).
pub(crate) const PULL_REQUEST_FILE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.pull_request_file",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.tag` (the mirrored tag).
pub(crate) const TAG_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.tag",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.commit_file` (the mirrored commit file).
pub(crate) const COMMIT_FILE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.commit_file",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.review_thread` (the mirrored review thread).
pub(crate) const REVIEW_THREAD_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.review_thread",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.commit_comment` (the mirrored commit comment).
pub(crate) const COMMIT_COMMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.commit_comment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.issue_event` (the mirrored issue event).
pub(crate) const ISSUE_EVENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.issue_event",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.deployment` (the mirrored deployment).
pub(crate) const DEPLOYMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.deployment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.pull_request_commit` (the mirrored pull request commit).
pub(crate) const PULL_REQUEST_COMMIT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.pull_request_commit",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.commit_status` (the mirrored commit status).
pub(crate) const COMMIT_STATUS_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.commit_status",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.workflow_job` (the mirrored workflow job).
pub(crate) const WORKFLOW_JOB_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.workflow_job",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.issue_reaction` (the mirrored issue reaction).
pub(crate) const ISSUE_REACTION_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.issue_reaction",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.check_run` (the mirrored check run).
pub(crate) const CHECK_RUN_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.check_run",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.issue_timeline` (the mirrored issue timeline).
pub(crate) const ISSUE_TIMELINE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.issue_timeline",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Progress once every mirrored table has been written.
const PROGRESS_STORED: u8 = 95;
/// How often the run persists its progress while it is working.
const HEARTBEAT_SECS: u64 = 2;

/// How long a session must go without a heartbeat before start-up treats its
/// holder as gone.
///
/// A running sync re-stamps `updated_at` every [`HEARTBEAT_SECS`], so a row
/// this far behind belongs to a process that is no longer writing. The margin
/// is wide enough for a run stalled on a slow write or a rate-limit wait to
/// keep its lock.
const ABANDONED_AFTER_SECS: i64 = 300;

/// How often this process refreshes its `gm_active_syncs` rows, so another
/// process's start-up sweep sees them as alive.
pub(crate) const ACTIVE_SYNC_TOUCH_EVERY: std::time::Duration = std::time::Duration::from_mins(1);

const TAKE_ACTIVE_SYNC_ATTEMPTS: usize = 3;

const QUEUE_FULL_RETRY_AFTER_SECS: u64 = 30;

const MAX_REPORTED_FAILURES: usize = 20;

const MAX_REPORTED_DRIFT: usize = 100;

const SESSION_WRITE_ATTEMPTS: u32 = 3;

const SESSION_WRITE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// How long after start-up the interrupted-session sweep runs a second time,
/// once a process that died just before this one has gone stale.
pub(crate) const SWEEP_AGAIN_AFTER: std::time::Duration =
    std::time::Duration::from_secs(ABANDONED_AFTER_SECS.unsigned_abs() + HEARTBEAT_SECS);

/// Most repositories one resume call will re-queue: what the sync channel
/// holds, so a call never queues more than fits; the rest stay `in_progress`
/// for the next call.
const RESUME_LIMIT: usize = SYNC_QUEUE_DEPTH;

/// Phase progress of one sync, published through a shared atomic so the
/// heartbeat can read it without touching the running fetch (DESIGN §4
/// "Progress"). Values are clamped monotonically non-decreasing.
///
/// While the phases run, `RepoPhaseRunner` publishes the phase-weighted
/// estimate into this atomic after every task it finishes; the milestones
/// below only cover what happens after the runner returns.
#[domain_model]
#[derive(Debug)]
pub struct SyncProgress {
    percent: Arc<AtomicU8>,
}

impl Default for SyncProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncProgress {
    #[must_use]
    pub fn new() -> Self {
        Self {
            percent: Arc::new(AtomicU8::new(0)),
        }
    }

    /// A second handle on the same counter, for the heartbeat to read.
    #[must_use]
    pub fn handle(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.percent)
    }

    #[must_use]
    pub fn percent(&self) -> u8 {
        self.percent.load(Ordering::Relaxed)
    }

    fn raise_to(&self, value: u8) {
        self.percent.fetch_max(value, Ordering::Relaxed);
    }

    /// Every mirrored table has been written.
    pub(crate) fn stored(&self) {
        self.raise_to(PROGRESS_STORED);
    }

    /// The run is over, whatever its outcome.
    pub(crate) fn finished(&self) {
        self.raise_to(100);
    }
}

/// How many enqueued syncs may wait for the worker before `POST /sync` starts
/// rejecting. One repository at a time is the current worker concurrency, so
/// this is the depth of the backlog, not of the parallelism.
///
/// The same number twice over: the channel holds this many jobs, and the pool
/// parks this many more once every worker is busy, so a caller first sees the
/// queue-full error at roughly twice this many outstanding syncs. One constant
/// so the two halves cannot drift apart.
pub(crate) const SYNC_QUEUE_DEPTH: usize = 64;

/// What a sync request got: the session to poll, and what that session is
/// doing right now.
///
/// The status is read rather than assumed, because a request that collapsed
/// into a run already going is handed that run's session, which may have left
/// `queued` some time ago.
#[domain_model]
#[derive(Debug, Default)]
pub struct ResumeOutcome {
    pub session_ids: Vec<Uuid>,
    pub refused: Vec<RefusedResume>,
}

#[domain_model]
#[derive(Debug)]
pub struct RefusedResume {
    pub repository: String,
    pub error: DomainError,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedSync {
    pub session_id: Uuid,
    pub status: SessionStatus,
}

/// One unit of background work: sync `owner/name` on behalf of `ctx`, and
/// record the outcome against the session row created at enqueue time.
///
/// The caller's [`SecurityContext`] travels with the job because the work
/// outlives the request that asked for it, and every write it makes is still
/// tenant-scoped through the same policy enforcer.
#[domain_model]
#[derive(Debug)]
pub struct SyncJob {
    pub session_id: Uuid,
    pub ctx: SecurityContext,
    pub owner: String,
    pub name: String,
    /// What this run collects. Resolved at enqueue time from the request, or
    /// from the gear config when the request says nothing.
    pub scope: ScopeConfig,
    /// PRD §5.2 force mode: the GitHub client skips its stored `ETag`s, the
    /// sweep ignores its watermark and the change gate re-fetches every
    /// entity, so the whole repository is read again from GitHub.
    pub force: bool,
    /// Oldest closed entity worth collecting, from the request.
    pub since: Option<DateTime<Utc>>,
    pub access_scope: AccessScope,
    #[expect(
        dead_code,
        reason = "held for its drop: a job that goes away, run or not, gives its claim back"
    )]
    /// The repository's active-sync claim, released when the job is dropped.
    pub(crate) claim: Option<ClaimRelease>,
}

/// Authorization resource type `github_mirror.repo_sync_status` (the mirrored repo sync status).
pub(crate) const REPO_SYNC_STATUS_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.repo_sync_status",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.sync_session` (the mirrored sync session).
pub(crate) const SYNC_SESSION_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.sync_session",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

/// Authorization resource type `github_mirror.sync` (the mirrored sync).
pub(crate) const SYNC_RESOURCE: ResourceType =
    ResourceType::from_static("github_mirror.sync", &[pep_properties::OWNER_TENANT_ID]);

/// Action names used when authorizing operations against the mirror's resource types.
pub(crate) mod actions {
    /// Action for listing mirrored resources.
    pub const LIST: &str = "list";
    /// Action for upserting mirrored resources (used by the sync path).
    pub const UPSERT: &str = "upsert";
    /// Action for triggering a repository sync.
    pub const SYNC: &str = "sync";
    /// Action for reading a single mirrored resource.
    pub const GET: &str = "get";
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub api_base_url: String,
    /// What a sync collects when the request does not say.
    pub scope: ScopeConfig,
    /// How many repositories may sync at the same time.
    pub max_concurrent_syncs: NonZeroUsize,
    /// How many tasks one repository's sync runs at the same time.
    pub max_concurrent_tasks: NonZeroUsize,
    /// How long one repository's sync may run before it is stopped.
    pub sync_deadline: std::time::Duration,
}

#[domain_model]
pub struct Service {
    db: Arc<DbProvider>,
    repo: Arc<dyn RepoRepository>,
    issues: Arc<dyn IssueRepository>,
    pull_requests: Arc<dyn PullRequestRepository>,
    commits: Arc<dyn CommitRepository>,
    comments: Arc<dyn CommentRepository>,
    review_comments: Arc<dyn ReviewCommentRepository>,
    reviews: Arc<dyn ReviewRepository>,
    labels: Arc<dyn LabelRepository>,
    milestones: Arc<dyn MilestoneRepository>,
    releases: Arc<dyn ReleaseRepository>,
    branches: Arc<dyn BranchRepository>,
    contributors: Arc<dyn ContributorRepository>,
    workflow_runs: Arc<dyn WorkflowRunRepository>,
    pull_request_files: Arc<dyn PullRequestFileRepository>,
    tags: Arc<dyn TagRepository>,
    commit_files: Arc<dyn CommitFileRepository>,
    review_threads: Arc<dyn ReviewThreadRepository>,
    commit_comments: Arc<dyn CommitCommentRepository>,
    issue_events: Arc<dyn IssueEventRepository>,
    deployments: Arc<dyn DeploymentRepository>,
    pull_request_commits: Arc<dyn PullRequestCommitRepository>,
    commit_statuses: Arc<dyn CommitStatusRepository>,
    workflow_jobs: Arc<dyn WorkflowJobRepository>,
    issue_reactions: Arc<dyn IssueReactionRepository>,
    check_runs: Arc<dyn CheckRunRepository>,
    issue_timeline: Arc<dyn IssueTimelineRepository>,
    sync_sessions: Arc<dyn SyncSessionRepository>,
    repo_sync_status: Arc<dyn RepoSyncStatusRepository>,
    sync_writer: Arc<dyn SyncWriter>,
    change_gate: Arc<ChangeGate>,
    sweep_watermark: Arc<SweepWatermark>,
    github: Arc<dyn GithubPort>,
    policy_enforcer: PolicyEnforcer,
    config: ServiceConfig,
    sync_tx: mpsc::Sender<SyncJob>,
    sync_rx: Arc<Mutex<Option<mpsc::Receiver<SyncJob>>>>,
    active_syncs: Arc<dyn ActiveSyncRepository>,
    instance_id: Uuid,
    in_process_syncs: TaskTracker,
    in_process_slots: Arc<tokio::sync::Semaphore>,
    /// One gate per repository, so the look, the write and the claim that a
    /// queue request makes are serialised for that repository alone rather than
    /// for the whole gear.
    ///
    /// A gate lives while a request holds it: the last request to let go
    /// removes it, so the map holds only repositories a request is deciding
    /// about right now.
    claim_gates: gate::ClaimGates,
    /// The gear's shutdown token, bound when the sync pool starts. Syncs that
    /// do not come from the pool — the in-process client's — carry it too, so
    /// a shutdown reaches them as well.
    shutdown: Arc<OnceLock<CancellationToken>>,
}

/// One repository of one tenant: what a queued or running sync occupies.
type InFlightKey = (Uuid, String);

enum PreparedSync {
    Joined(QueuedSync),
    Claimed {
        job: Box<SyncJob>,
        session: Box<SyncSessionRecord>,
    },
}

/// A repository's `gm_active_syncs` claim, deleted in the background when
/// this guard is dropped.
#[domain_model]
pub(crate) struct ClaimRelease {
    active_syncs: Arc<dyn ActiveSyncRepository>,
    scope: AccessScope,
    repo_full_name: String,
    session_id: Uuid,
}

impl std::fmt::Debug for ClaimRelease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimRelease")
            .field("repo_full_name", &self.repo_full_name)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl Drop for ClaimRelease {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let active_syncs = Arc::clone(&self.active_syncs);
        let scope = self.scope.clone();
        let repo_full_name = std::mem::take(&mut self.repo_full_name);
        let session_id = self.session_id;
        runtime.spawn(async move {
            if let Err(e) = active_syncs
                .delete(&scope, &repo_full_name, session_id)
                .await
            {
                tracing::warn!(
                    repository = %repo_full_name,
                    session_id = %session_id,
                    error = %e,
                    "could not remove the repository's active sync row"
                );
            }
        });
    }
}

mod gate {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio::sync::{Mutex, MutexGuard};
    use toolkit_macros::domain_model;

    use super::InFlightKey;

    type Gates = Arc<std::sync::Mutex<HashMap<InFlightKey, Arc<Mutex<()>>>>>;

    #[domain_model]
    #[derive(Clone, Default)]
    pub(super) struct ClaimGates(Gates);

    impl ClaimGates {
        /// This repository's gate, made on first use.
        pub(super) fn lease(&self, key: &InFlightKey) -> GateLease {
            let mut gates = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let gate = Arc::clone(
                gates
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            );
            GateLease {
                gates: Arc::clone(&self.0),
                key: key.clone(),
                gate,
            }
        }
    }

    /// What a request holds while it decides whether one repository already has a
    /// sync in flight.
    #[domain_model]
    pub(super) struct GateLease {
        gates: Gates,
        key: InFlightKey,
        gate: Arc<Mutex<()>>,
    }

    impl GateLease {
        pub(super) async fn lock(&self) -> MutexGuard<'_, ()> {
            self.gate.lock().await
        }
    }

    impl Drop for GateLease {
        fn drop(&mut self) {
            let mut gates = self
                .gates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if Arc::strong_count(&self.gate) == 2 {
                gates.remove(&self.key);
            }
        }
    }
}

/// The sync holding a repository, and what it was asked to collect.
///
/// The terms travel with the claim because a second request that wants
/// something else must not be handed this one's session: it would be told a
/// sync ran for terms it never asked for. `force` is deliberately not part of
/// them — a forced request and a plain one collect the same things, and the
/// run in flight is already fetching the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Claim {
    session_id: Uuid,
    scope: ScopeConfig,
    since: Option<DateTime<Utc>>,
}

/// Manual `Clone`: every field is an `Arc` (cheap refcount bump) or already
/// `Clone` (`PolicyEnforcer`, `ServiceConfig`). A `#[derive(Clone)]` would add
/// a spurious `T: Clone` bound to each of the 26 repository generics even
/// though `Arc<T>: Clone` never needs one, so it is written out by hand.
///
/// Exists so a caller can obtain an owned handle to hand into a `'static`
/// closure (e.g. a DB transaction) without borrowing `&self` across it.
impl Clone for Service {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            repo: Arc::clone(&self.repo),
            issues: Arc::clone(&self.issues),
            pull_requests: Arc::clone(&self.pull_requests),
            commits: Arc::clone(&self.commits),
            comments: Arc::clone(&self.comments),
            review_comments: Arc::clone(&self.review_comments),
            reviews: Arc::clone(&self.reviews),
            labels: Arc::clone(&self.labels),
            milestones: Arc::clone(&self.milestones),
            releases: Arc::clone(&self.releases),
            branches: Arc::clone(&self.branches),
            contributors: Arc::clone(&self.contributors),
            workflow_runs: Arc::clone(&self.workflow_runs),
            pull_request_files: Arc::clone(&self.pull_request_files),
            tags: Arc::clone(&self.tags),
            commit_files: Arc::clone(&self.commit_files),
            review_threads: Arc::clone(&self.review_threads),
            commit_comments: Arc::clone(&self.commit_comments),
            issue_events: Arc::clone(&self.issue_events),
            deployments: Arc::clone(&self.deployments),
            pull_request_commits: Arc::clone(&self.pull_request_commits),
            commit_statuses: Arc::clone(&self.commit_statuses),
            workflow_jobs: Arc::clone(&self.workflow_jobs),
            issue_reactions: Arc::clone(&self.issue_reactions),
            check_runs: Arc::clone(&self.check_runs),
            issue_timeline: Arc::clone(&self.issue_timeline),
            sync_sessions: Arc::clone(&self.sync_sessions),
            repo_sync_status: Arc::clone(&self.repo_sync_status),
            sync_writer: Arc::clone(&self.sync_writer),
            change_gate: Arc::clone(&self.change_gate),
            sweep_watermark: Arc::clone(&self.sweep_watermark),
            github: Arc::clone(&self.github),
            policy_enforcer: self.policy_enforcer.clone(),
            config: self.config.clone(),
            sync_tx: self.sync_tx.clone(),
            sync_rx: Arc::clone(&self.sync_rx),
            active_syncs: Arc::clone(&self.active_syncs),
            instance_id: self.instance_id,
            in_process_syncs: self.in_process_syncs.clone(),
            in_process_slots: Arc::clone(&self.in_process_slots),
            claim_gates: self.claim_gates.clone(),
            shutdown: Arc::clone(&self.shutdown),
        }
    }
}

impl Service {
    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per repository the service owns; the gear wires them \
                  in one place and a holder struct would only move the same list \
                  there"
    )]
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<dyn RepoRepository>,
        issues: Arc<dyn IssueRepository>,
        pull_requests: Arc<dyn PullRequestRepository>,
        commits: Arc<dyn CommitRepository>,
        comments: Arc<dyn CommentRepository>,
        review_comments: Arc<dyn ReviewCommentRepository>,
        reviews: Arc<dyn ReviewRepository>,
        labels: Arc<dyn LabelRepository>,
        milestones: Arc<dyn MilestoneRepository>,
        releases: Arc<dyn ReleaseRepository>,
        branches: Arc<dyn BranchRepository>,
        contributors: Arc<dyn ContributorRepository>,
        workflow_runs: Arc<dyn WorkflowRunRepository>,
        pull_request_files: Arc<dyn PullRequestFileRepository>,
        tags: Arc<dyn TagRepository>,
        commit_files: Arc<dyn CommitFileRepository>,
        review_threads: Arc<dyn ReviewThreadRepository>,
        commit_comments: Arc<dyn CommitCommentRepository>,
        issue_events: Arc<dyn IssueEventRepository>,
        deployments: Arc<dyn DeploymentRepository>,
        pull_request_commits: Arc<dyn PullRequestCommitRepository>,
        commit_statuses: Arc<dyn CommitStatusRepository>,
        workflow_jobs: Arc<dyn WorkflowJobRepository>,
        issue_reactions: Arc<dyn IssueReactionRepository>,
        check_runs: Arc<dyn CheckRunRepository>,
        issue_timeline: Arc<dyn IssueTimelineRepository>,
        sync_sessions: Arc<dyn SyncSessionRepository>,
        repo_sync_status: Arc<dyn RepoSyncStatusRepository>,
        active_syncs: Arc<dyn ActiveSyncRepository>,
        sync_writer: Arc<dyn SyncWriter>,
        fingerprints: Arc<dyn EntityFingerprintRepository>,
        watermark_store: Arc<dyn SyncWatermarkRepository>,
        github: Arc<dyn GithubPort>,
        policy_enforcer: PolicyEnforcer,
        config: ServiceConfig,
    ) -> Self {
        let (sync_tx, sync_rx) = mpsc::channel(SYNC_QUEUE_DEPTH);
        let in_process_slots = Arc::new(tokio::sync::Semaphore::new(
            config.max_concurrent_syncs.get(),
        ));
        Self {
            db,
            repo,
            issues,
            pull_requests,
            commits,
            comments,
            review_comments,
            reviews,
            labels,
            milestones,
            releases,
            branches,
            contributors,
            workflow_runs,
            pull_request_files,
            tags,
            commit_files,
            review_threads,
            commit_comments,
            issue_events,
            deployments,
            pull_request_commits,
            commit_statuses,
            workflow_jobs,
            issue_reactions,
            check_runs,
            issue_timeline,
            sync_sessions,
            repo_sync_status,
            sync_writer,
            change_gate: Arc::new(ChangeGate::new(fingerprints)),
            sweep_watermark: Arc::new(SweepWatermark::new(watermark_store)),
            github,
            policy_enforcer,
            config,
            sync_tx,
            sync_rx: Arc::new(Mutex::new(Some(sync_rx))),
            active_syncs,
            instance_id: Uuid::new_v4(),
            in_process_syncs: TaskTracker::new(),
            in_process_slots,
            claim_gates: gate::ClaimGates::default(),
            shutdown: Arc::new(OnceLock::new()),
        }
    }

    /// Fetch one mirrored repository (`owner/name`), tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn get_repo(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
    ) -> Result<Repo, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        self.repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)
    }

    /// Fetch one mirrored issue by number, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository or issue is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn get_issue(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<Issue, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        self.issues
            .find_by_number(&scope, repository.id, number)
            .await?
            .ok_or(DomainError::NotFound)
    }

    /// Fetch one mirrored pull request by number, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository or pull request is not
    /// mirrored; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn get_pull_request(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<PullRequest, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        self.pull_requests
            .find_by_number(&scope, repository.id, number)
            .await?
            .ok_or(DomainError::NotFound)
    }

    /// Fetch one mirrored commit by SHA, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository or commit is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn get_commit(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        sha: &str,
    ) -> Result<Commit, DomainError> {
        validate_commit_sha(sha)?;
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        self.commits
            .find_by_sha(&scope, repository.id, sha)
            .await?
            .ok_or(DomainError::NotFound)
    }

    #[must_use]
    pub fn status(&self) -> MirrorStatus {
        MirrorStatus {
            gear: GEAR_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            api_base_url: self.config.api_base_url.clone(),
        }
    }

    /// List mirrored repositories visible to the caller's tenant.
    ///
    /// # Errors
    /// Returns `DomainError::Forbidden` when the PDP denies access and
    /// `DomainError::Database`/`Internal` on storage failures.
    pub async fn list_repos(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Repo>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        self.repo.list(&scope, query).await
    }

    /// One numbered page of mirrored repositories, for the
    /// GitHub-compatible `GET /user/repos`.
    ///
    /// # Errors
    /// `Forbidden` on PDP denial, `Database`/`Internal` on storage failures.
    pub async fn list_repos_page(
        &self,
        ctx: &SecurityContext,
        window: PageWindow,
    ) -> Result<Vec<Repo>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        self.repo.list_window(&scope, window).await
    }

    /// Insert or update a mirrored repository row for the caller's tenant.
    ///
    /// # Errors
    /// Returns `DomainError::Forbidden` when the PDP denies access and
    /// `DomainError::Database`/`Internal` on storage failures.
    pub async fn upsert_repo(
        &self,
        ctx: &SecurityContext,
        record: RepoRecord,
    ) -> Result<Repo, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        self.repo.upsert(&scope, tenant_id, record).await
    }

    /// The repository's issues, filtered as GitHub's list endpoint filters.
    ///
    /// `filter`: GitHub's list-endpoint filters (state, sort, direction,
    /// since). The handler applies GitHub's own defaults.
    ///
    /// # Errors
    /// `NotFound` when the repository is not mirrored; `Forbidden`/`Database`
    /// as usual.
    pub async fn list_issues(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
        filter: ListingFilter,
    ) -> Result<(Page<Issue>, u64), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let (items, total) = self
            .issues
            .page_by_repo(&scope, repository.id, window, filter)
            .await?;

        // Counted on the scope and repository already resolved above: the
        // GitHub-compatible listings report a total, and doing it here saves
        // a second policy evaluation and repository lookup per request.

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored issue row for the caller's tenant.
    ///
    /// The owning repository must already be mirrored (`DomainError::NotFound`
    /// otherwise) so issues can never dangle.
    ///
    /// # Errors
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_issue(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: IssueRecord,
    ) -> Result<Issue, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = IssueRecord {
            repo_id: repository.id,
            ..record
        };
        self.issues.upsert(&scope, tenant_id, record).await
    }

    /// The repository's pull requests, filtered as GitHub's list endpoint
    /// filters.
    ///
    /// `filter`: GitHub's list-endpoint filters (state, sort, direction,
    /// since). The handler applies GitHub's own defaults.
    ///
    /// # Errors
    /// `NotFound` when the repository is not mirrored; `Forbidden`/`Database`
    /// as usual.
    pub async fn list_pull_requests(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
        filter: ListingFilter,
    ) -> Result<(Page<PullRequest>, u64), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let (items, total) = self
            .pull_requests
            .page_by_repo(&scope, repository.id, window, filter)
            .await?;

        // Counted on the scope and repository already resolved above: the
        // GitHub-compatible listings report a total, and doing it here saves
        // a second policy evaluation and repository lookup per request.

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored pull-request row for the caller's tenant.
    ///
    /// The owning repository must already be mirrored (`DomainError::NotFound`
    /// otherwise).
    ///
    /// # Errors
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_pull_request(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: PullRequestRecord,
    ) -> Result<PullRequest, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = PullRequestRecord {
            repo_id: repository.id,
            ..record
        };
        self.pull_requests.upsert(&scope, tenant_id, record).await
    }

    /// The repository's mirrored commits, newest first.
    ///
    /// # Errors
    /// `NotFound` when the repository is not mirrored; `Forbidden`/`Database`
    /// as usual.
    pub async fn list_commits(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
        since: Option<DateTime<Utc>>,
    ) -> Result<(Page<Commit>, u64), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let (items, total) = self
            .commits
            .page_by_repo(&scope, repository.id, window, since)
            .await?;

        // Counted on the scope and repository already resolved above: the
        // GitHub-compatible listings report a total, and doing it here saves
        // a second policy evaluation and repository lookup per request.

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored commit row for the caller's tenant.
    ///
    /// The owning repository must already be mirrored (`DomainError::NotFound`
    /// otherwise).
    ///
    /// # Errors
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_commit(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CommitRecord,
    ) -> Result<Commit, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommitRecord {
            repo_id: repository.id,
            ..record
        };
        self.commits.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored comments of one issue/PR (`owner/name` + number),
    /// tenant-scoped, oldest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_comments(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        issue_number: i64,
        window: PageWindow,
    ) -> Result<Page<Comment>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMENT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .comments
            .list_by_issue(&scope, repository.id, issue_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored comment row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_comment(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CommentRecord,
    ) -> Result<Comment, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMENT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommentRecord {
            repo_id: repository.id,
            ..record
        };
        self.comments.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored review comments of one pull request, tenant-scoped,
    /// oldest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_review_comments(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        pull_number: i64,
        window: PageWindow,
    ) -> Result<Page<ReviewComment>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_COMMENT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .review_comments
            .list_by_pull(&scope, repository.id, pull_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored review-comment row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_review_comment(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: ReviewCommentRecord,
    ) -> Result<ReviewComment, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_COMMENT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReviewCommentRecord {
            repo_id: repository.id,
            ..record
        };
        self.review_comments.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored reviews of one pull request, tenant-scoped, oldest
    /// first (by review id).
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_reviews(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        pull_number: i64,
        window: PageWindow,
    ) -> Result<Page<Review>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .reviews
            .list_by_pull(&scope, repository.id, pull_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored review row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_review(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: ReviewRecord,
    ) -> Result<Review, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReviewRecord {
            repo_id: repository.id,
            ..record
        };
        self.reviews.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored labels of one repository (`owner/name`), tenant-scoped,
    /// by name.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_labels(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Label>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &LABEL_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .labels
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored label row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_label(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: LabelRecord,
    ) -> Result<Label, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &LABEL_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = LabelRecord {
            repo_id: repository.id,
            ..record
        };
        self.labels.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored milestones of one repository (`owner/name`),
    /// tenant-scoped, by milestone number.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_milestones(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Milestone>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &MILESTONE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .milestones
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored milestone row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_milestone(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: MilestoneRecord,
    ) -> Result<Milestone, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &MILESTONE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = MilestoneRecord {
            repo_id: repository.id,
            ..record
        };
        self.milestones.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored releases of one repository (`owner/name`),
    /// tenant-scoped, newest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_releases(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Release>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &RELEASE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .releases
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored release row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_release(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: ReleaseRecord,
    ) -> Result<Release, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &RELEASE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReleaseRecord {
            repo_id: repository.id,
            ..record
        };
        self.releases.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored branch heads of one repository (`owner/name`),
    /// tenant-scoped, by name.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_branches(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Branch>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &BRANCH_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .branches
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored branch-head row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_branch(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: BranchRecord,
    ) -> Result<Branch, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &BRANCH_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = BranchRecord {
            repo_id: repository.id,
            ..record
        };
        self.branches.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored contributors of one repository (`owner/name`),
    /// tenant-scoped, most contributions first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_contributors(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Contributor>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &CONTRIBUTOR_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .contributors
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored contributor row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_contributor(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: ContributorRecord,
    ) -> Result<Contributor, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &CONTRIBUTOR_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ContributorRecord {
            repo_id: repository.id,
            ..record
        };
        self.contributors.upsert(&scope, tenant_id, record).await
    }

    /// # Errors
    /// `NotFound` when the repository is not mirrored; `Forbidden`/`Database`
    /// as usual.
    pub async fn list_workflow_runs(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<(Page<WorkflowRun>, u64), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &WORKFLOW_RUN_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        // Both statements run on one transaction, and on the scope and
        // repository already resolved above: the total describes the page it
        // is returned with, and one policy evaluation covers both.
        let (items, total) = self
            .workflow_runs
            .page_by_repo(&scope, repository.id, window)
            .await?;

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored workflow-run row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_workflow_run(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: WorkflowRunRecord,
    ) -> Result<WorkflowRun, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &WORKFLOW_RUN_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = WorkflowRunRecord {
            repo_id: repository.id,
            ..record
        };
        self.workflow_runs.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored changed files of one pull request, tenant-scoped, by
    /// file name.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_pull_request_files(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        pull_number: i64,
        window: PageWindow,
    ) -> Result<Page<PullRequestFile>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_FILE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .pull_request_files
            .list_by_pull(&scope, repository.id, pull_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored pull-request-file row for the caller's
    /// tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_pull_request_file(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: PullRequestFileRecord,
    ) -> Result<PullRequestFile, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_FILE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = PullRequestFileRecord {
            repo_id: repository.id,
            ..record
        };
        self.pull_request_files
            .upsert(&scope, tenant_id, record)
            .await
    }

    /// List mirrored tags of one repository (`owner/name`), tenant-scoped,
    /// by name.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_tags(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Tag>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &TAG_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .tags
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored tag row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_tag(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: TagRecord,
    ) -> Result<Tag, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &TAG_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = TagRecord {
            repo_id: repository.id,
            ..record
        };
        self.tags.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored changed files of one commit, tenant-scoped, by file
    /// name.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_commit_files(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        commit_sha: &str,
        query: &ODataQuery,
    ) -> Result<Page<CommitFile>, DomainError> {
        validate_commit_sha(commit_sha)?;
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_FILE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        self.commit_files
            .list_by_commit(&scope, repository.id, commit_sha, query)
            .await
    }

    /// Insert or update a mirrored commit-file row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_commit_file(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CommitFileRecord,
    ) -> Result<CommitFile, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_FILE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommitFileRecord {
            repo_id: repository.id,
            ..record
        };
        self.commit_files.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored review threads of one pull request, tenant-scoped, by
    /// thread id.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_review_threads(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        pull_number: i64,
        query: &ODataQuery,
    ) -> Result<Page<ReviewThread>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_THREAD_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        self.review_threads
            .list_by_pull(&scope, repository.id, pull_number, query)
            .await
    }

    /// Insert or update a mirrored review-thread row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_review_thread(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: ReviewThreadRecord,
    ) -> Result<ReviewThread, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_THREAD_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReviewThreadRecord {
            repo_id: repository.id,
            ..record
        };
        self.review_threads.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored comments of one commit, tenant-scoped, oldest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_commit_comments(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        commit_sha: &str,
        window: PageWindow,
    ) -> Result<Page<CommitComment>, DomainError> {
        validate_commit_sha(commit_sha)?;
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_COMMENT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .commit_comments
            .list_by_commit(&scope, repository.id, commit_sha, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored commit-comment row for the caller's
    /// tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_commit_comment(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CommitCommentRecord,
    ) -> Result<CommitComment, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_COMMENT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommitCommentRecord {
            repo_id: repository.id,
            ..record
        };
        self.commit_comments.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored events of one issue, tenant-scoped, oldest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_issue_events(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        issue_number: i64,
        window: PageWindow,
    ) -> Result<Page<IssueEvent>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_EVENT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .issue_events
            .list_by_issue(&scope, repository.id, issue_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored issue-event row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_issue_event(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: IssueEventRecord,
    ) -> Result<IssueEvent, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_EVENT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = IssueEventRecord {
            repo_id: repository.id,
            ..record
        };
        self.issue_events.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored deployments of one repository (`owner/name`),
    /// tenant-scoped, newest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_deployments(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        window: PageWindow,
    ) -> Result<Page<Deployment>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &DEPLOYMENT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .deployments
            .list_by_repo(&scope, repository.id, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored deployment row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_deployment(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: DeploymentRecord,
    ) -> Result<Deployment, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &DEPLOYMENT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = DeploymentRecord {
            repo_id: repository.id,
            ..record
        };
        self.deployments.upsert(&scope, tenant_id, record).await
    }

    /// List the mirrored commits of one pull request, tenant-scoped,
    /// oldest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_pull_request_commits(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        pull_number: i64,
        window: PageWindow,
    ) -> Result<Page<PullRequestCommit>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_COMMIT_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .pull_request_commits
            .list_by_pull(&scope, repository.id, pull_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored pull-request-commit row for the caller's
    /// tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_pull_request_commit(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: PullRequestCommitRecord,
    ) -> Result<PullRequestCommit, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &PULL_REQUEST_COMMIT_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = PullRequestCommitRecord {
            repo_id: repository.id,
            ..record
        };
        self.pull_request_commits
            .upsert(&scope, tenant_id, record)
            .await
    }

    /// List mirrored statuses of one commit, tenant-scoped, newest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_commit_statuses(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        commit_sha: &str,
        window: PageWindow,
    ) -> Result<Page<CommitStatus>, DomainError> {
        validate_commit_sha(commit_sha)?;
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_STATUS_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .commit_statuses
            .list_by_commit(&scope, repository.id, commit_sha, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored commit-status row for the caller's
    /// tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_commit_status(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CommitStatusRecord,
    ) -> Result<CommitStatus, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &COMMIT_STATUS_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommitStatusRecord {
            repo_id: repository.id,
            ..record
        };
        self.commit_statuses.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored jobs of one workflow run, tenant-scoped, by job id.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_workflow_jobs(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        run_id: i64,
        window: PageWindow,
    ) -> Result<(Page<WorkflowJob>, u64), DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &WORKFLOW_JOB_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        // Both statements run on one transaction, and on the scope and
        // repository already resolved above.
        let (items, total) = self
            .workflow_jobs
            .page_by_run(&scope, repository.id, run_id, window)
            .await?;

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored workflow-job row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_workflow_job(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: WorkflowJobRecord,
    ) -> Result<WorkflowJob, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &WORKFLOW_JOB_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = WorkflowJobRecord {
            repo_id: repository.id,
            ..record
        };
        self.workflow_jobs.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored reactions of one issue or pull request, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_issue_reactions(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        issue_number: i64,
        window: PageWindow,
    ) -> Result<Page<IssueReaction>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_REACTION_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .issue_reactions
            .list_by_issue(&scope, repository.id, issue_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update a mirrored issue-reaction row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_issue_reaction(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: IssueReactionRecord,
    ) -> Result<IssueReaction, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_REACTION_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = IssueReactionRecord {
            repo_id: repository.id,
            ..record
        };
        self.issue_reactions.upsert(&scope, tenant_id, record).await
    }

    /// List mirrored check runs of one commit, tenant-scoped, by check-run id.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_check_runs(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        head_sha: &str,
        window: PageWindow,
    ) -> Result<(Page<CheckRun>, u64), DomainError> {
        validate_commit_sha(head_sha)?;
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &CHECK_RUN_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        // Both statements run on one transaction, and on the scope and
        // repository already resolved above.
        let (items, total) = self
            .check_runs
            .page_by_commit(&scope, repository.id, head_sha, window)
            .await?;

        Ok((
            Page::new(
                items,
                PageInfo {
                    next_cursor: None,
                    prev_cursor: None,
                    limit: window.limit(),
                },
            ),
            total,
        ))
    }

    /// Insert or update a mirrored check-run row for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_check_run(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: CheckRunRecord,
    ) -> Result<CheckRun, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &CHECK_RUN_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CheckRunRecord {
            repo_id: repository.id,
            ..record
        };
        self.check_runs.upsert(&scope, tenant_id, record).await
    }

    /// List the mirrored timeline of one issue or pull request, in the order
    /// GitHub served it, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_issue_timeline(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        issue_number: i64,
        window: PageWindow,
    ) -> Result<Page<IssueTimelineEvent>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_TIMELINE_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let items = self
            .issue_timeline
            .list_by_issue(&scope, repository.id, issue_number, window)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: window.limit(),
            },
        ))
    }

    /// Insert or update one mirrored timeline entry for the caller's tenant.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored;
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn upsert_issue_timeline_event(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        record: IssueTimelineEventRecord,
    ) -> Result<IssueTimelineEvent, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &ISSUE_TIMELINE_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let full_name = repo_full_name(owner, name)?;
        let repository = self
            .repo
            .find_by_full_name(&scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = IssueTimelineEventRecord {
            repo_id: repository.id,
            ..record
        };
        self.issue_timeline.upsert(&scope, tenant_id, record).await
    }

    /// Drop cached GitHub responses for one owner or one repository.
    ///
    /// DESIGN §4's `clear_cache(session, scope)`. The mirrored rows are left
    /// alone: this only discards the raw responses, so the next sync re-fetches
    /// rather than revalidating.
    ///
    /// # Errors
    /// `DomainError::Validation` when `owner`, or `owner/name`, is not a
    /// usable GitHub path; `Forbidden`/`Database` as usual.
    pub async fn clear_cache(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: Option<&str>,
    ) -> Result<u64, DomainError> {
        match name {
            Some(name) => validate_repo_path(owner, name)?,
            None => validate_owner(owner)?,
        }
        // Clearing a tenant's own cache is a sync-scoped action: it changes
        // nothing anyone can read, only what the next sync will re-fetch.
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SYNC_RESOURCE,
                actions::SYNC,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let repo_ids = self.cached_repo_ids(ctx, owner, name).await;

        let removed = self
            .github
            .clear_cache(&scope, owner, name, &repo_ids)
            .await?;
        tracing::info!(
            owner,
            repository = name,
            removed,
            "cleared cached responses"
        );
        Ok(removed)
    }

    /// Hand the service the token the gear cancels on shutdown. Called once,
    /// when the sync pool starts.
    ///
    /// A second, different token is logged and dropped rather than returned as
    /// an error: the pool that is already running is cancelled by the first
    /// one, and swapping it would leave that pool with no way to be stopped.
    /// The caller has nothing useful to do about it, which is why nothing is
    /// handed back.
    pub fn bind_shutdown(&self, token: CancellationToken) {
        if self.shutdown.set(token).is_err() {
            tracing::warn!("the shutdown token is already bound; keeping the first one");
        }
    }

    /// The shutdown token, or a detached one before the pool has started.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.get().cloned().unwrap_or_default()
    }

    /// What a sync collects when the request does not narrow it.
    #[must_use]
    pub fn default_scope(&self) -> ScopeConfig {
        self.config.scope
    }

    /// How many repositories the sync worker pool runs at once.
    #[must_use]
    pub fn max_concurrent_syncs(&self) -> usize {
        self.config.max_concurrent_syncs.get()
    }

    /// Scope for writing this tenant's session rows.
    async fn session_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        Ok(self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SYNC_SESSION_RESOURCE,
                action,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?)
    }

    async fn sync_access_scope(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        Ok(self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SYNC_RESOURCE,
                actions::SYNC,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?)
    }

    /// Scope for this tenant's per-repository run-status rows.
    /// GitHub's id for a mirrored repository, once Discovery has stored it.
    /// A repository not yet mirrored, or a lookup the caller may not make,
    /// simply leaves the run-status row without an id.
    async fn stored_repo_id(&self, ctx: &SecurityContext, repo_full_name: &str) -> Option<i64> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await
            .ok()?;
        self.repo
            .find_by_full_name(&scope, repo_full_name)
            .await
            .ok()?
            .map(|repo| repo.id)
    }

    async fn repo_status_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        Ok(self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPO_SYNC_STATUS_RESOURCE,
                action,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?)
    }

    /// Write the repository's run status, preserving whatever a previous run
    /// recorded in the fields this transition does not own.
    async fn mark_repo_complete(
        &self,
        ctx: &SecurityContext,
        repo_full_name: &str,
        session_id: Uuid,
    ) -> Result<(), DomainError> {
        let scope = self.repo_status_scope(ctx, actions::UPSERT).await?;
        let repo_id = self.stored_repo_id(ctx, repo_full_name).await;
        let written = self
            .repo_sync_status
            .complete_if_last_session(&scope, repo_full_name, session_id, repo_id, &now_rfc3339())
            .await?;
        if !written {
            tracing::info!(
                repository = repo_full_name,
                session_id = %session_id,
                "a newer sync owns this repository's run status; leaving it as it is"
            );
        }
        Ok(())
    }

    async fn mark_repo_status_in(
        &self,
        scope: &AccessScope,
        ctx: &SecurityContext,
        repo_full_name: &str,
        session_id: Uuid,
        status: RepoRunStatus,
        synced_at: Option<String>,
    ) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let previous = self.repo_sync_status.find(scope, repo_full_name).await?;
        let repo_id = match previous.as_ref().and_then(|p| p.repo_id) {
            Some(id) => Some(id),
            None => self.stored_repo_id(ctx, repo_full_name).await,
        };
        let record = RepoSyncStatusRecord {
            repo_full_name: repo_full_name.to_owned(),
            repo_id,
            status,
            last_session_id: Some(session_id),
            last_synced_at: synced_at.or_else(|| previous.and_then(|p| p.last_synced_at)),
        };
        self.repo_sync_status
            .upsert(scope, tenant_id, record)
            .await?;
        Ok(())
    }

    /// GitHub's ids for the repositories a cache clear covers, so the pages
    /// cached under `…/repositories/{id}/...` go with the rest.
    ///
    /// Best effort by design: reading them needs repository-list rights,
    /// which a caller holding only the sync right does not have, and the
    /// clear itself is already authorised by then. Without the ids the owner
    /// and slug prefixes still clear; only the linked later pages survive,
    /// and a forced sync ignores the cache anyway.
    async fn cached_repo_ids(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: Option<&str>,
    ) -> Vec<i64> {
        let found = async {
            let repo_scope = self
                .policy_enforcer
                .access_scope_with(
                    ctx,
                    &REPO_RESOURCE,
                    actions::LIST,
                    None,
                    &AccessRequest::new().resource_property(
                        pep_properties::OWNER_TENANT_ID,
                        ctx.subject_tenant_id(),
                    ),
                )
                .await?;
            let ids: Vec<i64> = match name {
                Some(name) => self
                    .repo
                    .find_by_full_name(&repo_scope, &format!("{owner}/{name}"))
                    .await?
                    .map(|repo| repo.id)
                    .into_iter()
                    .collect(),
                None => self.repo.ids_by_owner(&repo_scope, owner).await?,
            };
            Ok::<_, DomainError>(ids)
        }
        .await;

        match found {
            Ok(ids) => ids,
            Err(e @ DomainError::Forbidden(_)) => {
                tracing::info!(
                    owner,
                    repository = name,
                    error = %e.public_text(),
                    "clearing the cache without GitHub's repository ids; the pages linked under \
                     them stay until they are re-fetched"
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(
                    owner,
                    repository = name,
                    error = %e.public_text(),
                    "could not read GitHub's repository ids for the cache clear; the pages \
                     linked under them stay until they are re-fetched"
                );
                Vec::new()
            }
        }
    }

    /// The tenant's per-repository run statuses, optionally one status only.
    ///
    /// # Errors
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_repo_sync_status(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
        status: Option<RepoRunStatus>,
    ) -> Result<Page<RepoSyncStatusRecord>, DomainError> {
        let scope = self.repo_status_scope(ctx, actions::LIST).await?;

        let limit = list_limit(query)?;
        let after = cursor_keys(query, REPO_STATUS_ORDER, 1)?;
        let rows = self
            .repo_sync_status
            .list(
                &scope,
                status,
                after.map(|keys| keys[0].as_str()),
                limit.saturating_add(1),
            )
            .await?;

        keyset_page(rows, limit, |last| {
            encode_cursor(
                REPO_STATUS_ORDER,
                SortDir::Asc,
                vec![last.repo_full_name.clone()],
            )
        })
    }

    /// The repositories a resume should re-run: one named slug, or every
    /// repository the scope can see that is still `in_progress`.
    async fn repos_awaiting_resume(
        &self,
        ctx: &SecurityContext,
        only: Option<&str>,
    ) -> Result<Vec<RepoSyncStatusRecord>, DomainError> {
        let scope = self.repo_status_scope(ctx, actions::LIST).await?;

        let Some(slug) = only else {
            return self
                .repo_sync_status
                .list(
                    &scope,
                    Some(RepoRunStatus::InProgress),
                    None,
                    u64::try_from(RESUME_LIMIT).unwrap_or(u64::MAX),
                )
                .await;
        };

        Ok(self
            .repo_sync_status
            .find(&scope, slug)
            .await?
            .filter(|r| r.status == RepoRunStatus::InProgress)
            .map_or_else(Vec::new, |r| vec![r]))
    }

    /// Re-run every repository this tenant still has marked `in_progress`.
    ///
    /// This is PRD §5.2's resume operation. Resume is a re-run, not a restore:
    /// nothing about the interrupted run is replayed. What keeps the re-run
    /// cheap is the state the previous one left behind — a stored `ETag` lets
    /// GitHub answer `304` for a page that has not changed, and a stored
    /// fingerprint keeps an unchanged entity from being fetched again.
    ///
    /// Resume takes no per-run scope: `ALGORITHMS.md` §8 has it resolve scope
    /// from configuration only, so a resumed run collects whatever the gear
    /// is configured to collect.
    ///
    /// `only` narrows the operation to one `owner/name` slug, matching the
    /// documented CLI's `resume <ORG/REPO>`. A repository that is not
    /// `in_progress` has nothing to resume and yields an empty result rather
    /// than a fresh sync — asking for that is what `POST /sync` is for.
    ///
    /// A repository whose sync is already queued or running resumes into that
    /// run rather than a second one, so calling resume twice is harmless.
    ///
    /// # Errors
    /// `Forbidden`/`Database` as usual, and, when `only` names one
    /// repository, the error that kept it from being queued. Otherwise a
    /// repository that cannot be queued is listed in `refused`, so one full
    /// queue does not abandon the rest.
    pub async fn resume_incomplete_syncs(
        &self,
        ctx: &SecurityContext,
        only: Option<&str>,
        force: bool,
    ) -> Result<ResumeOutcome, DomainError> {
        let pending = self.repos_awaiting_resume(ctx, only).await?;
        let scopes = self.enqueue_scopes(ctx).await?;

        let mut outcome = ResumeOutcome::default();
        for repo in pending {
            let Some((owner, name)) = repo.repo_full_name.split_once('/') else {
                tracing::warn!(
                    repository = %repo.repo_full_name,
                    "run-status row has no owner/name slug; skipping"
                );
                continue;
            };
            match self
                .enqueue_sync_scoped(ctx, &scopes, owner, name, None, force, None)
                .await
            {
                Ok(queued) => outcome.session_ids.push(queued.session_id),
                Err(e) if only.is_some() => return Err(e),
                Err(e) => {
                    tracing::warn!(
                        repository = %repo.repo_full_name,
                        error = %e,
                        "could not queue a resume for this repository"
                    );
                    outcome.refused.push(RefusedResume {
                        repository: repo.repo_full_name,
                        error: e,
                    });
                }
            }
        }

        Ok(outcome)
    }

    /// Record a sync request and hand it to the background worker.
    ///
    /// Returns as soon as the `queued` row is durable — the fetch itself
    /// happens later, on the gear's background task. Poll
    /// [`Self::get_session`] with the returned id to watch it finish.
    ///
    /// A repository already queued or running on the same terms collapses into
    /// that run: the caller gets its session id back instead of a second
    /// session, so a double-click, a retry and a resume of the same repository
    /// cost one sync. `force` does not split the two — the run in flight is
    /// already fetching the repository. The status that comes back says which
    /// of the two happened: `queued` for a session this call created, whatever
    /// the existing session holds for one it joined.
    ///
    /// A request that asks for a different scope or a different `since` is
    /// refused instead, because collapsing it would report a sync of terms it
    /// never asked for.
    ///
    /// # Errors
    /// `Conflict` when a sync of this repository is running on other terms,
    /// `Forbidden`/`Database` as usual, or `Unavailable` when the queue is full
    /// or the background worker is not running; in the last case the session
    /// is left behind in `failed` rather than silently dropped.
    pub async fn enqueue_sync(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        sync_scope: Option<ScopeConfig>,
        force: bool,
        since: Option<DateTime<Utc>>,
    ) -> Result<QueuedSync, DomainError> {
        let scopes = self.enqueue_scopes(ctx).await?;
        self.enqueue_sync_scoped(ctx, &scopes, owner, name, sync_scope, force, since)
            .await
    }

    /// The scopes one sync request needs, resolved once so a resume of hundreds
    /// of repositories asks the policy enforcer once, not per repository. The
    /// sync permission is among them, so a caller without it gets 403 here
    /// rather than a 202 for a run that fails later.
    async fn enqueue_scopes(&self, ctx: &SecurityContext) -> Result<EnqueueScopes, DomainError> {
        Ok(EnqueueScopes {
            sync: self.sync_access_scope(ctx).await?,
            session: self.session_scope(ctx, actions::UPSERT).await?,
            repo_status: self.repo_status_scope(ctx, actions::UPSERT).await?,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the request's own terms, one per argument; they are what the claim \
                  is compared on, so hiding them in a struct would make that \
                  comparison harder to read, not easier"
    )]
    async fn enqueue_sync_scoped(
        &self,
        ctx: &SecurityContext,
        scopes: &EnqueueScopes,
        owner: &str,
        name: &str,
        sync_scope: Option<ScopeConfig>,
        force: bool,
        since: Option<DateTime<Utc>>,
    ) -> Result<QueuedSync, DomainError> {
        let (job, session) = match self
            .prepare_sync(ctx, scopes, owner, name, sync_scope, force, since)
            .await?
        {
            PreparedSync::Joined(running) => return Ok(running),
            PreparedSync::Claimed { job, session } => (job, session),
        };
        let id = session.id;
        if let Err(e) = self.sync_tx.try_send(*job) {
            let error = match e {
                mpsc::error::TrySendError::Full(_) => DomainError::Unavailable {
                    message: "the sync queue is full; try again shortly".to_owned(),
                    retry_after_secs: Some(QUEUE_FULL_RETRY_AFTER_SECS),
                },
                mpsc::error::TrySendError::Closed(_) => DomainError::Unavailable {
                    message: "the sync worker is not running".to_owned(),
                    retry_after_secs: None,
                },
            };
            self.fail_session(
                &scopes.session,
                ctx.subject_tenant_id(),
                *session,
                error.public_text(),
            )
            .await;
            return Err(error);
        }

        Ok(QueuedSync {
            session_id: id,
            status: SessionStatus::Queued,
        })
    }

    #[must_use]
    pub fn spawn_sync_now(
        self: &Arc<Self>,
        ctx: SecurityContext,
        owner: String,
        name: String,
    ) -> tokio::task::JoinHandle<Result<SyncSummary, DomainError>> {
        let service = Arc::clone(self);
        self.in_process_syncs.spawn(async move {
            let shutdown = service.shutdown_token();
            let _slot = tokio::select! {
                slot = service.in_process_slots.acquire() => slot.map_err(|e| {
                    DomainError::internal(format!("the in-process sync slots are closed: {e}"))
                })?,
                () = shutdown.cancelled() => return Err(DomainError::Cancelled),
            };
            service.sync_now(&ctx, &owner, &name).await
        })
    }

    pub async fn wait_for_in_process_syncs(&self) {
        self.in_process_syncs.close();
        self.in_process_syncs.wait().await;
    }

    /// Sync `owner/name` on the task that calls this and hand back what it
    /// collected; `LocalClient` calls it on a task of its own, so a caller that
    /// drops the SDK call does not stop the run. It takes the claim, the
    /// session row and the repository status a queued sync takes, so it shows
    /// in `/sessions`, keeps its lock alive with a heartbeat and stops at the
    /// deadline; only the queue and the pool are skipped.
    ///
    /// # Errors
    /// `Conflict` when a sync of this repository is already in flight, the
    /// sync's own error when it fails, and `Cancelled` when the gear stops it.
    pub async fn sync_now(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
    ) -> Result<SyncSummary, DomainError> {
        validate_repo_path(owner, name)?;
        let scopes = self.enqueue_scopes(ctx).await?;
        let job = match self
            .prepare_sync(ctx, &scopes, owner, name, None, false, None)
            .await?
        {
            PreparedSync::Joined(running) => {
                return Err(DomainError::Conflict(format!(
                    "a sync of {owner}/{name} is already in flight; session {} is the one \
                     running",
                    running.session_id
                )));
            }
            PreparedSync::Claimed { job, .. } => job,
        };
        self.run_and_record(&job, &self.shutdown_token()).await?
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the request's own terms, one per argument, as in `enqueue_sync_scoped`"
    )]
    async fn prepare_sync(
        &self,
        ctx: &SecurityContext,
        scopes: &EnqueueScopes,
        owner: &str,
        name: &str,
        sync_scope: Option<ScopeConfig>,
        force: bool,
        since: Option<DateTime<Utc>>,
    ) -> Result<PreparedSync, DomainError> {
        let sync_scope = sync_scope.unwrap_or(self.config.scope);
        sync_scope.validate()?;
        let tenant_id = ctx.subject_tenant_id();
        let scope = &scopes.session;
        let key = (tenant_id, format!("{owner}/{name}"));
        let id = Uuid::new_v4();
        let now = now_rfc3339();
        let session = SyncSessionRecord {
            id,
            repo_full_name: format!("{owner}/{name}"),
            repo_id: None,
            status: SessionStatus::Queued,
            progress_percent: 0,
            error: None,
            summary_json: None,
            created_at: now.clone(),
            started_at: None,
            ended_at: None,
            updated_at: Some(now.clone()),
        };
        self.release_lock_left_by_a_dead_run(scopes, tenant_id, &key.1)
            .await;

        let claim = {
            // Held for this repository only, so two concurrent requests for it
            // cannot both decide they are the first while a request for
            // another repository waits on nothing. The session row is written
            // before the gate is let go, so the id a collapsing request of
            // this process is handed always names a session it can read.
            let lease = self.claim_gates.lease(&key);
            let claimed = lease.lock().await;

            let active = ActiveSyncRecord {
                repo_full_name: key.1.clone(),
                session_id: id,
                owner_id: self.instance_id,
                scope: sync_scope,
                since,
                updated_at: now,
            };
            if let Some(running) = self.take_active_sync(scopes, tenant_id, &active).await? {
                drop(claimed);
                return self
                    .join_or_refuse(scope, &key.1, running, sync_scope, since)
                    .await
                    .map(PreparedSync::Joined);
            }
            let claim = ClaimRelease {
                active_syncs: Arc::clone(&self.active_syncs),
                scope: scopes.repo_status.clone(),
                repo_full_name: key.1,
                session_id: id,
            };
            self.mark_repo_status_in(
                &scopes.repo_status,
                ctx,
                &session.repo_full_name,
                id,
                RepoRunStatus::InProgress,
                None,
            )
            .await?;
            self.sync_sessions
                .upsert(scope, tenant_id, session.clone())
                .await?;
            claim
        };

        let job = SyncJob {
            session_id: id,
            ctx: ctx.clone(),
            owner: owner.to_owned(),
            name: name.to_owned(),
            scope: sync_scope,
            force,
            since,
            access_scope: scopes.sync.clone(),
            claim: Some(claim),
        };
        Ok(PreparedSync::Claimed {
            job: Box::new(job),
            session: Box::new(session),
        })
    }

    /// Close a queued session out as failed, so a caller polling it is not
    /// told the work is waiting when nothing will ever run it.
    ///
    /// Failing to record that is logged rather than returned: the caller is
    /// already getting the error that stopped the sync, and the start-up sweep
    /// closes out whatever a dead process left behind.
    async fn fail_session(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        mut session: SyncSessionRecord,
        reason: String,
    ) {
        session.status = SessionStatus::Failed;
        session.progress_percent = 100;
        session.ended_at = Some(now_rfc3339());
        session.updated_at.clone_from(&session.ended_at);
        session.error = Some(reason);
        if let Err(e) = self
            .upsert_session_with_retry(scope, tenant_id, session)
            .await
        {
            tracing::error!(
                error = %crate::redact::redacted(&e.to_string()),
                "a sync that could not be queued or started could not be marked failed either"
            );
        }
    }

    async fn upsert_session_with_retry(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        session: SyncSessionRecord,
    ) -> Result<(), DomainError> {
        let mut attempt = 1;
        loop {
            match self
                .sync_sessions
                .upsert(scope, tenant_id, session.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if attempt < SESSION_WRITE_ATTEMPTS => {
                    tracing::warn!(
                        session_id = %session.id,
                        attempt,
                        error = %crate::redact::redacted(&e.to_string()),
                        "could not write the sync session; trying again"
                    );
                    tokio::time::sleep(SESSION_WRITE_RETRY_DELAY).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn finish_session_with_retry(
        &self,
        scope: &AccessScope,
        session: &SyncSessionRecord,
    ) -> Result<bool, DomainError> {
        let mut attempt = 1;
        loop {
            match self.sync_sessions.finish_if_running(scope, session).await {
                Ok(written) => return Ok(written),
                Err(e) if attempt < SESSION_WRITE_ATTEMPTS => {
                    tracing::warn!(
                        session_id = %session.id,
                        attempt,
                        error = %crate::redact::redacted(&e.to_string()),
                        "could not write the sync session; trying again"
                    );
                    tokio::time::sleep(SESSION_WRITE_RETRY_DELAY).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn interrupt_unstarted_job(&self, job: SyncJob) {
        let tenant_id = job.ctx.subject_tenant_id();
        match self
            .sync_sessions
            .find_by_id(&job.access_scope, job.session_id)
            .await
        {
            Ok(Some(mut session)) if session.status == SessionStatus::Queued => {
                session.status = SessionStatus::Interrupted;
                session.progress_percent = 100;
                session.ended_at = Some(now_rfc3339());
                session.updated_at.clone_from(&session.ended_at);
                session.error = Some("the gear stopped before this sync started".to_owned());
                if let Err(e) = self
                    .upsert_session_with_retry(&job.access_scope, tenant_id, session)
                    .await
                {
                    tracing::error!(
                        session_id = %job.session_id,
                        error = %crate::redact::redacted(&e.to_string()),
                        "a sync dropped at shutdown could not be marked interrupted"
                    );
                }
            }
            Ok(_) => {}
            Err(e) => tracing::error!(
                session_id = %job.session_id,
                error = %crate::redact::redacted(&e.to_string()),
                "a sync dropped at shutdown could not be read to mark it interrupted"
            ),
        }
    }

    async fn fail_unstarted_session(&self, job: &SyncJob, error: &DomainError) {
        let tenant_id = job.ctx.subject_tenant_id();
        match self
            .sync_sessions
            .find_by_id(&job.access_scope, job.session_id)
            .await
        {
            Ok(Some(session)) if session.status == SessionStatus::Queued => {
                self.fail_session(&job.access_scope, tenant_id, session, error.public_text())
                    .await;
            }
            Ok(_) => {}
            Err(e) => tracing::error!(
                session_id = %job.session_id,
                error = %crate::redact::redacted(&e.to_string()),
                "a sync that could not start could not be marked failed either"
            ),
        }
    }

    async fn start_session(
        &self,
        job: &SyncJob,
    ) -> Result<(AccessScope, SyncSessionRecord), DomainError> {
        let scope = self.session_scope(&job.ctx, actions::UPSERT).await?;
        let mut session = self
            .sync_sessions
            .find_by_id(&scope, job.session_id)
            .await?
            .ok_or(DomainError::SessionNotFound)?;
        session.status = SessionStatus::InProgress;
        session.started_at = Some(now_rfc3339());
        session.updated_at.clone_from(&session.started_at);
        session.ended_at = None;
        session.error = None;
        self.sync_sessions
            .upsert(&scope, job.ctx.subject_tenant_id(), session.clone())
            .await?;
        Ok((scope, session))
    }

    /// Answer a request for a repository a sync already holds: its session
    /// when the terms match, a refusal when they do not.
    ///
    /// Refused rather than queued behind the run: the repository's lock is
    /// held for the whole of it, so a second job would take a session, reach
    /// that lock and end failed. Saying so now lets the caller watch the run
    /// it was told about and ask again after.
    ///
    /// # Errors
    /// `Conflict` when the terms differ; `Database` when the running
    /// session's row cannot be read.
    async fn join_or_refuse(
        &self,
        scope: &AccessScope,
        repo_full_name: &str,
        running: Claim,
        sync_scope: ScopeConfig,
        since: Option<DateTime<Utc>>,
    ) -> Result<QueuedSync, DomainError> {
        if running.scope != sync_scope || running.since != since {
            tracing::debug!(
                repository = repo_full_name,
                session_id = %running.session_id,
                "a sync of this repository is running on other terms"
            );
            return Err(DomainError::Conflict(format!(
                "a sync of {repo_full_name} is already running on different terms; \
                 session {} is the one in flight",
                running.session_id
            )));
        }

        tracing::debug!(
            repository = repo_full_name,
            session_id = %running.session_id,
            "sync already in flight; collapsing into it"
        );
        // Read rather than assumed: the row is written before the claim goes
        // in, so it is there, and a worker may already have moved it on from
        // `queued`.
        let status = self
            .running_status(scope, repo_full_name, running.session_id)
            .await?;
        Ok(QueuedSync {
            session_id: running.session_id,
            status,
        })
    }

    async fn running_status(
        &self,
        scope: &AccessScope,
        repo_full_name: &str,
        session_id: Uuid,
    ) -> Result<SessionStatus, DomainError> {
        let session = self.sync_sessions.find_by_id(scope, session_id).await?;
        if session.is_none() {
            tracing::warn!(
                repository = repo_full_name,
                session_id = %session_id,
                "the running sync's session row is missing; reporting it as in progress"
            );
        }
        Ok(session.map_or(SessionStatus::InProgress, |session| session.status))
    }

    async fn take_active_sync(
        &self,
        scopes: &EnqueueScopes,
        tenant_id: Uuid,
        mine: &ActiveSyncRecord,
    ) -> Result<Option<Claim>, DomainError> {
        for _ in 0..TAKE_ACTIVE_SYNC_ATTEMPTS {
            let held = self
                .active_syncs
                .find(&scopes.repo_status, &mine.repo_full_name)
                .await?;
            let taken = match held {
                None => {
                    self.active_syncs
                        .insert(&scopes.repo_status, tenant_id, mine)
                        .await?
                }
                Some(held) if self.active_sync_is_live(&scopes.session, &held).await? => {
                    return Ok(Some(Claim {
                        session_id: held.session_id,
                        scope: held.scope,
                        since: held.since,
                    }));
                }
                Some(held) => {
                    self.active_syncs
                        .replace(&scopes.repo_status, mine, held.session_id)
                        .await?
                }
            };
            if taken {
                return Ok(None);
            }
        }
        Err(DomainError::Conflict(format!(
            "another request is starting a sync of {} right now; try again",
            mine.repo_full_name
        )))
    }

    async fn active_sync_is_live(
        &self,
        scope: &AccessScope,
        held: &ActiveSyncRecord,
    ) -> Result<bool, DomainError> {
        if silent_too_long(&held.updated_at, Utc::now()) {
            return Ok(false);
        }
        let session = self
            .sync_sessions
            .find_by_id(scope, held.session_id)
            .await?;
        Ok(session.is_none_or(|session| {
            matches!(
                session.status,
                SessionStatus::Queued | SessionStatus::InProgress
            )
        }))
    }

    /// Refresh every `gm_active_syncs` row this process holds.
    ///
    /// # Errors
    /// `Database` when the rows cannot be updated.
    pub(crate) async fn touch_active_syncs(&self) -> Result<(), DomainError> {
        self.active_syncs
            .touch(&AccessScope::allow_all(), self.instance_id, &now_rfc3339())
            .await
    }

    /// Take sole ownership of the job stream. The gear's background task calls
    /// this once at startup; every later call sees `None`.
    pub async fn take_sync_receiver(&self) -> Option<mpsc::Receiver<SyncJob>> {
        self.sync_rx.lock().await.take()
    }

    /// Run one queued job to completion and record the outcome on its session.
    ///
    /// Errors from the sync land in the session row rather than propagating —
    /// nobody is waiting on the return value, so a failed run must still be
    /// visible through the sessions API.
    ///
    /// # Errors
    /// Only failures to *persist* the outcome, which the caller logs.
    pub async fn run_sync_job(
        &self,
        job: &SyncJob,
        cancel: &CancellationToken,
    ) -> Result<(), DomainError> {
        self.run_and_record(job, cancel).await.map(|_| ())
    }

    async fn run_and_record(
        &self,
        job: &SyncJob,
        cancel: &CancellationToken,
    ) -> Result<Result<SyncSummary, DomainError>, DomainError> {
        let (scope, mut session) = match self.start_session(job).await {
            Ok(started) => started,
            Err(e) => {
                self.fail_unstarted_session(job, &e).await;
                return Err(e);
            }
        };

        let progress = SyncProgress::new();
        let outcome = self
            .sync_within_deadline(job, &scope, &progress, cancel)
            .await;

        let completed = outcome.is_ok();
        match &outcome {
            Ok(summary) => {
                session.status = SessionStatus::Complete;
                session.summary_json = stored_summary_json(job.session_id, summary);
            }
            Err(e) => {
                tracing::error!(
                    session_id = %job.session_id,
                    repository = %format!("{}/{}", job.owner, job.name),
                    error = %crate::redact::redacted(&e.to_string()),
                    "sync run failed"
                );
                session.status = if matches!(e, DomainError::Cancelled) {
                    SessionStatus::Interrupted
                } else {
                    SessionStatus::Failed
                };
                session.error = Some(e.public_text());
            }
        }
        // Whatever the outcome: the run is over, and a caller watching the
        // percentage should not be left waiting at the point a failed run
        // stopped. What happened is in `status` and `error`.
        progress.finished();
        session.progress_percent = i32::from(progress.percent());
        session.ended_at = Some(now_rfc3339());
        session.updated_at.clone_from(&session.ended_at);
        let repo_full_name = session.repo_full_name.clone();
        if !self.finish_session_with_retry(&scope, &session).await? {
            report_session_closed_meanwhile(job.session_id);
            return Ok(outcome);
        }

        // A run that failed leaves the repository `in_progress` on purpose:
        // that is the marker the resume operation looks for (PRD §5.2).
        if completed {
            self.mark_repo_complete(&job.ctx, &repo_full_name, job.session_id)
                .await?;
        }

        Ok(outcome)
    }

    /// Run the sync, stopping it if it passes the configured deadline.
    ///
    /// A run that will not end otherwise ends here: the engine is told to
    /// stop and then given time to wind down, so the tasks in flight finish
    /// their writes and the repository's lock is released. The durable state
    /// keeps what the run reached, so the next sync or resume carries on from
    /// there rather than starting again.
    ///
    /// The token cancelled is this job's own, a child of the pool's, so a
    /// deadline stops one repository and a shutdown still stops them all.
    async fn sync_within_deadline(
        &self,
        job: &SyncJob,
        session_scope: &AccessScope,
        progress: &SyncProgress,
        cancel: &CancellationToken,
    ) -> Result<SyncSummary, DomainError> {
        let job_cancel = cancel.child_token();
        let deadline = self.config.sync_deadline;
        let sync = self.sync_with_heartbeat(job, session_scope, progress, &job_cancel);
        let mut sync = std::pin::pin!(sync);

        tokio::select! {
            outcome = &mut sync => outcome,
            () = tokio::time::sleep(deadline) => {
                job_cancel.cancel();
                let stopped_with = sync.await.err();
                Err(past_deadline(job, deadline, stopped_with))
            }
        }
    }

    /// Run the sync while a ticker writes its progress to the session row.
    ///
    /// DESIGN §4 has progress "published via a shared atomic" and persisted
    /// "incrementally ... via a heartbeat (also stamping `ended_at`)", so a
    /// caller polling the session sees the run advance. Here the beat stamps
    /// `updated_at` instead, so `ended_at` means what it says, and it writes
    /// only the two columns it owns rather than a copy of the whole row. A
    /// heartbeat that fails to write is logged and skipped — losing a progress
    /// sample must not fail the sync.
    async fn sync_with_heartbeat(
        &self,
        job: &SyncJob,
        session_scope: &AccessScope,
        progress: &SyncProgress,
        cancel: &CancellationToken,
    ) -> Result<SyncSummary, DomainError> {
        let options = FetchOptions {
            tenant_id: job.ctx.subject_tenant_id(),
            access_scope: job.access_scope.clone(),
            scope: job.scope,
            force: job.force,
            since: job.since,
            cancel: cancel.clone(),
        };
        let stop_beating = CancellationToken::new();
        let _stop_on_drop = stop_beating.clone().drop_guard();
        let beat = tokio::spawn(heartbeat(
            Arc::clone(&self.sync_sessions),
            session_scope.clone(),
            job.session_id,
            progress.handle(),
            stop_beating.clone(),
        ));

        let outcome = self
            .sync_repository_scoped(&job.ctx, &job.owner, &job.name, &options, progress, cancel)
            .await;

        stop_beating.cancel();
        if let Err(e) = beat.await {
            tracing::warn!(
                session_id = %job.session_id,
                error = %e,
                "sync heartbeat task did not finish cleanly"
            );
        }
        outcome
    }

    /// Close out sessions left mid-flight by a previous process.
    ///
    /// Only `queued` and `in_progress` rows are read. A row already
    /// `interrupted` needs nothing done to it, and those rows are never
    /// pruned, so reading them would make every restart walk work it finished
    /// long ago. What is left is what one process had in flight, which the
    /// pool width bounds.
    ///
    /// Only a row [`abandoned`] says is dead, and whose `gm_active_syncs` row
    /// has gone as long without a refresh, is closed: during a rolling deploy
    /// or with two replicas the other process keeps both fresh, so its runs
    /// are left alone. A row from a process that died moments before this one
    /// started is not abandoned yet, which is why start-up calls this again after
    /// [`SWEEP_AGAIN_AFTER`]. Across every tenant — hence the unconstrained
    /// scope, which is why this takes no [`SecurityContext`] and is not
    /// reachable from the API.
    ///
    /// # Errors
    /// `Database` when the sweep cannot read or write the session table.
    pub async fn sweep_interrupted_sessions(
        &self,
        scope: &AccessScope,
    ) -> Result<usize, DomainError> {
        let stale = self
            .sync_sessions
            .list_by_statuses(scope, &[SessionStatus::Queued, SessionStatus::InProgress])
            .await?;

        let now = Utc::now();
        let live: HashSet<Uuid> = self
            .active_syncs
            .list(scope)
            .await?
            .into_iter()
            .filter(|active| !silent_too_long(&active.updated_at, now))
            .map(|active| active.session_id)
            .collect();
        let mut count = 0;
        for (tenant_id, mut session) in stale {
            if !abandoned(&session, now) || live.contains(&session.id) {
                continue;
            }
            let (repo_full_name, session_id) = (session.repo_full_name.clone(), session.id);
            self.release_stale_sync_lock(tenant_id, &session.repo_full_name)
                .await;
            count += 1;
            session.status = SessionStatus::Interrupted;
            session.progress_percent = 100;
            session.ended_at = Some(now_rfc3339());
            session.updated_at.clone_from(&session.ended_at);
            session.error = Some("the server restarted while this sync was in flight".to_owned());
            self.sync_sessions.upsert(scope, tenant_id, session).await?;
            self.active_syncs
                .delete(scope, &repo_full_name, session_id)
                .await?;
        }

        Ok(count)
    }

    /// Drop the per-repo sync lock marker left for `repo` by a run whose
    /// process is gone.
    ///
    /// Only ever called for a session [`abandoned`] says is dead, because the
    /// lock itself cannot say who holds it: during a rolling restart the
    /// marker may belong to the outgoing process, still syncing that
    /// repository, and taking it would let both sync it at once.
    async fn release_stale_sync_lock(&self, tenant_id: Uuid, repo: &str) {
        let Some((owner, name)) = repo.split_once('/') else {
            return;
        };
        let lock_key = format!("sync/{tenant_id}/{owner}/{name}");
        match self
            .db
            .db()
            .remove_lock_marker_at_startup(GEAR_NAME, &lock_key)
            .await
        {
            Ok(true) => tracing::info!(
                repository = repo,
                "released the sync lock a dead process left behind"
            ),
            Ok(false) => {}
            Err(e) => tracing::warn!(
                repository = repo,
                error = %e,
                "could not release a stale sync lock"
            ),
        }
    }

    /// Drop the sync lock marker for `repo_full_name` when the run that took
    /// it is gone.
    ///
    /// The start-up sweep reads only sessions still queued or in progress, so
    /// a marker whose session an earlier sweep already marked `interrupted`
    /// has nothing left to clean it, and every later request for that
    /// repository would answer 409 with nothing running. A new request is the
    /// next moment anyone asks about that repository, so the check belongs
    /// here: the run-status row still names the session that took the lock,
    /// because a request writes its own session into that row only further
    /// down.
    ///
    /// Best effort throughout. A row that is not there, a status that is not
    /// `in_progress`, a session that is not `in_progress` or `interrupted`
    /// (a `queued` one never took the lock, a `failed` or `complete` one
    /// already gave it back), or a read that fails leaves the marker alone,
    /// and the request goes on to take the lock or to answer 409 exactly as
    /// before.
    async fn release_lock_left_by_a_dead_run(
        &self,
        scopes: &EnqueueScopes,
        tenant_id: Uuid,
        repo_full_name: &str,
    ) {
        let Ok(Some(status)) = self
            .repo_sync_status
            .find(&scopes.repo_status, repo_full_name)
            .await
        else {
            return;
        };
        if status.status != RepoRunStatus::InProgress {
            return;
        }
        let Some(session_id) = status.last_session_id else {
            return;
        };
        let Ok(Some(session)) = self
            .sync_sessions
            .find_by_id(&scopes.session, session_id)
            .await
        else {
            return;
        };
        let took_the_lock = matches!(
            session.status,
            SessionStatus::InProgress | SessionStatus::Interrupted
        );
        if took_the_lock && abandoned(&session, Utc::now()) {
            self.release_stale_sync_lock(tenant_id, repo_full_name)
                .await;
        }
    }

    /// One sync session by id, tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::SessionNotFound` when the session does not exist for
    /// this tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn get_session(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<SyncSessionRecord, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SYNC_SESSION_RESOURCE,
                actions::GET,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;
        self.sync_sessions
            .find_by_id(&scope, id)
            .await?
            .ok_or(DomainError::SessionNotFound)
    }

    /// The tenant's sync sessions, newest first.
    ///
    /// # Errors
    /// `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_sessions(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<SyncSessionRecord>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &SYNC_SESSION_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let limit = list_limit(query)?;
        let after = cursor_keys(query, SESSIONS_ORDER, 2)?
            .map(|keys| {
                let id = keys[1].parse::<Uuid>().map_err(|_| invalid_cursor())?;
                Ok::<_, DomainError>((keys[0].as_str(), id))
            })
            .transpose()?;
        let rows = self
            .sync_sessions
            .list_recent(&scope, after, limit.saturating_add(1))
            .await?;

        keyset_page(rows, limit, |last| {
            encode_cursor(
                SESSIONS_ORDER,
                SortDir::Desc,
                vec![last.created_at.clone(), last.id.to_string()],
            )
        })
    }

    /// Cheap DB reachability probe for the platform's readiness aggregation
    /// (`RestApiCapability::healthcheck`): acquiring a pooled connection, no
    /// query, so a routine `/readyz` poll costs nothing beyond a pool-handle
    /// acquisition.
    #[must_use]
    pub(crate) fn db_reachable(&self) -> bool {
        self.db.conn().is_ok()
    }

    /// Sync one repository from GitHub into the mirror: the 5-phase runner
    /// (DESIGN §4) under the per-repo advisory lock, then the deletion pass.
    ///
    /// Every task writes its own transaction, so a run that stops partway
    /// leaves the tables consistent and the next run picks up where it left
    /// off (ADR-0001: resume by re-running, no persisted task state).
    ///
    /// # Errors
    /// `DomainError::NotFound` when GitHub does not know the repository,
    /// `Conflict` when a sync of it is already running, `Forbidden` on PDP
    /// denial, `Internal` when tasks failed or the run outlived its budget.
    pub async fn sync_repository(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        options: &FetchOptions,
        progress: &SyncProgress,
        cancel: &CancellationToken,
    ) -> Result<SyncSummary, DomainError> {
        let options = FetchOptions {
            access_scope: self.sync_access_scope(ctx).await?,
            ..options.clone()
        };
        self.sync_repository_scoped(ctx, owner, name, &options, progress, cancel)
            .await
    }

    async fn sync_repository_scoped(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        options: &FetchOptions,
        progress: &SyncProgress,
        cancel: &CancellationToken,
    ) -> Result<SyncSummary, DomainError> {
        // Checked here and not only in the REST handler: the tests call this
        // directly, and `owner`/`name` reach a log line below, where a newline
        // would forge a record of its own.
        validate_repo_path(owner, name)?;
        options.scope.validate()?;

        let tenant_id = ctx.subject_tenant_id();

        let lock_key = format!("sync/{tenant_id}/{owner}/{name}");
        let sync_lock = match self.db.db().lock(GEAR_NAME, &lock_key).await {
            Ok(guard) => guard,
            Err(toolkit_db::DbError::Lock(toolkit_db::DbLockError::AlreadyHeld { .. })) => {
                // Logged as well as returned: on the file-marker backend the
                // library takes no lock back automatically (a TTL or a PID
                // check can steal a live one), so a marker left by a killed
                // process keeps answering 409 until someone removes it. The
                // key is what an operator needs to find it.
                // The repository, not the composite key: the key carries the
                // tenant id, which is an authorization identifier and not
                // something every log consumer needs to see.
                tracing::warn!(
                    owner,
                    name,
                    "sync lock already held; a repeated 409 with no sync                      running means a stale marker for this repository"
                );
                return Err(DomainError::Conflict(format!(
                    "a sync for {owner}/{name} is already running"
                )));
            }
            Err(e) => return Err(DomainError::Database(e)),
        };

        let run = Arc::new(RunState::new(
            Uuid::new_v4(),
            options.access_scope.clone(),
            tenant_id,
            owner,
            name,
            FetchOptions {
                cancel: cancel.clone(),
                ..options.clone()
            },
        ));
        let outcome = self.run_phases(&run, progress, cancel).await;

        // Deterministic unlock on the way out; a failed release is only
        // logged — the guard's Drop already queued a best-effort release,
        // and the sync itself succeeded or failed on its own merits.
        release_sync_lock(sync_lock, owner, name).await;
        outcome
    }

    /// The part of a sync that runs under the lock: phases, then reconciliation.
    async fn run_phases(
        &self,
        run: &Arc<RunState>,
        progress: &SyncProgress,
        cancel: &CancellationToken,
    ) -> Result<SyncSummary, DomainError> {
        // Captured before any row is written: every upsert in this sync stamps
        // `extracted_at` with a later instant, so "extracted_at < watermark"
        // identifies exactly the rows this sync did not touch.
        let watermark = Utc::now();

        let worker: Arc<dyn Worker> = Arc::new(MirrorWorker::new(
            Arc::clone(&self.github),
            Arc::clone(&self.sync_writer),
            Arc::clone(&self.change_gate),
            Arc::clone(&self.sweep_watermark),
            Arc::clone(&self.pull_requests),
            Arc::clone(run),
            self.config.scope,
        ));
        let runner = RepoPhaseRunner::new(
            vec![worker],
            run.identity(),
            self.config.max_concurrent_tasks,
            cancel.child_token(),
            progress.handle(),
        );
        let mut report = runner.run().await;
        let contributors = run.take_contributors();
        let contributors_synced = if contributors.is_empty() {
            0
        } else {
            self.sync_writer
                .write_contributors(&run.scope, run.tenant_id, run.repo_id()?, contributors)
                .await?
        };
        // Discovery failing is the repository failing: GitHub's own answer
        // (404, 403 ...) is the sync's outcome, not a task statistic.
        if let Some(discovery) = report
            .failures
            .iter()
            .position(|failure| failure.kind == Some(TaskKind::Discover))
        {
            return Err(report.failures.swap_remove(discovery).error);
        }
        if report.cancelled
            || report
                .failures
                .iter()
                .any(|failure| matches!(failure.error, DomainError::Cancelled))
        {
            return Err(DomainError::Cancelled);
        }
        if !report.failures.is_empty() {
            let shown: Vec<String> = report
                .failures
                .iter()
                .take(MAX_REPORTED_FAILURES)
                .map(TaskFailure::public_text)
                .collect();
            let more = report.failures.len().saturating_sub(shown.len());
            let rest = if more > 0 {
                format!("; and {more} more")
            } else {
                String::new()
            };
            let message = format!(
                "{} of {} sync tasks failed: {}{rest}",
                report.tasks_failed(),
                report.tasks_done + report.tasks_failed(),
                shown.join("; ")
            );
            if report
                .failures
                .iter()
                .any(|failure| matches!(failure.error, DomainError::AccessLost(_)))
            {
                return Err(DomainError::AccessLost(message));
            }
            return Err(DomainError::internal(message));
        }

        for family in Family::SWEPT {
            if let Some(end) = run.swept_end(family) {
                self.sweep_watermark
                    .promote(
                        &run.scope,
                        run.tenant_id,
                        run.repo_id()?,
                        family,
                        end.page1_etag,
                        end.head_sha,
                    )
                    .await?;
            }
        }

        let stale_rows_deleted = self
            .sync_writer
            .reconcile_stale(&run.scope, run.repo_id()?, &run.completeness(), watermark)
            .await?;
        if stale_rows_deleted > 0 {
            tracing::info!(
                repository = %format!("{}/{}", run.owner, run.name),
                stale_rows_deleted,
                "reconciled upstream deletions"
            );
        }
        progress.stored();

        let mut summary = run.summary();
        summary.contributors_synced = contributors_synced;
        summary.stale_rows_deleted = stale_rows_deleted;
        let drift = run.drift();
        summary.accepted_drift_total = u64::try_from(drift.len()).unwrap_or(u64::MAX);
        summary.accepted_drift = drift.into_iter().take(MAX_REPORTED_DRIFT).collect();
        Ok(summary)
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a panic in these tests is the failure report"
)]
mod service_tests;
