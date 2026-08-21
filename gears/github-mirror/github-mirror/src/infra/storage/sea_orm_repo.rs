use async_trait::async_trait;
use github_mirror_sdk::{
    Comment, Commit, Issue, Label, Milestone, PullRequest, Release, Repository, Review,
    ReviewComment,
};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, Order};
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    CommentRecord, CommentRepository, CommitRecord, CommitRepository, IssueRecord, IssueRepository,
    LabelRecord, LabelRepository, MilestoneRecord, MilestoneRepository, PullRequestRecord,
    PullRequestRepository, ReleaseRecord, ReleaseRepository, RepoRepository, RepositoryRecord,
    ReviewCommentRecord, ReviewCommentRepository, ReviewRecord, ReviewRepository,
};

use super::entity::comments::{self, Entity as CommentEntity};
use super::entity::commits::{self, Entity as CommitEntity};
use super::entity::issues::{self, Entity as IssueEntity};
use super::entity::labels::{self, Entity as LabelEntity};
use super::entity::milestones::{self, Entity as MilestoneEntity};
use super::entity::pull_requests::{self, Entity as PullRequestEntity};
use super::entity::releases::{self, Entity as ReleaseEntity};
use super::entity::repositories::{self, Entity as RepoEntity};
use super::entity::review_comments::{self, Entity as ReviewCommentEntity};
use super::entity::reviews::{self, Entity as ReviewEntity};

pub struct SeaOrmRepoRepository;

impl SeaOrmRepoRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmRepoRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::forbidden(msg),
        ScopeError::Invalid(msg) => DomainError::internal(format!("scope invalid: {msg}")),
        ScopeError::Db(e) => DomainError::internal(format!("database error: {e}")),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::forbidden(format!("tenant {tenant_id} not in scope"))
        }
    }
}

fn active_model(tenant_id: Uuid, r: &RepositoryRecord) -> repositories::ActiveModel {
    repositories::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        owner: ActiveValue::Set(r.owner.clone()),
        name: ActiveValue::Set(r.name.clone()),
        full_name: ActiveValue::Set(r.full_name.clone()),
        default_branch: ActiveValue::Set(r.default_branch.clone()),
        private: ActiveValue::Set(r.private),
        pushed_at: ActiveValue::Set(r.pushed_at.clone()),
        stars: ActiveValue::Set(r.stars),
        forks: ActiveValue::Set(r.forks),
        description: ActiveValue::Set(r.description.clone()),
    }
}

#[async_trait]
impl RepoRepository for SeaOrmRepoRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: RepositoryRecord,
    ) -> Result<Repository, DomainError> {
        let on_conflict = SecureOnConflict::<RepoEntity>::columns([
            repositories::Column::TenantId,
            repositories::Column::Id,
        ])
        .update_columns([
            repositories::Column::Owner,
            repositories::Column::Name,
            repositories::Column::FullName,
            repositories::Column::DefaultBranch,
            repositories::Column::Private,
            repositories::Column::PushedAt,
            repositories::Column::Stars,
            repositories::Column::Forks,
            repositories::Column::Description,
        ])
        .map_err(map_scope_error)?;

        RepoEntity::insert(active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Repository {
            id: record.id,
            owner: record.owner,
            name: record.name,
            full_name: record.full_name,
            private: record.private,
            description: record.description,
        })
    }

    async fn list<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        limit: u64,
    ) -> Result<Vec<Repository>, DomainError> {
        let rows = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .order_by(repositories::Column::FullName, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn find_by_full_name<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        full_name: &str,
    ) -> Result<Option<Repository>, DomainError> {
        let row = RepoEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(repositories::Column::FullName.eq(full_name)))
            .one(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(row.map(Into::into))
    }
}

pub struct SeaOrmIssueRepository;

