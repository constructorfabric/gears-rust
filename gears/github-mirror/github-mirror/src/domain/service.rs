use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use github_mirror_sdk::{
    Branch, Comment, Commit, Issue, Label, Milestone, PullRequest, Release, Repository, Review,
    ReviewComment,
};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::{SecurityContext, pep_properties};

use super::error::DomainError;
use super::ports::github::GithubPort;
use super::repo::{
    BranchRecord, BranchRepository, CommentRecord, CommentRepository, CommitRecord,
    CommitRepository, IssueRecord, IssueRepository, LabelRecord, LabelRepository, MilestoneRecord,
    MilestoneRepository, PullRequestRecord, PullRequestRepository, ReleaseRecord,
    ReleaseRepository, RepoRepository, RepositoryRecord, ReviewCommentRecord,
    ReviewCommentRepository, ReviewRecord, ReviewRepository,
};

pub const GEAR_NAME: &str = "github-mirror";

const DEFAULT_LIST_LIMIT: u64 = 50;

pub(crate) type DbProvider = toolkit_db::DBProvider<toolkit_db::DbError>;

pub(crate) const REPOSITORY_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.repository",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const ISSUE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.issue",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const PULL_REQUEST_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.pull_request",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const COMMIT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.commit",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const COMMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.comment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const REVIEW_COMMENT_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.review_comment",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const REVIEW_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.review",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const LABEL_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.label",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const MILESTONE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.milestone",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const RELEASE_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.release",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const BRANCH_RESOURCE: ResourceType = ResourceType::from_static(
    "github_mirror.branch",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) const SYNC_RESOURCE: ResourceType =
    ResourceType::from_static("github_mirror.sync", &[pep_properties::OWNER_TENANT_ID]);

pub(crate) mod actions {
    pub const LIST: &str = "list";
    pub const UPSERT: &str = "upsert";
    pub const SYNC: &str = "sync";
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub api_base_url: String,
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct MirrorStatus {
    pub gear: String,
    pub version: String,
    pub api_base_url: String,
}

/// What one sync pass wrote, returned to the caller.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub repository: String,
    pub issues_synced: u64,
    pub pull_requests_synced: u64,
    pub commits_synced: u64,
    pub comments_synced: u64,
    pub review_comments_synced: u64,
    pub reviews_synced: u64,
    pub labels_synced: u64,
    pub milestones_synced: u64,
    pub releases_synced: u64,
    pub branches_synced: u64,
}

#[domain_model]
pub struct Service<
    R: RepoRepository,
    I: IssueRepository,
    P: PullRequestRepository,
    C: CommitRepository,
    M: CommentRepository,
    V: ReviewCommentRepository,
    W: ReviewRepository,
    L: LabelRepository,
    N: MilestoneRepository,
    E: ReleaseRepository,
    B: BranchRepository,
> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    issues: Arc<I>,
    pull_requests: Arc<P>,
    commits: Arc<C>,
    comments: Arc<M>,
    review_comments: Arc<V>,
    reviews: Arc<W>,
    labels: Arc<L>,
    milestones: Arc<N>,
    releases: Arc<E>,
    branches: Arc<B>,
    github: Arc<dyn GithubPort>,
    policy_enforcer: PolicyEnforcer,
    config: ServiceConfig,
}

impl<
    R: RepoRepository,
    I: IssueRepository,
    P: PullRequestRepository,
    C: CommitRepository,
    M: CommentRepository,
    V: ReviewCommentRepository,
    W: ReviewRepository,
    L: LabelRepository,
    N: MilestoneRepository,
    E: ReleaseRepository,
    B: BranchRepository,
