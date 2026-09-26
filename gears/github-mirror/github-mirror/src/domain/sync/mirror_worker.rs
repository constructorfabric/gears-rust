//! The gear's [`Worker`]: one implementation handling every phase, fetching
//! through the GitHub port and writing through the sync writer.
//!
//! Discovery fetches the repository row and seeds one Indexing task per
//! enabled family. Each Indexing task walks its family's listings, writes
//! them, and seeds one Refinement task per entity that has per-entity
//! sub-resources to fetch. Each Refinement task fetches and writes exactly one
//! entity's detail, in its own transaction, so a run interrupted anywhere
//! leaves nothing half-written.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use async_trait::async_trait;
use chrono::Utc;
use github_mirror_sdk::{CountDrift, SyncSummary};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::change_gate::{self, ChangeGate, GateInputs};
use super::sweep_watermark::{SweepWatermark, high_water, is_stale};
use super::task::{Entity, ExtractionTask, Family, NewTask, RunIdentity, TaskKind, TaskPriority};
use super::verification::{CountGap, GapOutcome, pull_gaps};
use super::worker::{Worker, WorkerContext};
use crate::domain::error::DomainError;
use crate::domain::ports::github::{
    FetchOptions, GithubPort, IssueDetailWants, ListCursor, ListingCompleteness, RepoRef,
};
use crate::domain::repo::{
    CommitRecord, ContributorRecord, IssueRecord, PullRequestRecord, PullRequestRepository,
    SyncWriter, WorkflowRunRecord,
};
use crate::domain::scope::CollectionMode;

/// Everything the tasks of one run share.
///
/// `repo_id` is learned by Discovery and read by every later task; the
/// completeness flags and the summary are accumulated as tasks finish and
/// read by the service once the run is over.
pub struct RunState {
    pub session_id: Uuid,
    pub scope: AccessScope,
    pub tenant_id: Uuid,
    pub owner: String,
    pub name: String,
    pub options: FetchOptions,
    repo_id: OnceLock<i64>,
    complete: Mutex<ListingCompleteness>,
    swept: Mutex<HashMap<Family, Option<String>>>,
    summary: Mutex<SyncSummary>,
    drift: Mutex<Vec<CountDrift>>,
    contributors: Mutex<HashMap<i64, ContributorRecord>>,
    /// The size of the last count gap seen per pull request and entity type,
    /// so a repair pass can tell a shrinking gap from one GitHub will not
    /// close.
    gap_sizes: Mutex<HashMap<(i64, String), u64>>,
}

impl RunState {
    #[must_use]
    pub fn new(
        session_id: Uuid,
        scope: AccessScope,
        tenant_id: Uuid,
        owner: &str,
        name: &str,
        options: FetchOptions,
    ) -> Self {
        Self {
            session_id,
            scope,
            tenant_id,
            owner: owner.to_owned(),
            name: name.to_owned(),
            options,
            repo_id: OnceLock::new(),
            complete: Mutex::new(ListingCompleteness::none()),
            swept: Mutex::new(HashMap::new()),
            summary: Mutex::new(SyncSummary {
                repository: format!("{owner}/{name}"),
                ..SyncSummary::default()
            }),
            drift: Mutex::new(Vec::new()),
            contributors: Mutex::new(HashMap::new()),
            gap_sizes: Mutex::new(HashMap::new()),
        }
    }

    /// GitHub's id for the repository, once Discovery has run.
    ///
    /// # Errors
    /// `Internal` when asked before Discovery — a scheduling bug, since every
    /// other phase is seeded by it.
    pub fn repo_id(&self) -> Result<i64, DomainError> {
        self.repo_id
            .get()
            .copied()
            .ok_or_else(|| DomainError::internal("repository was not discovered before indexing"))
    }

    #[must_use]
    pub fn identity(&self) -> RunIdentity {
        RunIdentity {
            session_id: self.session_id,
            tenant_id: self.tenant_id,
        }
    }

