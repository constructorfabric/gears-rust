// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-transactional-sink:p1
// @cpt-dod:cpt-cf-settings-service-dod-audit-store-retention:p1
//! The R1 `AuditSink`: the gear's own `audit_records` table, written in the
//! mutation's transaction, read back for the per-(setting, scope) history, and
//! pruned by retention and nothing else.

use async_trait::async_trait;
use sea_orm::{ActiveValue::Set, ColumnTrait, EntityTrait, QueryFilter};
use time::{Duration, OffsetDateTime};
use toolkit_db::odata::{FieldToColumn, LimitCfg, ODataFieldMapping, paginate_odata};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt};
use toolkit_odata::filter::{FieldKind, FilterField};
use toolkit_odata::{ODataQuery, Page, SortDir};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{
    ActorClassification, AuditOperation, AuditOutcome, AuditRecord, AuditSink, AuditValue,
    StoredAuditRecord,
};
use crate::domain::error::DomainError;
use crate::infra::storage::entity::audit_record::{self, Entity as AuditEntity};

/// Page bounds of the history read.
const HISTORY_LIMIT_CFG: LimitCfg = LimitCfg {
    default: 50,
    max: 200,
};

/// The store. Stateless: every operation takes its connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct AuditStore;

fn unavailable(err: impl std::fmt::Display) -> DomainError {
    DomainError::Unavailable {
        detail: format!("audit store: {err}"),
    }
}

fn image_to_json(image: Option<&AuditValue>) -> Option<serde_json::Value> {
    image.map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
}

fn image_from_json(json: Option<serde_json::Value>) -> Option<AuditValue> {
    json.and_then(|v| serde_json::from_value(v).ok())
}

fn to_domain(model: audit_record::Model) -> Result<StoredAuditRecord, DomainError> {
    let corrupt = |what: &str, raw: &str| DomainError::Internal {
        diagnostic: format!(
            "audit record {} carries an unknown {what} `{raw}`",
            model.id
        ),
    };
    Ok(StoredAuditRecord {
        id: model.id,
        declaration_key: model.declaration_key,
        tenant_id: model.tenant_id,
        operation: AuditOperation::parse(&model.operation)
            .ok_or_else(|| corrupt("operation", &model.operation))?,
        actor: model.actor,
        actor_classification: ActorClassification::parse(&model.actor_classification)
            .ok_or_else(|| corrupt("actor classification", &model.actor_classification))?,
        pre_image: image_from_json(model.pre_value),
        post_image: image_from_json(model.post_value),
        outcome: AuditOutcome::parse(&model.outcome)
            .ok_or_else(|| corrupt("outcome", &model.outcome))?,
        request_id: model.request_id,
        change_set_id: model.change_set_id,
        occurred_at: model.occurred_at,
        retain_until: model.retain_until,
    })
}

/// The orderable surface of the history read: newest first, ties broken by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditFilterField {
    /// When the record was written.
    OccurredAt,
    /// The row identity, as the tiebreaker.
    Id,
}

impl FilterField for AuditFilterField {
    const FIELDS: &'static [Self] = &[Self::OccurredAt, Self::Id];

    fn name(&self) -> &'static str {
        match self {
            Self::OccurredAt => "occurred_at",
            Self::Id => "id",
        }
    }

    fn kind(&self) -> FieldKind {
        match self {
            Self::OccurredAt => FieldKind::DateTimeUtc,
            Self::Id => FieldKind::Uuid,
        }
    }
}

/// Column mapping for the history page.
pub struct AuditODataMapper;

impl FieldToColumn<AuditFilterField> for AuditODataMapper {
    type Column = audit_record::Column;

    fn map_field(field: AuditFilterField) -> audit_record::Column {
        match field {
            AuditFilterField::OccurredAt => audit_record::Column::OccurredAt,
            AuditFilterField::Id => audit_record::Column::Id,
        }
    }
}

impl ODataFieldMapping<AuditFilterField> for AuditODataMapper {
    type Entity = AuditEntity;

    fn extract_cursor_value(
        model: &audit_record::Model,
        field: AuditFilterField,
    ) -> sea_orm::Value {
        match field {
            AuditFilterField::OccurredAt => model.occurred_at.into(),
            AuditFilterField::Id => model.id.into(),
        }
    }
}

