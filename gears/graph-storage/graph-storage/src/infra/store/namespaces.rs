//! The source-namespace ownership registry, in `PostgreSQL`
//! (`cpt-cf-graph-storage-fr-source-ownership`).
//!
//! Two callers: the ingest path, which claims an unclaimed namespace and
//! refuses a write under someone else's, and the administrative transfer,
//! which is the only way a namespace changes hands.

use graph_storage_sdk::models::{SourceNamespaceOwner, Subject};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt};

use crate::domain::ownership::{Claim, decide};
use crate::infra::storage::entity::source_namespace_owner as owner;
use crate::infra::store::{PgGraphStore, map_db_error, map_scope_err};

fn to_model(row: owner::Model) -> SourceNamespaceOwner {
    SourceNamespaceOwner {
        namespace: row.namespace,
        owner_principal: row.owner_principal,
        claimed_at: row.claimed_at,
        previous_owner: row.previous_owner,
        transferred_at: row.transferred_at,
        transferred_by: row.transferred_by_subject_id.map(|subject_id| Subject {
            subject_id,
            subject_type: row.transferred_by_subject_type,
        }),
    }
}

/// Authorize one namespaced write inside the ingest transaction, claiming the
/// namespace if nobody holds it.
///
/// Runs in the write transaction on purpose: two producers claiming one
/// namespace at the same instant serialize on the row, so exactly one claim
/// wins and the other is told it is forbidden — rather than both believing
/// they own it.
pub(crate) async fn authorize_write(
    tenant: uuid::Uuid,
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    namespace: &str,
    writer: &str,
) -> Result<(), GraphStoreError> {
    let existing = owner::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(owner::Column::Namespace.eq(namespace)))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    match decide(
        existing.as_ref().map(|row| row.owner_principal.as_str()),
        writer,
    ) {
        Claim::Allowed => Ok(()),
        Claim::Forbidden => Err(GraphStoreError::SourceNamespaceForbidden {
            namespace: namespace.to_owned(),
        }),
        Claim::Take => {
            let active = owner::ActiveModel {
                tenant_id: ActiveValue::Set(tenant),
                namespace: ActiveValue::Set(namespace.to_owned()),
                owner_principal: ActiveValue::Set(writer.to_owned()),
                claimed_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
                previous_owner: ActiveValue::Set(None),
                transferred_at: ActiveValue::Set(None),
                transferred_by_subject_id: ActiveValue::Set(None),
                transferred_by_subject_type: ActiveValue::Set(None),
            };
            // On conflict, keep the owner that is already there: inside
            // `DO UPDATE SET`, an unqualified column on the right refers to the
            // *existing* row, so this writes the current owner back to itself.
            // It is `DO NOTHING` with a row lock — which is the point. Two
            // producers claiming one namespace in the same instant serialize
            // here, and the re-read below tells the loser it is forbidden
            // rather than letting both believe they own it. A claim must never
            // overwrite a claim.
            let on_conflict = toolkit_db::secure::SecureOnConflict::<owner::Entity>::columns([
                owner::Column::TenantId,
                owner::Column::Namespace,
            ])
            .value(
                owner::Column::OwnerPrincipal,
                // Table-qualified on purpose: inside `DO UPDATE SET` a bare
                // column name is ambiguous between the target row and
                // `excluded`, and PostgreSQL refuses it. Qualified, it
                // names the row that is already there — which is the whole
                // intent, "keep the owner you have".
                Expr::col((owner::Entity, owner::Column::OwnerPrincipal)),
            )
            .map_err(map_scope_err)?;
            owner::Entity::insert(active)
                .secure()
                .scope_unchecked(scope)
                .map_err(map_scope_err)?
                .on_conflict(on_conflict)
                .exec(tx)
                .await
                .map_err(map_scope_err)?;

            let settled = owner::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(Condition::all().add(owner::Column::Namespace.eq(namespace)))
                .one(tx)
                .await
                .map_err(map_scope_err)?;
            match settled {
                Some(row) if row.owner_principal == writer => Ok(()),
                Some(_) => Err(GraphStoreError::SourceNamespaceForbidden {
                    namespace: namespace.to_owned(),
                }),
                None => Err(GraphStoreError::Internal(
                    "the namespace claim neither inserted nor conflicted".to_owned(),
                )),
            }
        }
    }
}