    /// # Errors
    /// As [`Self::repo_id`]: the repository must have been discovered.
    pub fn repo_ref(&self) -> Result<RepoRef<'_>, DomainError> {
        Ok(RepoRef {
            owner: &self.owner,
            name: &self.name,
            repo_id: self.repo_id()?,
        })
    }

    /// Which listings this run walked to their end.
    #[must_use]
    pub fn completeness(&self) -> ListingCompleteness {
        self.complete
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Row counts so far, in the shape the session records.
    #[must_use]
    pub fn summary(&self) -> SyncSummary {
        self.summary
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn mark_complete(&self, complete: &ListingCompleteness) {
        self.complete
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .absorb(complete);
    }

    /// Record that `family`'s sweep reached its last page, so its watermark
    /// may be promoted. Independent of [`Self::completeness`]: a walk bounded
    /// by `updated_after` saw everything it asked for without seeing everything there
    /// is, so it may advance the watermark but not drive reconciliation.
    fn mark_swept(&self, family: Family, page1_etag: Option<String>) {
        self.swept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(family, page1_etag);
    }

    #[must_use]
    pub fn is_swept(&self, family: Family) -> bool {
        self.swept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&family)
    }

    #[must_use]
    pub fn swept_page1_etag(&self, family: Family) -> Option<String> {
        self.swept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&family)
            .cloned()
            .flatten()
    }

    pub fn accept_drift(&self, drift: CountDrift) {
        self.drift
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(drift);
    }

    #[must_use]
    pub fn drift(&self) -> Vec<CountDrift> {
        self.drift
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn absorb_contributors(&self, records: Vec<ContributorRecord>) {
        let mut known = self
            .contributors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for record in records {
            match known.entry(record.user_id) {
                Entry::Vacant(slot) => {
                    slot.insert(record);
                }
                Entry::Occupied(mut slot) => slot.get_mut().absorb(record),
            }
        }
    }

    /// Record how wide `entity_type`'s gap on this pull request is now, and
    /// answer with how wide it was on the pass before.
    fn note_gap(&self, pull_number: i64, entity_type: &str, size: u64) -> Option<u64> {
        self.gap_sizes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((pull_number, entity_type.to_owned()), size)
    }

    #[must_use]
    pub fn take_contributors(&self) -> Vec<ContributorRecord> {
        let mut known = self
            .contributors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::take(&mut *known).into_values().collect()
    }

    fn tally(&self, add: impl FnOnce(&mut SyncSummary)) {
        add(&mut self.summary.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// The worker behind every task of one repository sync.
pub struct MirrorWorker {
    github: Arc<dyn GithubPort>,
    writer: Arc<dyn SyncWriter>,
    gate: Arc<ChangeGate>,
    watermark: Arc<SweepWatermark>,
    pull_requests: Arc<dyn PullRequestRepository>,
    run: Arc<RunState>,
}

impl MirrorWorker {
    #[must_use]
    pub fn new(
        github: Arc<dyn GithubPort>,
        writer: Arc<dyn SyncWriter>,
        gate: Arc<ChangeGate>,
        watermark: Arc<SweepWatermark>,
        pull_requests: Arc<dyn PullRequestRepository>,
        run: Arc<RunState>,
    ) -> Self {
        Self {
            github,
            writer,
            gate,
            watermark,
            pull_requests,
            run,
        }
    }

    async fn seed_refinements(
        &self,
        ctx: &WorkerContext,
        entity: Entity,
        candidates: Vec<RefinementCandidate>,
    ) -> Result<(), DomainError> {
        if candidates.is_empty() {
            return Ok(());
        }
        let run = &self.run;
        let items: Vec<(&str, &GateInputs)> = candidates
            .iter()
            .map(|candidate| (candidate.entity_id.as_str(), &candidate.inputs))
            .collect();
        let reasons = self
            .gate
            .evaluate_page(
                &run.scope,
                run.tenant_id,
                run.repo_id()?,
                entity,
                &items,
                Utc::now(),
                run.options.force,
            )
            .await?;
        for (candidate, reason) in candidates.into_iter().zip(reasons) {
            let Some(reason) = reason else {
                continue;
            };
            tracing::debug!(
                entity = %entity,
                entity_id = %candidate.entity_id,
                reason = reason.as_str(),
                "refining"
            );
            self.seed(
                ctx,
                TaskKind::Refine(entity),
                Some(candidate.entity_id),
                candidate.priority,
            );
        }
        Ok(())
    }

    async fn mark_refined(&self, entity: Entity, entity_id: &str) -> Result<(), DomainError> {
        let run = &self.run;
        self.gate
            .mark_refined(
                &run.scope,
                run.tenant_id,
                run.repo_id()?,
                entity,
                entity_id,
                Utc::now(),
            )
            .await
    }

    fn seed(
        &self,
        ctx: &WorkerContext,
        kind: TaskKind,
        entity_id: Option<String>,
        priority: TaskPriority,
    ) {
        self.seed_attempt(ctx, kind, entity_id, priority, 0);
    }

    fn seed_attempt(
        &self,
        ctx: &WorkerContext,
        kind: TaskKind,
        entity_id: Option<String>,
        priority: TaskPriority,
        attempt: u32,
    ) {
        ctx.queue.enqueue_task(&NewTask {
            run: self.run.identity(),
            kind,
            entity_id,
            priority,
            attempt,
        });
    }

    async fn discover(&self, ctx: &WorkerContext) -> Result<(), DomainError> {
        let run = &self.run;
        let repository = self
            .github
            .fetch_repository_metadata(&run.owner, &run.name, &run.options)
            .await?;
        let stored = self
            .writer
            .write_repository(&run.scope, run.tenant_id, repository)
            .await?;
        run.repo_id.set(stored.id).map_err(|_| {
            DomainError::internal("the repository was discovered twice in one sync")
        })?;
        run.tally(|s| s.repository.clone_from(&stored.full_name));

        let objects = run.options.scope.objects;
        let seeds = [
            (
                Family::PullRequests,
                objects.pull_requests,
                TaskPriority::OPEN_PR,
            ),
            (Family::Issues, objects.issues, TaskPriority::OPEN_ISSUE),
            (Family::Commits, objects.commits, TaskPriority::GLOBAL),
            (
                Family::Metadata,
                objects.labels || objects.milestones || objects.releases || objects.branches,
                TaskPriority::GLOBAL,
            ),
            (
                Family::Actions,
                objects.github_actions,
                TaskPriority::GLOBAL,
            ),
        ];
        for (family, enabled, priority) in seeds {
            if enabled {
                self.seed(ctx, TaskKind::Index(family), None, priority);
            }
        }
        Ok(())
    }

    async fn index_issues(&self, ctx: &WorkerContext) -> Result<(), DomainError> {
        let run = &self.run;
        let repo_id = run.repo_id()?;
        let start = self
            .watermark
            .start_sweep(&run.scope, repo_id, Family::Issues, run.options.force)
            .await?;
        let updated_after = start.updated_after;
        let collection = run.options.scope.collection;
        let mut high = updated_after;
        let mut page1_etag: Option<String> = None;
        let mut swept: HashSet<i64> = HashSet::new();
        let mut continue_from: Option<String> = None;

        while !ctx.cancel.is_cancelled() {
            let mut listing = self
                .github
                .list_issues(
                    run.repo_ref()?,
                    ListCursor {
                        updated_after,
                        page1_etag: start.page1_etag.as_deref(),
                        continue_from: continue_from.as_deref(),
                    },
                    &run.options,
                )
                .await?;
            run.mark_complete(&listing.complete);
            if page1_etag.is_none() {
                page1_etag.clone_from(&listing.page1_etag);
            }
            if listing.swept_to_end {
                run.mark_swept(Family::Issues, page1_etag.clone());
            }
            let seen: Vec<&str> = listing
                .issues
                .iter()
                .map(|i| i.updated_at.as_str())
                .collect();
            high = high_water(&seen, high);
            if listing.unchanged {
                return Ok(());
            }

            let mut candidates = Vec::new();
            for issue in &listing.issues {
                if !swept.insert(issue.number) || is_stale(Some(&issue.updated_at), updated_after) {
                    continue;
                }
                let open = issue.state == "open";
                if !collection.wants_issue_detail(open) {
                    continue;
                }
                candidates.push(RefinementCandidate {
                    entity_id: issue.number.to_string(),
                    inputs: issue_inputs(issue),
                    priority: if open {
                        TaskPriority::OPEN_ISSUE
                    } else {
                        TaskPriority::CLOSED_ISSUE
                    },
                });
            }
            self.seed_refinements(ctx, Entity::Issue, candidates)
                .await?;

            run.absorb_contributors(std::mem::take(&mut listing.contributors));
            let (issues, comments, events) = (
                count(&listing.issues),
                count(&listing.comments),
                count(&listing.issue_events),
            );
            let next = listing.next.clone();
            self.writer
                .write_issue_listing(&run.scope, run.tenant_id, repo_id, listing)
                .await?;
            run.tally(|s| {
                s.issues_synced += issues;
                s.comments_synced += comments;
                s.issue_events_synced += events;
            });
            match next {
                Some(next) => continue_from = Some(next),
                None => break,
            }
        }

        self.watermark
            .stage(&run.scope, run.tenant_id, repo_id, Family::Issues, high)
            .await?;
        Ok(())
    }

    async fn refine_issue(&self, task: &ExtractionTask) -> Result<(), DomainError> {
        let run = &self.run;
        let repo_id = run.repo_id()?;
        let number = entity_number(task)?;
        let open = task.priority.is_open_tier();
        let collection = run.options.scope.collection;

        let wants = IssueDetailWants {
            reactions: collection.reactions.includes(open),
            timeline: collection.timeline.includes(open),
        };
        let detail = self
            .github
            .refine_issue(run.repo_ref()?, number, wants, &run.options)
            .await?;
        let (reactions, timeline) = (count(&detail.reactions), count(&detail.timeline));
        self.writer
            .write_issue_detail(&run.scope, run.tenant_id, repo_id, detail)
            .await?;
        run.tally(|s| {
            s.issue_reactions_synced += reactions;
            s.issue_timeline_synced += timeline;
        });
        self.mark_refined(Entity::Issue, &number.to_string()).await
    }

    async fn index_pull_requests(&self, ctx: &WorkerContext) -> Result<(), DomainError> {
        let run = &self.run;
        let repo_id = run.repo_id()?;
        let start = self
            .watermark
            .start_sweep(&run.scope, repo_id, Family::PullRequests, run.options.force)
            .await?;
        let updated_after = start.updated_after;
        let mut high = updated_after;
        let mut page1_etag: Option<String> = None;
        let mut swept: HashSet<i64> = HashSet::new();
        let mut continue_from: Option<String> = None;

        while !ctx.cancel.is_cancelled() {
            let mut listing = self
                .github
                .list_pull_requests(
                    run.repo_ref()?,
                    ListCursor {
                        updated_after,
                        page1_etag: start.page1_etag.as_deref(),
                        continue_from: continue_from.as_deref(),
                    },
                    &run.options,
                )
                .await?;
            run.mark_complete(&listing.complete);
            if page1_etag.is_none() {
                page1_etag.clone_from(&listing.page1_etag);
            }
            if listing.swept_to_end {
                run.mark_swept(Family::PullRequests, page1_etag.clone());
            }
            let seen: Vec<&str> = listing
                .pull_requests
                .iter()
                .map(|p| p.updated_at.as_str())
                .collect();
            high = high_water(&seen, high);
            if listing.unchanged {
                return Ok(());
            }

            let mut candidates = Vec::new();
            for pull in &listing.pull_requests {
                if !swept.insert(pull.number) || is_stale(Some(&pull.updated_at), updated_after) {
                    continue;
                }
                candidates.push(RefinementCandidate {
                    entity_id: pull.number.to_string(),
                    inputs: pull_inputs(pull),
                    priority: if pull.state == "open" {
                        TaskPriority::OPEN_PR
                    } else {
                        TaskPriority::CLOSED_PR
                    },
                });
            }
            self.seed_refinements(ctx, Entity::PullRequest, candidates)
                .await?;

            run.absorb_contributors(std::mem::take(&mut listing.contributors));
            let (pulls, comments) = (
                count(&listing.pull_requests),
                count(&listing.review_comments),
            );
            let next = listing.next.clone();
            self.writer
                .write_pull_listing(&run.scope, run.tenant_id, repo_id, listing)
                .await?;
            run.tally(|s| {
                s.pull_requests_synced += pulls;
                s.review_comments_synced += comments;
            });
            match next {
                Some(next) => continue_from = Some(next),
                None => break,
            }
        }

        self.watermark
            .stage(
                &run.scope,
                run.tenant_id,
                repo_id,
                Family::PullRequests,
                high,
            )
            .await?;
        Ok(())
    }

    async fn refine_pull_request(
        &self,
        ctx: &WorkerContext,
        task: &ExtractionTask,
    ) -> Result<(), DomainError> {
        let run = &self.run;
        let repo_id = run.repo_id()?;
        let number = entity_number(task)?;
        let mut detail = self
            .github
            .refine_pull_request(run.repo_ref()?, number, &run.options)
            .await?;
        let gaps = pull_gaps(&detail);
        let threads_complete = detail.review_threads_complete;
        run.absorb_contributors(std::mem::take(&mut detail.contributors));
        let (reviews, files, commits, threads) = (
            count(&detail.reviews),
            count(&detail.files),
            count(&detail.commits),
            count(&detail.review_threads),
        );
        self.writer
            .write_pull_detail(&run.scope, run.tenant_id, repo_id, detail)
            .await?;
        run.tally(|s| {
            s.reviews_synced += reviews;
            s.pull_request_files_synced += files;
            s.pull_request_commits_synced += commits;
            s.review_threads_synced += threads;
        });

        for gap in &gaps {
            self.report_gap(ctx, number, gap, task.attempt);
        }
        if !threads_complete {
            tracing::warn!(
                pull = number,
                "the pull request keeps its unrefined mark: its review threads are \
                 short, so the next run comes back to it"
            );
            return Ok(());
        }
        self.mark_refined(Entity::PullRequest, &number.to_string())
            .await
    }

    fn report_gap(&self, ctx: &WorkerContext, number: i64, gap: &CountGap, attempt: u32) {
        let previous_gap = self.run.note_gap(number, &gap.entity_type, gap.size());
        let gap = CountGap {
            repair_attempts: attempt,
            previous_gap,
            ..gap.clone()
        };
        match gap.outcome() {
            GapOutcome::Complete => {}
            GapOutcome::Repair => {
                tracing::debug!(
                    pull = number,
                    entity_type = %gap.entity_type,
                    expected = gap.expected,
                    stored = gap.stored,
                    attempt = attempt + 1,
                    "repairing a short pull request walk"
                );
                self.seed_attempt(
                    ctx,
                    TaskKind::Verify(Entity::PullRequest),
                    Some(number.to_string()),
                    TaskPriority::HIGH,
                    attempt + 1,
                );
            }
            GapOutcome::AcceptedDrift => {
                self.run.accept_drift(CountDrift {
                    entity_type: gap.entity_type.clone(),
                    pull_number: number,
                    expected: gap.expected,
                    stored: gap.stored,
                    passes: attempt,
                });
                tracing::warn!(
                    pull = number,
                    entity_type = %gap.entity_type,
                    expected = gap.expected,
                    stored = gap.stored,
                    passes = attempt,
                    "accepting a pull request count gap GitHub will not serve"
                );
            }
        }
    }

    async fn index_commits(&self, ctx: &WorkerContext) -> Result<(), DomainError> {
        let run = &self.run;
        let repo_id = run.repo_id()?;
        let start = self
            .watermark
            .start_sweep(&run.scope, repo_id, Family::Commits, run.options.force)
            .await?;
        let updated_after = start.updated_after;
        let with_ci = run.options.scope.collection.actions == CollectionMode::All;
        let mut high = updated_after;
        let mut page1_etag: Option<String> = None;
        let mut swept: HashSet<String> = HashSet::new();
        let mut continue_from: Option<String> = None;

        while !ctx.cancel.is_cancelled() {
            let mut listing = self
                .github
                .list_commits(
                    run.repo_ref()?,
                    ListCursor {
                        updated_after,
                        page1_etag: start.page1_etag.as_deref(),
                        continue_from: continue_from.as_deref(),
                    },
                    &run.options,
                )
                .await?;
            run.mark_complete(&listing.complete);
            if page1_etag.is_none() {
                page1_etag.clone_from(&listing.page1_etag);
            }
            if listing.swept_to_end {
                run.mark_swept(Family::Commits, page1_etag.clone());
            }
            let seen: Vec<&str> = listing
                .commits
                .iter()
                .filter_map(|c| c.committed_at.as_deref())
                .collect();
            high = high_water(&seen, high);
            if listing.unchanged {
                return Ok(());
            }

            let mut candidates = Vec::new();
            for commit in &listing.commits {
                if !swept.insert(commit.sha.clone())
                    || is_stale(commit.committed_at.as_deref(), updated_after)
                {
                    continue;
                }
                candidates.push(RefinementCandidate {
                    entity_id: commit.sha.clone(),
                    inputs: commit_inputs(commit, with_ci),
                    priority: TaskPriority::NORMAL,
                });
            }
            self.seed_refinements(ctx, Entity::Commit, candidates)
                .await?;

            run.absorb_contributors(std::mem::take(&mut listing.contributors));
            let (commits, comments) = (count(&listing.commits), count(&listing.commit_comments));
            let next = listing.next.clone();
            self.writer
                .write_commit_listing(&run.scope, run.tenant_id, repo_id, listing)
                .await?;
            run.tally(|s| {
                s.commits_synced += commits;
                s.commit_comments_synced += comments;
            });
            match next {
                Some(next) => continue_from = Some(next),
                None => break,
            }
        }

        self.watermark
            .stage(&run.scope, run.tenant_id, repo_id, Family::Commits, high)
            .await?;
        Ok(())
    }

    async fn refine_commit(&self, task: &ExtractionTask) -> Result<(), DomainError> {
        let run = &self.run;
        let sha = task
            .entity_id
            .as_deref()
            .ok_or_else(|| DomainError::internal("commit task without a SHA"))?;
        let with_ci = run.options.scope.collection.actions == CollectionMode::All;
        let detail = self
            .github
            .refine_commit(run.repo_ref()?, sha, with_ci, &run.options)
            .await?;
        let (files, statuses, checks) = (
            count(&detail.files),
            count(&detail.statuses),
            count(&detail.check_runs),
        );
        self.writer
            .write_commit_detail(&run.scope, run.tenant_id, detail)
            .await?;
        run.tally(|s| {
            s.commit_files_synced += files;
            s.commit_statuses_synced += statuses;
            s.check_runs_synced += checks;
        });
        self.mark_refined(Entity::Commit, sha).await
    }

    async fn index_metadata(&self) -> Result<(), DomainError> {
        let run = &self.run;
        let listing = self
            .github
            .list_metadata(run.repo_ref()?, &run.options)
            .await?;
        run.mark_complete(&listing.complete);
        let (labels, milestones, releases, branches, tags) = (
            count(&listing.labels),
            count(&listing.milestones),
            count(&listing.releases),
            count(&listing.branches),
            count(&listing.tags),
        );
        self.writer
            .write_metadata_listing(&run.scope, run.tenant_id, listing)
            .await?;
        run.tally(|s| {
            s.labels_synced += labels;
            s.milestones_synced += milestones;
            s.releases_synced += releases;
            s.branches_synced += branches;
            s.tags_synced += tags;
        });
        Ok(())
    }

    async fn open_pull_heads(&self, mode: CollectionMode) -> Result<HashSet<String>, DomainError> {
        if mode != CollectionMode::Open {
            return Ok(HashSet::new());
        }
        let run = &self.run;
        let heads = self
            .pull_requests
            .open_head_shas(&run.scope, run.repo_id()?)
            .await?;
        Ok(heads.into_iter().collect())
    }

    async fn index_actions(&self, ctx: &WorkerContext) -> Result<(), DomainError> {
        let run = &self.run;
        let listing = self
            .github
            .list_actions(run.repo_ref()?, &run.options)
            .await?;

        let mode = run.options.scope.collection.actions;
        let open_heads = self.open_pull_heads(mode).await?;
        let candidates = listing
            .workflow_runs
            .iter()
            .filter(|workflow_run| {
                mode == CollectionMode::All || open_heads.contains(&workflow_run.head_sha)
            })
            .map(|workflow_run| RefinementCandidate {
                entity_id: workflow_run.id.to_string(),
                inputs: workflow_run_inputs(workflow_run),
                priority: TaskPriority::NORMAL,
            })
            .collect();
        self.seed_refinements(ctx, Entity::WorkflowRun, candidates)
            .await?;

        let (runs, deployments) = (count(&listing.workflow_runs), count(&listing.deployments));
        self.writer
            .write_actions_listing(&run.scope, run.tenant_id, listing)
            .await?;
        run.tally(|s| {
            s.workflow_runs_synced += runs;
            s.deployments_synced += deployments;
        });
        Ok(())
    }

    async fn refine_workflow_run(&self, task: &ExtractionTask) -> Result<(), DomainError> {
        let run = &self.run;
        let run_id = entity_number(task)?;
        let jobs = self
            .github
            .refine_workflow_run(run.repo_ref()?, run_id, &run.options)
            .await?;
        let count = count(&jobs);
        self.writer
            .write_workflow_jobs(&run.scope, run.tenant_id, jobs)
            .await?;
        run.tally(|s| s.workflow_jobs_synced += count);
        self.mark_refined(Entity::WorkflowRun, &run_id.to_string())
            .await
    }
}

#[async_trait]
impl Worker for MirrorWorker {
    fn handles(&self, _kind: TaskKind) -> bool {
        true
    }

    async fn execute(&self, ctx: &WorkerContext, task: &ExtractionTask) -> Result<(), DomainError> {
        match task.kind {
            TaskKind::Discover => self.discover(ctx).await,
            TaskKind::Index(Family::Issues) => self.index_issues(ctx).await,
            TaskKind::Index(Family::PullRequests) => self.index_pull_requests(ctx).await,
            TaskKind::Index(Family::Commits) => self.index_commits(ctx).await,
            TaskKind::Index(Family::Metadata) => self.index_metadata().await,
            TaskKind::Index(Family::Actions) => self.index_actions(ctx).await,
            TaskKind::Refine(entity) | TaskKind::Verify(entity) => match entity {
                Entity::Issue => self.refine_issue(task).await,
                Entity::PullRequest => self.refine_pull_request(ctx, task).await,
                Entity::Commit => self.refine_commit(task).await,
                Entity::WorkflowRun => self.refine_workflow_run(task).await,
            },
        }
    }
}

struct RefinementCandidate {
    entity_id: String,
    inputs: GateInputs,
    priority: TaskPriority,
}

fn issue_inputs(issue: &IssueRecord) -> GateInputs {
    GateInputs {
        fingerprint: change_gate::fingerprint(vec![
            ("updated_at", issue.updated_at.clone()),
            ("state", issue.state.clone()),
            ("closed_at", issue.closed_at.clone().unwrap_or_default()),
            ("labels", issue.labels_json.clone().unwrap_or_default()),
            (
                "assignees",
                issue.assignees_json.clone().unwrap_or_default(),
            ),
            ("locked", issue.locked.unwrap_or(false).to_string()),
        ]),
        child_counts_hash: change_gate::child_counts_hash(&[("comments", issue.comments_count)]),
        updated_at: Some(issue.updated_at.clone()),
        node_id: issue.node_id.clone(),
        terminal: issue.state != "open",
    }
}

fn pull_inputs(pull: &PullRequestRecord) -> GateInputs {
    GateInputs {
        fingerprint: change_gate::fingerprint(vec![
            ("updated_at", pull.updated_at.clone()),
            ("state", pull.state.clone()),
            ("draft", pull.draft.to_string()),
            ("merged", pull.merged.to_string()),
            ("merged_at", pull.merged_at.clone().unwrap_or_default()),
            ("closed_at", pull.closed_at.clone().unwrap_or_default()),
            ("head_sha", pull.head_sha.clone().unwrap_or_default()),
            ("base_sha", pull.base_sha.clone().unwrap_or_default()),
            ("labels", pull.labels_json.clone().unwrap_or_default()),
            ("assignees", pull.assignees_json.clone().unwrap_or_default()),
            (
                "reviewers",
                pull.requested_reviewers_json.clone().unwrap_or_default(),
            ),
        ]),
        child_counts_hash: change_gate::child_counts_hash(&[("comments", pull.comments_count)]),
        updated_at: Some(pull.updated_at.clone()),
        node_id: pull.node_id.clone(),
        terminal: pull.state != "open",
    }
}

fn commit_inputs(commit: &CommitRecord, with_ci: bool) -> GateInputs {
    GateInputs {
        fingerprint: change_gate::fingerprint(vec![("sha", commit.sha.clone())]),
        child_counts_hash: None,
        updated_at: commit.committed_at.clone(),
        node_id: None,
        terminal: !with_ci,
    }
}

fn workflow_run_inputs(run: &WorkflowRunRecord) -> GateInputs {
    GateInputs {
        fingerprint: change_gate::fingerprint(vec![
            ("updated_at", run.updated_at.clone()),
            ("status", run.status.clone().unwrap_or_default()),
            ("conclusion", run.conclusion.clone().unwrap_or_default()),
            ("run_attempt", run.run_attempt.to_string()),
            ("head_sha", run.head_sha.clone()),
        ]),
        child_counts_hash: None,
        updated_at: Some(run.updated_at.clone()),
        node_id: None,
        terminal: run.conclusion.is_some(),
    }
}
/// A number-keyed task's `entity_id`, parsed.
fn entity_number(task: &ExtractionTask) -> Result<i64, DomainError> {
    task.entity_id
        .as_deref()
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| {
            DomainError::internal(format!(
                "{} task without a numeric entity id: {:?}",
                task.kind, task.entity_id
            ))
        })
}

fn count<T>(items: &[T]) -> u64 {
    u64::try_from(items.len()).unwrap_or(u64::MAX)
}
