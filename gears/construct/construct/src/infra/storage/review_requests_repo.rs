use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QuerySelect, SqlErr};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::odata::LimitCfg;
use toolkit_db::odata::sea_orm_filter::{
    FieldToColumn, ODataFieldMapping, PaginateOdataTryError, paginate_odata_try,
};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use toolkit_odata::{Error as ODataError, ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::DbProvider;
use crate::domain::error::DomainError;
use crate::domain::pagination::{DEFAULT_LIMIT, MAX_LIMIT, PageError, QueryError, no_select};
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::review_queue::{
    ClaimId, Claimed, Closed, FactsUnderReview, NewReviewRequest, OpenReviewField, Opened,
    RequestKey, ReviewOutcome, ReviewRequest, ReviewRequests, ReviewStatus, ReviewText,
    SubjectReviewField,
};
use crate::domain::subject_settings::SubjectKey;

use super::review_requests_entity::{self as entity, Column, Entity as ReviewRequestEntity};
use super::scope_error::map_scope_error;

/// The table's name of an open request's state.
pub(crate) const OPEN: &str = "open";
/// The table's name of a closed request's state.
pub(crate) const CLOSED: &str = "closed";

const PAGE_LIMITS: LimitCfg = LimitCfg {
    default: DEFAULT_LIMIT,
    max: MAX_LIMIT,
};

fn is_unique_violation(err: &ScopeError) -> bool {
    matches!(
        err,
        ScopeError::Db(db) if matches!(db.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
    )
}

/// What a claim is about to write, as the `pending` column holds it.
#[derive(Debug, Serialize, Deserialize)]
struct PendingRow {
    outcome: String,
    corrected_fact_id: Option<String>,
    by: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    reason: Option<String>,
}

fn pending_text(pending: &Closed) -> Result<String, DomainError> {
    serde_json::to_string(&PendingRow {
        outcome: pending.outcome.name().to_owned(),
        corrected_fact_id: pending.outcome.corrected_fact_id().map(|key| key.0.clone()),
        by: pending.by,
        at: pending.at,
        reason: pending.reason.clone().map(ReviewText::into_string),
    })
    .map_err(|err| DomainError::internal(format!("a pending resolution is not JSON: {err}")))
}

/// The resolution a row's claim recorded, if any. A record Construct cannot
/// read is a defect.
fn pending_of(row: &entity::Model) -> Result<Option<Closed>, DomainError> {
    let Some(text) = &row.pending else {
        return Ok(None);
    };
    let defect = |what: String| {
        DomainError::internal(format!("review request {} has a pending {what}", row.id))
    };
    let pending: PendingRow = serde_json::from_str(text)
        .map_err(|err| defect(format!("resolution that is not readable: {err}")))?;
    let outcome =
        ReviewOutcome::from_parts(&pending.outcome, pending.corrected_fact_id.map(NodeKey))
            .ok_or_else(|| defect(format!("outcome `{}`", pending.outcome)))?;
    let reason = ReviewText::optional(pending.reason)
        .map_err(|rule| defect(format!("reason that {}", rule.description())))?;
    Ok(Some(Closed {
        outcome,
        by: pending.by,
        at: pending.at,
        reason,
    }))
}

/// A stored row as a request. A row Construct cannot read is a defect.
fn request_of(row: entity::Model) -> Result<ReviewRequest, DomainError> {
    let id = row.id;
    let defect = |what: String| DomainError::internal(format!("review request {id} has {what}"));
    let text = |what: &str, text: Option<String>| {
        ReviewText::optional(text)
            .map_err(|rule| defect(format!("a {what} that {}", rule.description())))
    };
    let category = Category::from_name(&row.entity_kind)
        .ok_or_else(|| defect(format!("an unknown entity kind `{}`", row.entity_kind)))?;
    let comment = text("comment", row.comment)?;
    let status = match row.state.as_str() {
        OPEN => {
            let raw = row
                .value
                .ok_or_else(|| defect("no flagged value while open".to_owned()))?;
            let value = serde_json::from_str(&raw)
                .map_err(|err| defect(format!("a flagged value that is not JSON: {err}")))?;
            ReviewStatus::Open { value }
        }
        CLOSED => {
            let corrected = row.corrected_fact_id.is_some();
            let outcome = row
                .outcome
                .as_deref()
                .and_then(|name| {
                    ReviewOutcome::from_parts(name, row.corrected_fact_id.clone().map(NodeKey))
                })
                .ok_or_else(|| {
                    defect(format!(
                        "outcome {:?} with a corrected fact id {}",
                        row.outcome,
                        if corrected { "set" } else { "missing" }
                    ))
                })?;
            let (Some(by), Some(at)) = (row.resolved_by, row.resolved_at) else {
                return Err(defect("no resolver or time while closed".to_owned()));
            };
            ReviewStatus::Closed(Closed {
                outcome,
                by,
                at,
                reason: text("reason", row.reason)?,
            })
        }
        state => return Err(defect(format!("an unknown state `{state}`"))),
    };
    Ok(ReviewRequest {
        id,
        tenant_id: row.tenant_id,
        subject_id: row.subject_id,
        fact_id: NodeKey(row.fact_id),
        category,
        property: row.property,
        comment,
        created_at: row.created_at,
        status,
    })
}

/// A paging failure as the domain names it: the caller's mistake for a bad
/// filter, order or cursor, Construct's own otherwise.
fn page_error(err: PaginateOdataTryError<DomainError>) -> DomainError {
    match err {
        PaginateOdataTryError::MapError(defect) => defect,
        PaginateOdataTryError::OData(err) => DomainError::from(match err {
            ODataError::InvalidFilter(detail) => PageError::Query(QueryError::Filter(detail)),
            ODataError::InvalidOrderByField(_) | ODataError::OrderWithCursor => {
                PageError::Query(QueryError::Order(err.to_string()))
            }
            ODataError::OrderMismatch
            | ODataError::FilterMismatch
            | ODataError::InvalidCursor
            | ODataError::CursorInvalidBase64
            | ODataError::CursorInvalidJson
            | ODataError::CursorInvalidVersion
            | ODataError::CursorInvalidKeys
            | ODataError::CursorInvalidFields
            | ODataError::CursorInvalidDirection => {
                PageError::Query(QueryError::Cursor(err.to_string()))
            }
            other => PageError::Internal(other.to_string()),
        }),
    }
}

/// `query` with `default` as its order when it names none and continues no
/// cursor, which carries its own.
fn ordered<'q>(query: &'q ODataQuery, default: (&str, SortDir)) -> Cow<'q, ODataQuery> {
    if query.cursor.is_none() && query.order.is_empty() {
        let mut ordered = query.clone();
        ordered.order = ordered.order.ensure_tiebreaker(default.0, default.1);
        Cow::Owned(ordered)
    } else {
        Cow::Borrowed(query)
    }
}