pub async fn list(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
) -> Result<Vec<SourceNamespaceOwner>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    Ok(owner::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .order_by(owner::Column::Namespace, sea_orm::Order::Asc)
        .all(&conn)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .map(to_model)
        .collect())
}

/// Move a namespace to another principal, recording who moved it and from
/// whom.
///
/// The transfer is the whole reason the registry is the authority rather than
/// `node.owner_principal`: the rows the previous owner created keep saying so —
/// that is provenance, and it is immutable — while the right to write the
/// namespace moves here, in one row, under the ontology-administration
/// permission.
pub async fn transfer(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    namespace: &str,
    owner_principal: &str,
) -> Result<SourceNamespaceOwner, GraphStoreError> {
    if owner_principal.trim().is_empty() {
        return Err(GraphStoreError::InvalidQuery {
            what: "a transfer needs the principal to transfer to".to_owned(),
        });
    }
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    let namespace = namespace.to_owned();
    let new_owner = owner_principal.to_owned();
    let subject = ctx.subject.clone();
    store
        .db()
        .transaction_ref_mapped::<_, SourceNamespaceOwner, crate::infra::store::TxStoreError>(
            move |tx| {
                let scope = scope.clone();
                let namespace = namespace.clone();
                let new_owner = new_owner.clone();
                let subject = subject.clone();
                Box::pin(async move {
                    transfer_in_tx(tenant, &scope, tx, &namespace, &new_owner, &subject)
                        .await
                        .map_err(crate::infra::store::TxStoreError::from)
                })
            },
        )
        .await
        .map_err(|error| error.0)
}

async fn transfer_in_tx(
    tenant: uuid::Uuid,
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    namespace: &str,
    new_owner: &str,
    subject: &Subject,
) -> Result<SourceNamespaceOwner, GraphStoreError> {
    let now = time::OffsetDateTime::now_utc();
    let existing = owner::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(owner::Column::Namespace.eq(namespace)))
        .one(tx)
        .await
        .map_err(map_scope_err)?;

    let Some(row) = existing else {
        // Assigning an unclaimed namespace is a claim on someone's behalf,
        // which is the same administrative act and is recorded the same way.
        let active = owner::ActiveModel {
            tenant_id: ActiveValue::Set(tenant),
            namespace: ActiveValue::Set(namespace.to_owned()),
            owner_principal: ActiveValue::Set(new_owner.to_owned()),
            claimed_at: ActiveValue::Set(now),
            previous_owner: ActiveValue::Set(None),
            transferred_at: ActiveValue::Set(Some(now)),
            transferred_by_subject_id: ActiveValue::Set(Some(subject.subject_id)),
            transferred_by_subject_type: ActiveValue::Set(subject.subject_type.clone()),
        };
        let row = owner::Entity::insert(active)
            .secure()
            .scope_unchecked(scope)
            .map_err(map_scope_err)?
            .exec_with_returning(tx)
            .await
            .map_err(map_scope_err)?;
        return Ok(to_model(row));
    };

    if row.owner_principal == new_owner {
        return Ok(to_model(row));
    }

    owner::Entity::update_many()
        .col_expr(owner::Column::OwnerPrincipal, Expr::value(new_owner))
        .col_expr(
            owner::Column::PreviousOwner,
            Expr::value(Some(row.owner_principal.clone())),
        )
        .col_expr(owner::Column::TransferredAt, Expr::value(Some(now)))
        .col_expr(
            owner::Column::TransferredBySubjectId,
            Expr::value(Some(subject.subject_id)),
        )
        .col_expr(
            owner::Column::TransferredBySubjectType,
            Expr::value(subject.subject_type.clone()),
        )
        .filter(Condition::all().add(owner::Column::Namespace.eq(namespace)))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await
        .map_err(map_scope_err)?;

    Ok(SourceNamespaceOwner {
        namespace: row.namespace,
        owner_principal: new_owner.to_owned(),
        claimed_at: row.claimed_at,
        previous_owner: Some(row.owner_principal),
        transferred_at: Some(now),
        transferred_by: Some(subject.clone()),
    })
}
