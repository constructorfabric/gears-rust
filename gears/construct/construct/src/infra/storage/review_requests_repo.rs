use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, SqlErr};
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
use crate::domain::pagination::{DEFAULT_LIMIT, MAX_LIMIT, PageError, QueryError};
use crate::domain::plan::NodeKey;
use crate::domain::profile::Category;
use crate::domain::review_queue::{
    CLOSED, Closed, FactsUnderReview, NewReviewRequest, OPEN, OpenReviewField, Opened, RequestKey,
    ReviewOutcome, ReviewRequest, ReviewRequests, ReviewStatus, ReviewText, SubjectReviewField,
};
use crate::domain::subject_settings::SubjectKey;

use super::review_requests_entity::{self as entity, Column, Entity as ReviewRequestEntity};
use super::scope_error::map_scope_error;

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
            let outcome = match (row.outcome.as_deref(), row.corrected_fact_id) {
                (Some("corrected"), Some(key)) => ReviewOutcome::Corrected(NodeKey(key)),
                (Some("deleted"), None) => ReviewOutcome::Deleted,
                (Some("rejected"), None) => ReviewOutcome::Rejected,
                (outcome, corrected) => {
                    return Err(defect(format!(
                        "outcome {outcome:?} with a corrected fact id {}",
                        if corrected.is_some() {
                            "set"
                        } else {
                            "missing"
                        }
                    )));
                }
            };
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

/// The review requests table. Every call runs on the runner and under the
/// scope its caller gives, so it can join a caller's transaction and never
/// reaches past the caller's authorization.
#[derive(Debug, Default)]
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
            claimed_by: ActiveValue::Set(None),
            claimed_at: ActiveValue::Set(None),
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
        ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_request(key))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(request_of)
            .transpose()
    }

    /// Set the claim of `by` on the open request `key`, unless another
    /// reviewer holds it since `stale_before`. Whether it was set.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn claim<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        by: Uuid,
        at: OffsetDateTime,
        stale_before: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let free = Condition::any()
            .add(Column::ClaimedAt.is_null())
            .add(Column::ClaimedAt.lt(stale_before))
            .add(Column::ClaimedBy.eq(by));
        let updated = ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(open().add(of_request(key)).add(free))
            .col_expr(Column::ClaimedBy, Expr::value(by))
            .col_expr(Column::ClaimedAt, Expr::value(at))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(updated.rows_affected > 0)
    }

    /// Clear the claim of `by` on the request `key`.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn release<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: RequestKey,
        by: Uuid,
    ) -> Result<(), DomainError> {
        ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(open().add(of_request(key)).add(Column::ClaimedBy.eq(by)))
            .col_expr(Column::ClaimedBy, Expr::value(Option::<Uuid>::None))
            .col_expr(
                Column::ClaimedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(())
    }

    /// Close the open request `key` as `closed`, clearing its flagged value
    /// and claim; with `claimed`, only while `closed.by` holds the claim.
    /// Whether it was closed.
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
        claimed: bool,
    ) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-close:p1:inst-close-update
        // @cpt-begin:cpt-cf-construct-state-review-queue-request:p1:inst-state-close
        let mut only = open().add(of_request(key));
        if claimed {
            only = only.add(Column::ClaimedBy.eq(closed.by));
        }
        let updated = ReviewRequestEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(only)
            .col_expr(Column::State, Expr::value(CLOSED))
            .col_expr(Column::OpenFactId, Expr::value(Option::<String>::None))
            .col_expr(Column::Value, Expr::value(Option::<String>::None))
            .col_expr(Column::ClaimedBy, Expr::value(Option::<Uuid>::None))
            .col_expr(
                Column::ClaimedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            )
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

    /// The id of the open request on the subject's fact, if there is one.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn open_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
    ) -> Result<Option<Uuid>, DomainError> {
        Ok(ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(of_subject(key).add(Column::OpenFactId.eq(fact_id.0.clone())))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(|row| row.id))
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
/// own, a close and its read-back in one transaction.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-storage:p1
pub struct ReviewRequestStore {
    db: Arc<DbProvider>,
    repo: Arc<SeaOrmReviewRequestRepository>,
}

impl ReviewRequestStore {
    #[must_use]
    pub fn new(db: Arc<DbProvider>) -> Self {
        Self {
            db,
            repo: Arc::new(SeaOrmReviewRequestRepository),
        }
    }

    /// Close `key` in one transaction with its read-back.
    async fn close_and_read(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        closed: &Closed,
        claimed: bool,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let (repo, scope, closed) = (Arc::clone(&self.repo), scope.clone(), closed.clone());
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move {
                    if !repo.close(tx, &scope, key, &closed, claimed).await? {
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
        Ok(self
            .repo
            .of_subject(&conn, scope, key, open())
            .await?
            .into_iter()
            .map(|request| request.fact_id)
            .collect())
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
        by: Uuid,
        at: OffsetDateTime,
        stale_before: OffsetDateTime,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        if !self
            .repo
            .claim(&conn, scope, key, by, at, stale_before)
            .await?
        {
            return Ok(None);
        }
        self.repo.find(&conn, scope, key).await
    }

    async fn release(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        by: Uuid,
    ) -> Result<(), DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        self.repo.release(&conn, scope, key, by).await
    }

    async fn close(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        self.close_and_read(scope, key, closed, true).await
    }

    async fn close_open_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let Some(id) = self.repo.open_on(&conn, scope, key, fact_id).await? else {
            return Ok(None);
        };
        let request = RequestKey {
            tenant_id: key.tenant_id,
            id,
        };
        self.close_and_read(scope, request, closed, false).await
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