fn of_subject(key: SubjectKey) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id))
        .add(Column::SubjectId.eq(key.subject_id))
}

fn of_request(key: RequestKey) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id))
        .add(Column::Id.eq(key.id))
}

fn open() -> Condition {
    Condition::all().add(Column::State.eq(OPEN))
}

/// The one column [`SeaOrmReviewRequestRepository::open_fact_ids`] reads.
#[derive(Debug, sea_orm::FromQueryResult)]
struct OpenFact {
    fact_id: String,
}

/// Which open request a close may take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseGuard {
    /// Only one held by this claim: a reviewer's resolution.
    HeldBy(ClaimId),
    /// Held or not, while what its claim recorded is still this pending
    /// text: the subject's delete.
    Recorded(Option<String>),
}

/// The review requests table. Every call runs on the runner and under the
/// scope its caller gives, so it can join a caller's transaction and never
/// reaches past the caller's authorization.
#[derive(Debug, Default, Clone, Copy)]
pub struct SeaOrmReviewRequestRepository;

impl SeaOrmReviewRequestRepository {
    /// Insert an open request; `AlreadyOpen` when the fact has one.
    ///
    /// # Errors
    ///
    /// When the row falls outside `scope` or the database fails.
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        request: NewReviewRequest,
    ) -> Result<Opened, DomainError> {
        let value = serde_json::to_string(&request.value)
            .map_err(|err| DomainError::internal(format!("a fact value is not JSON: {err}")))?;
        let row = entity::ActiveModel {
            id: ActiveValue::Set(request.id),
            tenant_id: ActiveValue::Set(request.key.tenant_id),
            subject_id: ActiveValue::Set(request.key.subject_id),
            fact_id: ActiveValue::Set(request.fact_id.0.clone()),
            entity_kind: ActiveValue::Set(request.category.name().to_owned()),
            property: ActiveValue::Set(request.property.clone()),
            value: ActiveValue::Set(Some(value)),
            comment: ActiveValue::Set(request.comment.clone().map(ReviewText::into_string)),
            state: ActiveValue::Set(OPEN.to_owned()),
            open_fact_id: ActiveValue::Set(Some(request.fact_id.0.clone())),
            outcome: ActiveValue::Set(None),
            reason: ActiveValue::Set(None),
            claim_id: ActiveValue::Set(None),
            claimed_at: ActiveValue::Set(None),
            pending: ActiveValue::Set(None),
            resolved_by: ActiveValue::Set(None),
            corrected_fact_id: ActiveValue::Set(None),
            created_at: ActiveValue::Set(request.created_at),
            resolved_at: ActiveValue::Set(None),
        };
        let inserted = ReviewRequestEntity::insert(row.clone())
            .secure()
            .scope_with_model(scope, &row)
            .map_err(map_scope_error)?
            .exec(conn)
            .await;
        match inserted {
            Ok(_) => {}
            Err(err) if is_unique_violation(&err) => return Ok(Opened::AlreadyOpen),
            Err(err) => return Err(map_scope_error(err)),
        }
        Ok(Opened::Opened(Box::new(ReviewRequest {
            id: request.id,
            tenant_id: request.key.tenant_id,
            subject_id: request.key.subject_id,
            fact_id: request.fact_id,
            category: request.category,
            property: request.property,
            comment: request.comment,
            created_at: request.created_at,
            status: ReviewStatus::Open {
                value: request.value,
            },
        })))
    }

    /// # Errors
    ///
    /// When the database fails or a row cannot be read.
    pub async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        self.find_row(conn, scope, key)
            .await?
            .map(request_of)
            .transpose()
    }

    async fn find_row<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
    ) -> Result<Option<entity::Model>, DomainError> {
        ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_request(key))
            .one(conn)
            .await
            .map_err(map_scope_error)
    }

    /// Claim the open request `key` as [`Self::claim`] does, and read what
    /// the claim holds, on the same connection: run it in a transaction to
    /// make the two one step.
    ///
    /// # Errors
    ///
    /// When the database fails or the row cannot be read.
    pub async fn claim_and_read<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        at: OffsetDateTime,
        hold: Duration,
    ) -> Result<Claimed, DomainError> {
        let claimed = self.claim(conn, scope, key, claim, at, hold).await?;
        let Some(row) = self.find_row(conn, scope, key).await? else {
            return Ok(Claimed::Closed);
        };
        let ours = claimed && row.claim_id == Some(claim.0);
        let pending = pending_of(&row)?;
        let request = request_of(row)?;
        Ok(if !request.is_open() {
            Claimed::Closed
        } else if ours {
            Claimed::Taken {
                request: Box::new(request),
                pending,
            }
        } else {
            Claimed::Held
        })
    }

    /// Set `claim` on the open request `key`, unless another claim holds it
    /// since less than `hold` before `at`. Whether it was set.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn claim<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        at: OffsetDateTime,
        hold: Duration,
    ) -> Result<bool, DomainError> {
        let free = Condition::any()
            .add(Column::ClaimedAt.is_null())
            .add(Column::ClaimedAt.lt(at - hold));
        let updated = ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(open().add(of_request(key)).add(free))
            .col_expr(Column::ClaimId, Expr::value(claim.0))
            .col_expr(Column::ClaimedAt, Expr::value(at))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(updated.rows_affected > 0)
    }

    /// Record `pending` on the open request `key` while `claim` holds it.
    /// Whether it was recorded.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn pend<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        pending: &Closed,
    ) -> Result<bool, DomainError> {
        let updated = ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(open().add(of_request(key)).add(Column::ClaimId.eq(claim.0)))
            .col_expr(Column::Pending, Expr::value(pending_text(pending)?))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(updated.rows_affected > 0)
    }

    /// Clear `claim` on the request `key`, and what it recorded.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn release<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
    ) -> Result<(), DomainError> {
        ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(open().add(of_request(key)).add(Column::ClaimId.eq(claim.0)))
            .col_expr(Column::ClaimId, Expr::value(Option::<Uuid>::None))
            .col_expr(
                Column::ClaimedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(Column::Pending, Expr::value(Option::<String>::None))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(())
    }

    /// Close the open request `key` as `closed`, clearing its flagged value,
    /// its claim and what the claim recorded, as `guard` allows. Whether it was closed.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn close<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        closed: &Closed,
        guard: CloseGuard,
    ) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-close:p1:inst-close-update
        // @cpt-begin:cpt-cf-construct-state-review-queue-request:p1:inst-state-close
        let only = open().add(of_request(key)).add(match guard {
            CloseGuard::HeldBy(claim) => Column::ClaimId.eq(claim.0),
            CloseGuard::Recorded(Some(text)) => Column::Pending.eq(text),
            CloseGuard::Recorded(None) => Column::Pending.is_null(),
        });
        let updated = ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(only)
            .col_expr(Column::State, Expr::value(CLOSED))
            .col_expr(Column::OpenFactId, Expr::value(Option::<String>::None))
            .col_expr(Column::Value, Expr::value(Option::<String>::None))
            .col_expr(Column::ClaimId, Expr::value(Option::<Uuid>::None))
            .col_expr(
                Column::ClaimedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .col_expr(Column::Pending, Expr::value(Option::<String>::None))
            .col_expr(Column::Outcome, Expr::value(closed.outcome.name()))
            .col_expr(
                Column::Reason,
                Expr::value(closed.reason.clone().map(ReviewText::into_string)),
            )
            .col_expr(Column::ResolvedBy, Expr::value(closed.by))
            .col_expr(
                Column::CorrectedFactId,
                Expr::value(closed.outcome.corrected_fact_id().map(|key| key.0.clone())),
            )
            .col_expr(Column::ResolvedAt, Expr::value(closed.at))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(updated.rows_affected > 0)
        // @cpt-end:cpt-cf-construct-state-review-queue-request:p1:inst-state-close
        // @cpt-end:cpt-cf-construct-algo-review-queue-close:p1:inst-close-update
    }

    /// The open request on the subject's fact, if there is one, with what
    /// its claim recorded.
    ///
    /// # Errors
    ///
    /// When the database fails, or the row cannot be read.
    pub async fn open_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
    ) -> Result<Option<(ReviewRequest, Option<Closed>)>, DomainError> {
        let Some(row) = ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_subject(key).add(Column::OpenFactId.eq(fact_id.0.clone())))
            .one(conn)
            .await
            .map_err(map_scope_error)?
        else {
            return Ok(None);
        };
        let recorded = pending_of(&row)?;
        Ok(Some((request_of(row)?, recorded)))
    }

    /// The facts of the subject's open requests, without reading the rest of
    /// each row.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn open_fact_ids<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<BTreeSet<NodeKey>, DomainError> {
        let ids: Vec<OpenFact> = ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_subject(key).add(open()))
            .project_all(conn, |select| {
                select
                    .select_only()
                    .column(Column::FactId)
                    .into_model::<OpenFact>()
            })
            .await
            .map_err(map_scope_error)?;
        Ok(ids.into_iter().map(|row| NodeKey(row.fact_id)).collect())
    }

    /// The subject's requests matching `also`, newest first.
    ///
    /// # Errors
    ///
    /// When the database fails or a row cannot be read.
    pub async fn of_subject<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        also: Condition,
    ) -> Result<Vec<ReviewRequest>, DomainError> {
        ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_subject(key).add(also))
            .order_by(Column::CreatedAt, sea_orm::Order::Desc)
            .order_by(Column::Id, sea_orm::Order::Desc)
            .all(conn)
            .await
            .map_err(map_scope_error)?
            .into_iter()
            .map(request_of)
            .collect()
    }

    /// The page of the subject's requests that `query` asks for, newest
    /// first by default.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidQuery`] for a query the list does not take.
    pub async fn subject_page<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        no_select(query).map_err(DomainError::InvalidQuery)?;
        let select = ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_subject(key));
        paginate_odata_try::<SubjectReviewField, SubjectReviewMapper, _, _, _, _, _>(
            select,
            conn,
            &ordered(query, ("created_at", SortDir::Desc)),
            ("id", SortDir::Desc),
            PAGE_LIMITS,
            request_of,
        )
        .await
        .map_err(page_error)
    }

    /// The page of the tenant's open requests that `query` asks for, oldest
    /// first by default.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidQuery`] for a query the list does not take.
    pub async fn open_page<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        no_select(query).map_err(DomainError::InvalidQuery)?;
        let select = ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(open().add(Column::TenantId.eq(tenant_id)));
        paginate_odata_try::<OpenReviewField, OpenReviewMapper, _, _, _, _, _>(
            select,
            conn,
            &ordered(query, ("created_at", SortDir::Asc)),
            ("id", SortDir::Asc),
            PAGE_LIMITS,
            request_of,
        )
        .await
        .map_err(page_error)
    }
}