impl SeaOrmIssueRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmIssueRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn issue_active_model(tenant_id: Uuid, r: &IssueRecord) -> issues::ActiveModel {
    issues::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        number: ActiveValue::Set(r.number),
        title: ActiveValue::Set(r.title.clone()),
        body: ActiveValue::Set(r.body.clone()),
        state: ActiveValue::Set(r.state.clone()),
        is_pull_request: ActiveValue::Set(r.is_pull_request),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        closed_at: ActiveValue::Set(r.closed_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl IssueRepository for SeaOrmIssueRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: IssueRecord,
    ) -> Result<Issue, DomainError> {
        let on_conflict = SecureOnConflict::<IssueEntity>::columns([
            issues::Column::TenantId,
            issues::Column::Id,
        ])
        .update_columns([
            issues::Column::RepoId,
            issues::Column::Number,
            issues::Column::Title,
            issues::Column::Body,
            issues::Column::State,
            issues::Column::IsPullRequest,
            issues::Column::CreatedAt,
            issues::Column::UpdatedAt,
            issues::Column::ClosedAt,
            issues::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        IssueEntity::insert(issue_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &issue_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Issue {
            id: record.id,
            repo_id: record.repo_id,
            number: record.number,
            title: record.title,
            body: record.body,
            state: record.state,
            is_pull_request: record.is_pull_request,
            created_at: record.created_at,
            updated_at: record.updated_at,
            closed_at: record.closed_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Issue>, DomainError> {
        let rows = IssueEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(issues::Column::RepoId.eq(repo_id)))
            .order_by(issues::Column::Number, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmPullRequestRepository;

impl SeaOrmPullRequestRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmPullRequestRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn pull_request_active_model(tenant_id: Uuid, r: &PullRequestRecord) -> pull_requests::ActiveModel {
    pull_requests::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        number: ActiveValue::Set(r.number),
        title: ActiveValue::Set(r.title.clone()),
        body: ActiveValue::Set(r.body.clone()),
        state: ActiveValue::Set(r.state.clone()),
        draft: ActiveValue::Set(r.draft),
        merged: ActiveValue::Set(r.merged),
        head_sha: ActiveValue::Set(r.head_sha.clone()),
        base_sha: ActiveValue::Set(r.base_sha.clone()),
        lines_added: ActiveValue::Set(r.lines_added),
        lines_removed: ActiveValue::Set(r.lines_removed),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        closed_at: ActiveValue::Set(r.closed_at.clone()),
        merged_at: ActiveValue::Set(r.merged_at.clone()),
    }
}

#[async_trait]
impl PullRequestRepository for SeaOrmPullRequestRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: PullRequestRecord,
    ) -> Result<PullRequest, DomainError> {
        let on_conflict = SecureOnConflict::<PullRequestEntity>::columns([
            pull_requests::Column::TenantId,
            pull_requests::Column::Id,
        ])
        .update_columns([
            pull_requests::Column::RepoId,
            pull_requests::Column::Number,
            pull_requests::Column::Title,
            pull_requests::Column::Body,
            pull_requests::Column::State,
            pull_requests::Column::Draft,
            pull_requests::Column::Merged,
            pull_requests::Column::HeadSha,
            pull_requests::Column::BaseSha,
            pull_requests::Column::LinesAdded,
            pull_requests::Column::LinesRemoved,
            pull_requests::Column::CreatedAt,
            pull_requests::Column::UpdatedAt,
            pull_requests::Column::ClosedAt,
            pull_requests::Column::MergedAt,
        ])
        .map_err(map_scope_error)?;

        PullRequestEntity::insert(pull_request_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &pull_request_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(PullRequest {
            id: record.id,
            repo_id: record.repo_id,
            number: record.number,
            title: record.title,
            body: record.body,
            state: record.state,
            draft: record.draft,
            merged: record.merged,
            head_sha: record.head_sha,
            base_sha: record.base_sha,
            lines_added: record.lines_added,
            lines_removed: record.lines_removed,
            created_at: record.created_at,
            updated_at: record.updated_at,
            closed_at: record.closed_at,
            merged_at: record.merged_at,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<PullRequest>, DomainError> {
        let rows = PullRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(pull_requests::Column::RepoId.eq(repo_id)))
            .order_by(pull_requests::Column::Number, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmCommitRepository;

impl SeaOrmCommitRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmCommitRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn commit_active_model(tenant_id: Uuid, r: &CommitRecord) -> commits::ActiveModel {
    commits::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        repo_id: ActiveValue::Set(r.repo_id),
        sha: ActiveValue::Set(r.sha.clone()),
        message: ActiveValue::Set(r.message.clone()),
        author_login: ActiveValue::Set(r.author_login.clone()),
        committer_login: ActiveValue::Set(r.committer_login.clone()),
        authored_at: ActiveValue::Set(r.authored_at.clone()),
        committed_at: ActiveValue::Set(r.committed_at.clone()),
        additions: ActiveValue::Set(r.additions),
        deletions: ActiveValue::Set(r.deletions),
    }
}

#[async_trait]
impl CommitRepository for SeaOrmCommitRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: CommitRecord,
    ) -> Result<Commit, DomainError> {
        let on_conflict = SecureOnConflict::<CommitEntity>::columns([
            commits::Column::TenantId,
            commits::Column::RepoId,
            commits::Column::Sha,
        ])
        .update_columns([
            commits::Column::Message,
            commits::Column::AuthorLogin,
            commits::Column::CommitterLogin,
            commits::Column::AuthoredAt,
            commits::Column::CommittedAt,
            commits::Column::Additions,
            commits::Column::Deletions,
        ])
        .map_err(map_scope_error)?;

        CommitEntity::insert(commit_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &commit_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Commit {
            repo_id: record.repo_id,
            sha: record.sha,
            message: record.message,
            author_login: record.author_login,
            committer_login: record.committer_login,
            authored_at: record.authored_at,
            committed_at: record.committed_at,
            additions: record.additions,
            deletions: record.deletions,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Commit>, DomainError> {
        let rows = CommitEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(commits::Column::RepoId.eq(repo_id)))
            .order_by(commits::Column::CommittedAt, Order::Desc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmCommentRepository;

impl SeaOrmCommentRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmCommentRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn comment_active_model(tenant_id: Uuid, r: &CommentRecord) -> comments::ActiveModel {
    comments::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        issue_number: ActiveValue::Set(r.issue_number),
        author_login: ActiveValue::Set(r.author_login.clone()),
        body: ActiveValue::Set(r.body.clone()),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl CommentRepository for SeaOrmCommentRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: CommentRecord,
    ) -> Result<Comment, DomainError> {
        let on_conflict = SecureOnConflict::<CommentEntity>::columns([
            comments::Column::TenantId,
            comments::Column::Id,
        ])
        .update_columns([
            comments::Column::RepoId,
            comments::Column::IssueNumber,
            comments::Column::AuthorLogin,
            comments::Column::Body,
            comments::Column::CreatedAt,
            comments::Column::UpdatedAt,
            comments::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        CommentEntity::insert(comment_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &comment_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Comment {
            id: record.id,
            repo_id: record.repo_id,
            issue_number: record.issue_number,
            author_login: record.author_login,
            body: record.body,
            created_at: record.created_at,
            updated_at: record.updated_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_issue<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        issue_number: i64,
        limit: u64,
    ) -> Result<Vec<Comment>, DomainError> {
        let rows = CommentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                sea_orm::Condition::all()
                    .add(comments::Column::RepoId.eq(repo_id))
                    .add(comments::Column::IssueNumber.eq(issue_number)),
            )
            .order_by(comments::Column::CreatedAt, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmReviewCommentRepository;

impl SeaOrmReviewCommentRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmReviewCommentRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn review_comment_active_model(
    tenant_id: Uuid,
    r: &ReviewCommentRecord,
) -> review_comments::ActiveModel {
    review_comments::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        pull_number: ActiveValue::Set(r.pull_number),
        author_login: ActiveValue::Set(r.author_login.clone()),
        body: ActiveValue::Set(r.body.clone()),
        path: ActiveValue::Set(r.path.clone()),
        diff_hunk: ActiveValue::Set(r.diff_hunk.clone()),
        in_reply_to_id: ActiveValue::Set(r.in_reply_to_id),
        commit_id: ActiveValue::Set(r.commit_id.clone()),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl ReviewCommentRepository for SeaOrmReviewCommentRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: ReviewCommentRecord,
    ) -> Result<ReviewComment, DomainError> {
        let on_conflict = SecureOnConflict::<ReviewCommentEntity>::columns([
            review_comments::Column::TenantId,
            review_comments::Column::Id,
        ])
        .update_columns([
            review_comments::Column::RepoId,
            review_comments::Column::PullNumber,
            review_comments::Column::AuthorLogin,
            review_comments::Column::Body,
            review_comments::Column::Path,
            review_comments::Column::DiffHunk,
            review_comments::Column::InReplyToId,
            review_comments::Column::CommitId,
            review_comments::Column::CreatedAt,
            review_comments::Column::UpdatedAt,
            review_comments::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        ReviewCommentEntity::insert(review_comment_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &review_comment_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(ReviewComment {
            id: record.id,
            repo_id: record.repo_id,
            pull_number: record.pull_number,
            author_login: record.author_login,
            body: record.body,
            path: record.path,
            diff_hunk: record.diff_hunk,
            in_reply_to_id: record.in_reply_to_id,
            commit_id: record.commit_id,
            created_at: record.created_at,
            updated_at: record.updated_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_pull<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        pull_number: i64,
        limit: u64,
    ) -> Result<Vec<ReviewComment>, DomainError> {
        let rows = ReviewCommentEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                sea_orm::Condition::all()
                    .add(review_comments::Column::RepoId.eq(repo_id))
                    .add(review_comments::Column::PullNumber.eq(pull_number)),
            )
            .order_by(review_comments::Column::CreatedAt, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmReviewRepository;

impl SeaOrmReviewRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmReviewRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn review_active_model(tenant_id: Uuid, r: &ReviewRecord) -> reviews::ActiveModel {
    reviews::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        pull_number: ActiveValue::Set(r.pull_number),
        author_login: ActiveValue::Set(r.author_login.clone()),
        state: ActiveValue::Set(r.state.clone()),
        body: ActiveValue::Set(r.body.clone()),
        commit_id: ActiveValue::Set(r.commit_id.clone()),
        submitted_at: ActiveValue::Set(r.submitted_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl ReviewRepository for SeaOrmReviewRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: ReviewRecord,
    ) -> Result<Review, DomainError> {
        let on_conflict = SecureOnConflict::<ReviewEntity>::columns([
            reviews::Column::TenantId,
            reviews::Column::Id,
        ])
        .update_columns([
            reviews::Column::RepoId,
            reviews::Column::PullNumber,
            reviews::Column::AuthorLogin,
            reviews::Column::State,
            reviews::Column::Body,
            reviews::Column::CommitId,
            reviews::Column::SubmittedAt,
            reviews::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        ReviewEntity::insert(review_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &review_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Review {
            id: record.id,
            repo_id: record.repo_id,
            pull_number: record.pull_number,
            author_login: record.author_login,
            state: record.state,
            body: record.body,
            commit_id: record.commit_id,
            submitted_at: record.submitted_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_pull<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        pull_number: i64,
        limit: u64,
    ) -> Result<Vec<Review>, DomainError> {
        let rows = ReviewEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                sea_orm::Condition::all()
                    .add(reviews::Column::RepoId.eq(repo_id))
                    .add(reviews::Column::PullNumber.eq(pull_number)),
            )
            .order_by(reviews::Column::Id, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmLabelRepository;

impl SeaOrmLabelRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmLabelRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn label_active_model(tenant_id: Uuid, r: &LabelRecord) -> labels::ActiveModel {
    labels::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        name: ActiveValue::Set(r.name.clone()),
        color: ActiveValue::Set(r.color.clone()),
        is_default: ActiveValue::Set(r.is_default),
        description: ActiveValue::Set(r.description.clone()),
    }
}

#[async_trait]
impl LabelRepository for SeaOrmLabelRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: LabelRecord,
    ) -> Result<Label, DomainError> {
        let on_conflict = SecureOnConflict::<LabelEntity>::columns([
            labels::Column::TenantId,
            labels::Column::Id,
        ])
        .update_columns([
            labels::Column::RepoId,
            labels::Column::Name,
            labels::Column::Color,
            labels::Column::IsDefault,
            labels::Column::Description,
        ])
        .map_err(map_scope_error)?;

        LabelEntity::insert(label_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &label_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Label {
            id: record.id,
            repo_id: record.repo_id,
            name: record.name,
            color: record.color,
            is_default: record.is_default,
            description: record.description,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Label>, DomainError> {
        let rows = LabelEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(labels::Column::RepoId.eq(repo_id)))
            .order_by(labels::Column::Name, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmMilestoneRepository;

impl SeaOrmMilestoneRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmMilestoneRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn milestone_active_model(tenant_id: Uuid, r: &MilestoneRecord) -> milestones::ActiveModel {
    milestones::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        number: ActiveValue::Set(r.number),
        title: ActiveValue::Set(r.title.clone()),
        state: ActiveValue::Set(r.state.clone()),
        description: ActiveValue::Set(r.description.clone()),
        open_issues: ActiveValue::Set(r.open_issues),
        closed_issues: ActiveValue::Set(r.closed_issues),
        due_on: ActiveValue::Set(r.due_on.clone()),
        created_at: ActiveValue::Set(r.created_at.clone()),
        updated_at: ActiveValue::Set(r.updated_at.clone()),
        closed_at: ActiveValue::Set(r.closed_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl MilestoneRepository for SeaOrmMilestoneRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: MilestoneRecord,
    ) -> Result<Milestone, DomainError> {
        let on_conflict = SecureOnConflict::<MilestoneEntity>::columns([
            milestones::Column::TenantId,
            milestones::Column::Id,
        ])
        .update_columns([
            milestones::Column::RepoId,
            milestones::Column::Number,
            milestones::Column::Title,
            milestones::Column::State,
            milestones::Column::Description,
            milestones::Column::OpenIssues,
            milestones::Column::ClosedIssues,
            milestones::Column::DueOn,
            milestones::Column::CreatedAt,
            milestones::Column::UpdatedAt,
            milestones::Column::ClosedAt,
            milestones::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        MilestoneEntity::insert(milestone_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &milestone_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Milestone {
            id: record.id,
            repo_id: record.repo_id,
            number: record.number,
            title: record.title,
            state: record.state,
            description: record.description,
            open_issues: record.open_issues,
            closed_issues: record.closed_issues,
            due_on: record.due_on,
            created_at: record.created_at,
            updated_at: record.updated_at,
            closed_at: record.closed_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Milestone>, DomainError> {
        let rows = MilestoneEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(milestones::Column::RepoId.eq(repo_id)))
            .order_by(milestones::Column::Number, Order::Asc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}

pub struct SeaOrmReleaseRepository;

impl SeaOrmReleaseRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for SeaOrmReleaseRepository {
    fn default() -> Self {
        Self::new()
    }
}

fn release_active_model(tenant_id: Uuid, r: &ReleaseRecord) -> releases::ActiveModel {
    releases::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        id: ActiveValue::Set(r.id),
        repo_id: ActiveValue::Set(r.repo_id),
        tag_name: ActiveValue::Set(r.tag_name.clone()),
        name: ActiveValue::Set(r.name.clone()),
        draft: ActiveValue::Set(r.draft),
        prerelease: ActiveValue::Set(r.prerelease),
        body: ActiveValue::Set(r.body.clone()),
        author_login: ActiveValue::Set(r.author_login.clone()),
        created_at: ActiveValue::Set(r.created_at.clone()),
        published_at: ActiveValue::Set(r.published_at.clone()),
        html_url: ActiveValue::Set(r.html_url.clone()),
    }
}

#[async_trait]
impl ReleaseRepository for SeaOrmReleaseRepository {
    async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        record: ReleaseRecord,
    ) -> Result<Release, DomainError> {
        let on_conflict = SecureOnConflict::<ReleaseEntity>::columns([
            releases::Column::TenantId,
            releases::Column::Id,
        ])
        .update_columns([
            releases::Column::RepoId,
            releases::Column::TagName,
            releases::Column::Name,
            releases::Column::Draft,
            releases::Column::Prerelease,
            releases::Column::Body,
            releases::Column::AuthorLogin,
            releases::Column::CreatedAt,
            releases::Column::PublishedAt,
            releases::Column::HtmlUrl,
        ])
        .map_err(map_scope_error)?;

        ReleaseEntity::insert(release_active_model(tenant_id, &record))
            .secure()
            .scope_with_model(scope, &release_active_model(tenant_id, &record))
            .map_err(map_scope_error)?
            .on_conflict(on_conflict)
            .exec(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(Release {
            id: record.id,
            repo_id: record.repo_id,
            tag_name: record.tag_name,
            name: record.name,
            draft: record.draft,
            prerelease: record.prerelease,
            body: record.body,
            author_login: record.author_login,
            created_at: record.created_at,
            published_at: record.published_at,
            html_url: record.html_url,
        })
    }

    async fn list_by_repo<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        repo_id: i64,
        limit: u64,
    ) -> Result<Vec<Release>, DomainError> {
        let rows = ReleaseEntity::find()
            .secure()
            .scope_with(scope)
            .filter(sea_orm::Condition::all().add(releases::Column::RepoId.eq(repo_id)))
            .order_by(releases::Column::CreatedAt, Order::Desc)
            .limit(limit)
            .all(conn)
            .await
            .map_err(map_scope_error)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }
}