> Service<R, I, P, C, M, V, W, L, N, E, B>
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        issues: Arc<I>,
        pull_requests: Arc<P>,
        commits: Arc<C>,
        comments: Arc<M>,
        review_comments: Arc<V>,
        reviews: Arc<W>,
        labels: Arc<L>,
        milestones: Arc<N>,
        releases: Arc<E>,
        branches: Arc<B>,
        github: Arc<dyn GithubPort>,
        policy_enforcer: PolicyEnforcer,
        config: ServiceConfig,
    ) -> Self {
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
            github,
            policy_enforcer,
            config,
        }
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
    pub async fn list_repositories(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Repository>, DomainError> {
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPOSITORY_RESOURCE,
                actions::LIST,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, ctx.subject_tenant_id()),
            )
            .await?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let conn = self.db.conn()?;
        let items = self.repo.list(&conn, &scope, limit).await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
        ))
    }

    /// Insert or update a mirrored repository row for the caller's tenant.
    ///
    /// # Errors
    /// Returns `DomainError::Forbidden` when the PDP denies access and
    /// `DomainError::Database`/`Internal` on storage failures.
    pub async fn upsert_repository(
        &self,
        ctx: &SecurityContext,
        record: RepositoryRecord,
    ) -> Result<Repository, DomainError> {
        let tenant_id = ctx.subject_tenant_id();

        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REPOSITORY_RESOURCE,
                actions::UPSERT,
                None,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;

        let conn = self.db.conn()?;
        self.repo.upsert(&conn, &scope, tenant_id, record).await
    }

    /// List mirrored issues of one repository (`owner/name`), tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_issues(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        query: &ODataQuery,
    ) -> Result<Page<Issue>, DomainError> {
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .issues
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = IssueRecord {
            repo_id: repository.id,
            ..record
        };
        self.issues.upsert(&conn, &scope, tenant_id, record).await
    }

    /// List mirrored pull requests of one repository (`owner/name`), tenant-scoped.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_pull_requests(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        query: &ODataQuery,
    ) -> Result<Page<PullRequest>, DomainError> {
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .pull_requests
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = PullRequestRecord {
            repo_id: repository.id,
            ..record
        };
        self.pull_requests
            .upsert(&conn, &scope, tenant_id, record)
            .await
    }

    /// List mirrored commits of one repository (`owner/name`), tenant-scoped,
    /// newest first.
    ///
    /// # Errors
    /// `DomainError::NotFound` when the repository is not mirrored for this
    /// tenant; `Forbidden`/`Database`/`Internal` as usual.
    pub async fn list_commits(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
        query: &ODataQuery,
    ) -> Result<Page<Commit>, DomainError> {
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .commits
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommitRecord {
            repo_id: repository.id,
            ..record
        };
        self.commits.upsert(&conn, &scope, tenant_id, record).await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .comments
            .list_by_issue(&conn, &scope, repository.id, issue_number, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = CommentRecord {
            repo_id: repository.id,
            ..record
        };
        self.comments.upsert(&conn, &scope, tenant_id, record).await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .review_comments
            .list_by_pull(&conn, &scope, repository.id, pull_number, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReviewCommentRecord {
            repo_id: repository.id,
            ..record
        };
        self.review_comments
            .upsert(&conn, &scope, tenant_id, record)
            .await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .reviews
            .list_by_pull(&conn, &scope, repository.id, pull_number, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReviewRecord {
            repo_id: repository.id,
            ..record
        };
        self.reviews.upsert(&conn, &scope, tenant_id, record).await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .labels
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = LabelRecord {
            repo_id: repository.id,
            ..record
        };
        self.labels.upsert(&conn, &scope, tenant_id, record).await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .milestones
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = MilestoneRecord {
            repo_id: repository.id,
            ..record
        };
        self.milestones
            .upsert(&conn, &scope, tenant_id, record)
            .await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .releases
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = ReleaseRecord {
            repo_id: repository.id,
            ..record
        };
        self.releases.upsert(&conn, &scope, tenant_id, record).await
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
        query: &ODataQuery,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        let items = self
            .branches
            .list_by_repo(&conn, &scope, repository.id, limit)
            .await?;

        Ok(Page::new(
            items,
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
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

        let conn = self.db.conn()?;
        let full_name = format!("{owner}/{name}");
        let repository = self
            .repo
            .find_by_full_name(&conn, &scope, &full_name)
            .await?
            .ok_or(DomainError::NotFound)?;

        let record = BranchRecord {
            repo_id: repository.id,
            ..record
        };
        self.branches.upsert(&conn, &scope, tenant_id, record).await
    }

    /// Fetch one repository from GitHub (first slice: repo + first page of
    /// issues, pull requests, and commits) and upsert it into the mirror.
    ///
    /// The REST face of the PRD's `sync_repo` entry point; the inline fetch
    /// is replaced by a queued sync session when the engine lands
    /// (gears-rust#4632).
    ///
    /// # Errors
    /// `DomainError::NotFound` when GitHub does not know the repository,
    /// `Forbidden` on PDP denial, `Internal` on GitHub/storage failures.
    // One linear upsert loop per mirrored table; splitting them into
    // helpers would only hide the sequence.
    #[allow(clippy::cast_possible_truncation, clippy::cognitive_complexity)]
    pub async fn sync_repository(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
    ) -> Result<SyncSummary, DomainError> {
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

        let fetched = self.github.fetch_repository(owner, name).await?;

        let conn = self.db.conn()?;
        let repository = self
            .repo
            .upsert(&conn, &scope, tenant_id, fetched.repository)
            .await?;

        let mut issues_synced: u64 = 0;
        for record in fetched.issues {
            self.issues.upsert(&conn, &scope, tenant_id, record).await?;
            issues_synced += 1;
        }

        let mut pull_requests_synced: u64 = 0;
        for record in fetched.pull_requests {
            self.pull_requests
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            pull_requests_synced += 1;
        }

        let mut commits_synced: u64 = 0;
        for record in fetched.commits {
            self.commits
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            commits_synced += 1;
        }

        let mut comments_synced: u64 = 0;
        for record in fetched.comments {
            self.comments
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            comments_synced += 1;
        }

        let mut review_comments_synced: u64 = 0;
        for record in fetched.review_comments {
            self.review_comments
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            review_comments_synced += 1;
        }

        let mut reviews_synced: u64 = 0;
        for record in fetched.reviews {
            self.reviews
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            reviews_synced += 1;
        }

        let mut labels_synced: u64 = 0;
        for record in fetched.labels {
            self.labels.upsert(&conn, &scope, tenant_id, record).await?;
            labels_synced += 1;
        }

        let mut milestones_synced: u64 = 0;
        for record in fetched.milestones {
            self.milestones
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            milestones_synced += 1;
        }

        let mut releases_synced: u64 = 0;
        for record in fetched.releases {
            self.releases
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            releases_synced += 1;
        }

        let mut branches_synced: u64 = 0;
        for record in fetched.branches {
            self.branches
                .upsert(&conn, &scope, tenant_id, record)
                .await?;
            branches_synced += 1;
        }

        Ok(SyncSummary {
            repository: repository.full_name,
            issues_synced,
            pull_requests_synced,
            commits_synced,
            comments_synced,
            review_comments_synced,
            reviews_synced,
            labels_synced,
            milestones_synced,
            releases_synced,
            branches_synced,
        })
    }
}