/// [`ReviewRequests`] over the database: each call on a connection of its
/// own, a claim or a close and its read-back in one transaction.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-storage:p1
pub struct ReviewRequestStore {
    db: Arc<DbProvider>,
    repo: SeaOrmReviewRequestRepository,
}

impl ReviewRequestStore {
    #[must_use]
    pub fn new(db: Arc<DbProvider>) -> Self {
        Self {
            db,
            repo: SeaOrmReviewRequestRepository,
        }
    }

    /// Close `key` in one transaction with its read-back.
    async fn close_and_read(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        closed: &Closed,
        guard: CloseGuard,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let (repo, scope, closed) = (self.repo, scope.clone(), closed.clone());
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move {
                    if !repo.close(tx, &scope, key, &closed, guard).await? {
                        return Ok(None);
                    }
                    repo.find(tx, &scope, key).await
                })
            })
            .await
    }
}

#[async_trait]
impl FactsUnderReview for ReviewRequestStore {
    async fn under_review(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<BTreeSet<NodeKey>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.open_fact_ids(&conn, scope, key).await
    }
}

#[async_trait]
impl ReviewRequests for ReviewRequestStore {
    async fn open(
        &self,
        scope: &AccessScope,
        request: NewReviewRequest,
    ) -> Result<Opened, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.insert(&conn, scope, request).await
    }

    async fn find(
        &self,
        scope: &AccessScope,
        key: RequestKey,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.find(&conn, scope, key).await
    }

    async fn claim(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        at: OffsetDateTime,
        hold: Duration,
    ) -> Result<Claimed, DomainError> {
        let (repo, scope) = (self.repo, scope.clone());
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move { repo.claim_and_read(tx, &scope, key, claim, at, hold).await })
            })
            .await
    }

    async fn pend(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        pending: &Closed,
    ) -> Result<bool, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.pend(&conn, scope, key, claim, pending).await
    }

    async fn release(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.release(&conn, scope, key, claim).await
    }

    async fn close(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        self.close_and_read(scope, key, closed, CloseGuard::HeldBy(claim))
            .await
    }

    async fn open_request_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
    ) -> Result<Option<(ReviewRequest, Option<Closed>)>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.open_on(&conn, scope, key, fact_id).await
    }

    async fn close_open(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        recorded: Option<&Closed>,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let recorded = recorded.map(pending_text).transpose()?;
        self.close_and_read(scope, key, closed, CloseGuard::Recorded(recorded))
            .await
    }

    async fn latest_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_ids: &[NodeKey],
    ) -> Result<Vec<ReviewRequest>, DomainError> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn().map_err(DomainError::from)?;
        let ids = fact_ids.iter().map(|key| key.0.clone());
        // Newest first, so the first request of a fact is its latest.
        let mut seen = BTreeSet::new();
        Ok(self
            .repo
            .of_subject(
                &conn,
                scope,
                key,
                Condition::all().add(Column::FactId.is_in(ids)),
            )
            .await?
            .into_iter()
            .filter(|request| seen.insert(request.fact_id.clone()))
            .collect())
    }

    async fn of_subject(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<Vec<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo
            .of_subject(&conn, scope, key, Condition::all())
            .await
    }

    async fn subject_page(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.subject_page(&conn, scope, key, query).await
    }

    async fn open_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.open_page(&conn, scope, tenant_id, query).await
    }
}

