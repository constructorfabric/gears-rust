use std::borrow::Cow;
use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, SqlErr};
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
    Closing, NewReviewRequest, OpenReviewField, Opened, ReviewOutcome, ReviewRequest,
    ReviewRequests, ReviewState, SubjectReviewField,
};
use crate::domain::subject_settings::SubjectKey;

use super::review_requests_entity::{self as entity, Column, Entity as ReviewRequestEntity};
use super::scope_error::map_scope_error;

const PAGE_LIMITS: LimitCfg = LimitCfg {
    default: DEFAULT_LIMIT,
    max: MAX_LIMIT,
};

/// `SeaORM`-backed [`ReviewRequests`]. Every query goes through the secure
/// ORM: the calls about one subject under the subject's tenant, the
/// reviewers' under their own scope, and each names its tenant besides.
pub struct SeaOrmReviewRequests {
    db: Arc<DbProvider>,
}

impl SeaOrmReviewRequests {
    #[must_use]
    pub fn new(db: Arc<DbProvider>) -> Self {
        Self { db }
    }
}

fn is_unique_violation(err: &ScopeError) -> bool {
    matches!(
        err,
        ScopeError::Db(db) if matches!(db.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
    )
}

/// A stored row as a request. A row Construct cannot read is a defect.
fn request_of(row: entity::Model) -> Result<ReviewRequest, DomainError> {
    let defect =
        |what: &str| DomainError::internal(format!("review request {} has {what}", row.id));
    let category = Category::from_name(&row.entity_kind)
        .ok_or_else(|| defect(&format!("an unknown entity kind `{}`", row.entity_kind)))?;
    let state = ReviewState::from_name(&row.state)
        .ok_or_else(|| defect(&format!("an unknown state `{}`", row.state)))?;
    let outcome = row
        .outcome
        .as_deref()
        .map(|name| {
            ReviewOutcome::from_name(name)
                .ok_or_else(|| defect(&format!("an unknown outcome `{name}`")))
        })
        .transpose()?;
    let value = row
        .value
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| defect("a value that is not JSON"))?;
    Ok(ReviewRequest {
        id: row.id,
        tenant_id: row.tenant_id,
        subject_id: row.subject_id,
        fact_id: NodeKey(row.fact_id),
        category,
        property: row.property,
        value,
        comment: row.comment,
        state,
        outcome,
        reason: row.reason,
        resolved_by: row.resolved_by,
        corrected_fact_id: row.corrected_fact_id.map(NodeKey),
        created_at: row.created_at,
        resolved_at: row.resolved_at,
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

fn open() -> Condition {
    Condition::all().add(Column::State.eq(ReviewState::Open.name()))
}

/// Close the open request `id` of `tenant_id`, and read it back.
async fn close_in<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
    closing: &Closing,
) -> Result<Option<ReviewRequest>, DomainError> {
    // @cpt-begin:cpt-cf-construct-algo-review-queue-close:p1:inst-close-update
    // @cpt-begin:cpt-cf-construct-state-review-queue-request:p1:inst-state-close
    let updated = ReviewRequestEntity::update_many()
        .secure()
        .scope_with(scope)
        .filter(
            open()
                .add(Column::TenantId.eq(tenant_id))
                .add(Column::Id.eq(id)),
        )
        .col_expr(Column::State, Expr::value(ReviewState::Closed.name()))
        .col_expr(Column::OpenFactId, Expr::value(Option::<String>::None))
        .col_expr(Column::Value, Expr::value(Option::<String>::None))
        .col_expr(Column::Outcome, Expr::value(closing.outcome.name()))
        .col_expr(Column::Reason, Expr::value(closing.reason.clone()))
        .col_expr(Column::ResolvedBy, Expr::value(closing.by))
        .col_expr(
            Column::CorrectedFactId,
            Expr::value(closing.corrected_fact_id.clone().map(|key| key.0)),
        )
        .col_expr(Column::ResolvedAt, Expr::value(closing.at))
        .exec(conn)
        .await
        .map_err(map_scope_error)?;
    if updated.rows_affected == 0 {
        return Ok(None);
    }
    // @cpt-end:cpt-cf-construct-state-review-queue-request:p1:inst-state-close
    // @cpt-end:cpt-cf-construct-algo-review-queue-close:p1:inst-close-update
    find_in(conn, scope, tenant_id, id).await
}

async fn find_in<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<ReviewRequest>, DomainError> {
    ReviewRequestEntity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(Column::TenantId.eq(tenant_id))
                .add(Column::Id.eq(id)),
        )
        .one(conn)
        .await
        .map_err(map_scope_error)?
        .map(request_of)
        .transpose()
}

/// @cpt-dod:cpt-cf-construct-dod-review-queue-storage:p1
#[async_trait]
impl ReviewRequests for SeaOrmReviewRequests {
    async fn open(&self, request: NewReviewRequest) -> Result<Opened, DomainError> {
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
            comment: ActiveValue::Set(request.comment.clone()),
            state: ActiveValue::Set(ReviewState::Open.name().to_owned()),
            open_fact_id: ActiveValue::Set(Some(request.fact_id.0.clone())),
            outcome: ActiveValue::Set(None),
            reason: ActiveValue::Set(None),
            resolved_by: ActiveValue::Set(None),
            corrected_fact_id: ActiveValue::Set(None),
            created_at: ActiveValue::Set(request.created_at),
            resolved_at: ActiveValue::Set(None),
        };
        let conn = self.db.conn().map_err(DomainError::from)?;
        let scope = AccessScope::for_tenant(request.key.tenant_id);
        let inserted = ReviewRequestEntity::insert(row.clone())
            .secure()
            .scope_with_model(&scope, &row)
            .map_err(map_scope_error)?
            .exec(&conn)
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
            value: Some(request.value),
            comment: request.comment,
            state: ReviewState::Open,
            outcome: None,
            reason: None,
            resolved_by: None,
            corrected_fact_id: None,
            created_at: request.created_at,
            resolved_at: None,
        })))
    }

    async fn find(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        find_in(&conn, scope, tenant_id, id).await
    }

    async fn close(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        id: Uuid,
        closing: &Closing,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let (scope, closing) = (scope.clone(), closing.clone());
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move { close_in(tx, &scope, tenant_id, id, &closing).await })
            })
            .await
    }

    async fn close_open_on(
        &self,
        key: SubjectKey,
        fact_id: &NodeKey,
        closing: &Closing,
    ) -> Result<Option<ReviewRequest>, DomainError> {
        let (fact_id, closing) = (fact_id.0.clone(), closing.clone());
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::for_tenant(key.tenant_id);
                    let Some(open_request) = ReviewRequestEntity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(of_subject(key).add(Column::OpenFactId.eq(fact_id)))
                        .one(tx)
                        .await
                        .map_err(map_scope_error)?
                    else {
                        return Ok(None);
                    };
                    close_in(tx, &scope, key.tenant_id, open_request.id, &closing).await
                })
            })
            .await
    }

    async fn under_review(&self, key: SubjectKey) -> Result<BTreeSet<NodeKey>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let rows = ReviewRequestEntity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(key.tenant_id))
            .filter(of_subject(key).add(open()))
            .all(&conn)
            .await
            .map_err(map_scope_error)?;
        Ok(rows.into_iter().map(|row| NodeKey(row.fact_id)).collect())
    }

    async fn of_subject(&self, key: SubjectKey) -> Result<Vec<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        ReviewRequestEntity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(key.tenant_id))
            .filter(of_subject(key))
            .order_by(Column::CreatedAt, sea_orm::Order::Desc)
            .order_by(Column::Id, sea_orm::Order::Desc)
            .all(&conn)
            .await
            .map_err(map_scope_error)?
            .into_iter()
            .map(request_of)
            .collect()
    }

    async fn subject_page(
        &self,
        key: SubjectKey,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let select = ReviewRequestEntity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(key.tenant_id))
            .filter(of_subject(key));
        paginate_odata_try::<SubjectReviewField, SubjectReviewMapper, _, _, _, _, _>(
            select,
            &conn,
            &ordered(query, ("created_at", SortDir::Desc)),
            ("id", SortDir::Desc),
            PAGE_LIMITS,
            request_of,
        )
        .await
        .map_err(page_error)
    }

    async fn open_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let select = ReviewRequestEntity::find()
            .secure()
            .scope_with(scope)
            .filter(open().add(Column::TenantId.eq(tenant_id)));
        paginate_odata_try::<OpenReviewField, OpenReviewMapper, _, _, _, _, _>(
            select,
            &conn,
            &ordered(query, ("created_at", SortDir::Asc)),
            ("id", SortDir::Asc),
            PAGE_LIMITS,
            request_of,
        )
        .await
        .map_err(page_error)
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