#[async_trait]
impl AuditSink for AuditStore {
    async fn append<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        record: AuditRecord,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-4
        let active = audit_record::ActiveModel {
            id: Set(Uuid::new_v4()),
            resource: Set(record.resource),
            declaration_key: Set(record.declaration_key),
            tenant_id: Set(record.tenant_id),
            operation: Set(record.operation.as_str().to_owned()),
            actor: Set(record.actor),
            actor_classification: Set(record.actor_classification.as_str().to_owned()),
            pre_value: Set(image_to_json(record.pre_image.as_ref())),
            post_value: Set(image_to_json(record.post_image.as_ref())),
            outcome: Set(record.outcome.as_str().to_owned()),
            request_id: Set(record.request_id),
            change_set_id: Set(record.change_set_id),
            occurred_at: Set(OffsetDateTime::now_utc()),
            retain_until: Set(record.retain_until),
        };
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-4
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-5
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-6
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-7
        // Inside the caller's transaction, on the scoped write path. A failure
        // here is unavailability: the caller propagates it and the transaction
        // rolls back, so the change it audits never commits without it. Nothing
        // else is tracked — the record lives or dies with the mutation.
        toolkit_db::secure::secure_insert::<AuditEntity>(active, scope, conn)
            .await
            .map(|_| ())
            .map_err(unavailable)
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-7
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-6
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-append:p1:inst-as-append-5
    }
}

impl AuditStore {
    /// The history of one setting at one scope, newest first, cursor-paginated.
    ///
    /// An index lookup on `(declaration_key, tenant_id)`; `query` contributes
    /// only `limit` and `cursor`, the order being fixed.
    ///
    /// # Errors
    /// [`DomainError::Validation`] on a malformed cursor; [`DomainError`] when
    /// the read fails.
    pub async fn history<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        declaration_key: &str,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<StoredAuditRecord>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-7
        let base = AuditEntity::find()
            .filter(audit_record::Column::DeclarationKey.eq(declaration_key))
            .filter(audit_record::Column::TenantId.eq(tenant_id))
            .secure()
            .scope_with(scope);
        let paged = ODataQuery {
            filter: None,
            order: toolkit_odata::ODataOrderBy(vec![toolkit_odata::OrderKey {
                field: "occurred_at".to_owned(),
                dir: SortDir::Desc,
            }]),
            limit: query.limit,
            cursor: query.cursor.clone(),
            filter_hash: None,
            select: None,
        };
        let page = paginate_odata::<AuditFilterField, AuditODataMapper, _, _, _, _>(
            base,
            conn,
            &paged,
            ("id", SortDir::Desc),
            HISTORY_LIMIT_CFG,
            |m: audit_record::Model| m,
        )
        .await
        .map_err(|err| DomainError::Validation {
            field: "query".to_owned(),
            code: crate::field::ODATA_QUERY,
            message: err.to_string(),
        })?;
        // @cpt-end:cpt-cf-settings-service-flow-audit-store-history:p1:inst-as-hist-7
        let items = page
            .items
            .into_iter()
            .map(to_domain)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// Delete the records past their retention horizon, returning how many.
    ///
    /// The only delete the table ever sees: rows with an explicit `retain_until`
    /// behind `now`, found through `idx_audit_retention`, and rows without one
    /// whose `occurred_at` plus the configured default is behind `now`.
    ///
    /// # Errors
    /// [`DomainError`] when the delete fails.
    pub async fn prune_expired<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        now: OffsetDateTime,
        default_retention: Duration,
    ) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-3
        let default_cutoff = now - default_retention;
        let expired = sea_orm::Condition::any()
            .add(
                sea_orm::Condition::all()
                    .add(audit_record::Column::RetainUntil.is_not_null())
                    .add(audit_record::Column::RetainUntil.lt(now)),
            )
            .add(
                sea_orm::Condition::all()
                    .add(audit_record::Column::RetainUntil.is_null())
                    .add(audit_record::Column::OccurredAt.lt(default_cutoff)),
            );
        let outcome = AuditEntity::delete_many()
            .filter(expired)
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(unavailable)?;
        Ok(outcome.rows_affected)
        // @cpt-end:cpt-cf-settings-service-algo-audit-store-retention:p1:inst-as-ret-3
    }

    /// Every record of one change set, for callers that retrieve them together.
    ///
    /// # Errors
    /// [`DomainError`] when the read fails.
    pub async fn by_change_set<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        change_set_id: Uuid,
    ) -> Result<Vec<StoredAuditRecord>, DomainError> {
        let rows = AuditEntity::find()
            .filter(audit_record::Column::ChangeSetId.eq(change_set_id))
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(unavailable)?;
        rows.into_iter().map(to_domain).collect()
    }
}

#[cfg(test)]
#[path = "audit_store_tests.rs"]
mod audit_store_tests;