/// The subject's list: its `OData` fields on the table's columns.
pub struct SubjectReviewMapper;

impl FieldToColumn<SubjectReviewField> for SubjectReviewMapper {
    type Column = Column;

    fn map_field(field: SubjectReviewField) -> Column {
        match field {
            SubjectReviewField::Id => Column::Id,
            SubjectReviewField::FactId => Column::FactId,
            SubjectReviewField::State => Column::State,
            SubjectReviewField::Outcome => Column::Outcome,
            SubjectReviewField::CreatedAt => Column::CreatedAt,
        }
    }
}

impl ODataFieldMapping<SubjectReviewField> for SubjectReviewMapper {
    type Entity = ReviewRequestEntity;

    fn extract_cursor_value(model: &entity::Model, field: SubjectReviewField) -> sea_orm::Value {
        match field {
            SubjectReviewField::Id => sea_orm::Value::Uuid(Some(model.id)),
            SubjectReviewField::FactId => sea_orm::Value::String(Some(model.fact_id.clone())),
            SubjectReviewField::State => sea_orm::Value::String(Some(model.state.clone())),
            SubjectReviewField::Outcome => sea_orm::Value::String(model.outcome.clone()),
            SubjectReviewField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
        }
    }
}

/// The reviewers' list: its `OData` fields on the table's columns.
pub struct OpenReviewMapper;

impl FieldToColumn<OpenReviewField> for OpenReviewMapper {
    type Column = Column;

    fn map_field(field: OpenReviewField) -> Column {
        match field {
            OpenReviewField::Id => Column::Id,
            OpenReviewField::SubjectId => Column::SubjectId,
            OpenReviewField::FactId => Column::FactId,
            OpenReviewField::EntityKind => Column::EntityKind,
            OpenReviewField::CreatedAt => Column::CreatedAt,
        }
    }
}

impl ODataFieldMapping<OpenReviewField> for OpenReviewMapper {
    type Entity = ReviewRequestEntity;

    fn extract_cursor_value(model: &entity::Model, field: OpenReviewField) -> sea_orm::Value {
        match field {
            OpenReviewField::Id => sea_orm::Value::Uuid(Some(model.id)),
            OpenReviewField::SubjectId => sea_orm::Value::Uuid(Some(model.subject_id)),
            OpenReviewField::FactId => sea_orm::Value::String(Some(model.fact_id.clone())),
            OpenReviewField::EntityKind => sea_orm::Value::String(Some(model.entity_kind.clone())),
            OpenReviewField::CreatedAt => {
                sea_orm::Value::TimeDateTimeWithTimeZone(Some(model.created_at))
            }
        }
    }
}
