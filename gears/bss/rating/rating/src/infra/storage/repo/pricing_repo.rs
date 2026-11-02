//! `bss_rating__pricing_binding` (one row per distinct answer), `bss_rating__pricing_resolve` (one
//! row per query and UTC day) and `bss_rating__derived_declaration`: all insert-only. The same
//! digest stored with other content is an alarm, never an overwrite (DESIGN §3.7).

use async_trait::async_trait;
use bss_rating_core::engine::current::input::{CoreCell, DerivedDeclaration, PricePin};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, Order, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, ScopeError, SecureEntityExt, SecureInsertExt,
};
use uuid::Uuid;

use crate::domain::declaration::{DeclarationRepo, StoredDeclaration};
use crate::domain::digest::Sha256Hex;
use crate::domain::error::{Cause, DomainError};
use crate::domain::pricing::{
    PricingBindingRepo, ResolveQuestion, StoreOutcome, StoredAnswer, StoredResolve,
};
use crate::infra::storage::entity::{derived_declaration, pricing_binding, pricing_resolve};
use crate::infra::storage::{corrupt, scope_failure};

/// The repository of stored Pricing answers and Products declarations.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrmPricingRepo;

fn not_inserted(err: &ScopeError) -> bool {
    matches!(err, ScopeError::Db(DbErr::RecordNotInserted))
}

fn json_error(context: &'static str, error: serde_json::Error) -> DomainError {
    DomainError::Internal(format!("{context}: {error}"), Some(Cause::new(error)))
}

const BINDING: &str = "bss_rating__pricing_binding";
const RESOLVE: &str = "bss_rating__pricing_resolve";
const DECLARATION: &str = "bss_rating__derived_declaration";

fn answer_to_domain(row: pricing_binding::Model) -> Result<StoredAnswer, DomainError> {
    let binding_digest = Sha256Hex::parse(&row.binding_digest)
        .map_err(|_| corrupt(BINDING, "binding_digest", &row.binding_digest))?;
    let cells: Vec<CoreCell> =
        serde_json::from_value(row.cells).map_err(|e| corrupt(BINDING, "cells", &e.to_string()))?;
    Ok(StoredAnswer {
        binding_digest,
        plan_id: row.plan_id,
        revision_id: row.revision_id,
        cells,
        first_fetched_at: row.first_fetched_at,
    })
}

async fn find_answer(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    binding_digest: &Sha256Hex,
) -> Result<Option<StoredAnswer>, DomainError> {
    pricing_binding::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(pricing_binding::Column::TenantId.eq(tenant_id))
                .add(pricing_binding::Column::BindingDigest.eq(binding_digest.as_str())),
        )
        .one(runner)
        .await
        .map_err(|e| scope_failure("read pricing answer", e))?
        .map(answer_to_domain)
        .transpose()
}

async fn insert_answer(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    answer: &StoredAnswer,
) -> Result<StoreOutcome, DomainError> {
    let cells = serde_json::to_value(&answer.cells).map_err(|e| json_error("answer cells", e))?;
    let active = pricing_binding::ActiveModel {
        tenant_id: Set(tenant_id),
        binding_digest: Set(answer.binding_digest.as_str().to_owned()),
        plan_id: Set(answer.plan_id),
        revision_id: Set(answer.revision_id),
        cells: Set(cells),
        first_fetched_at: Set(answer.first_fetched_at),
    };
    let inserted = pricing_binding::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert pricing answer", e))?
        .on_conflict_raw(
            OnConflict::columns([
                pricing_binding::Column::TenantId,
                pricing_binding::Column::BindingDigest,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => return Ok(StoreOutcome::Inserted),
        Err(e) if not_inserted(&e) => {}
        Err(e) => return Err(scope_failure("insert pricing answer", e)),
    }
    let stored = find_answer(runner, scope, tenant_id, &answer.binding_digest)
        .await?
        .ok_or_else(|| {
            DomainError::Concurrent("a pricing answer was not found after a conflict".to_owned())
        })?;
    if stored.plan_id == answer.plan_id
        && stored.revision_id == answer.revision_id
        && stored.cells == answer.cells
    {
        return Ok(StoreOutcome::AlreadyStored);
    }
    tracing::error!(
        target: "bss_rating.pricing",
        %tenant_id,
        binding_digest = answer.binding_digest.as_str(),
        "a pricing answer digest is stored with other content"
    );
    Err(DomainError::Internal(
        format!(
            "pricing answer {} is stored with other content",
            answer.binding_digest.as_str()
        ),
        None,
    ))
}

fn resolve_to_domain(row: pricing_resolve::Model) -> Result<StoredResolve, DomainError> {
    let query_digest = Sha256Hex::parse(&row.query_digest)
        .map_err(|_| corrupt(RESOLVE, "query_digest", &row.query_digest))?;
    let binding_digest = Sha256Hex::parse(&row.binding_digest)
        .map_err(|_| corrupt(RESOLVE, "binding_digest", &row.binding_digest))?;
    let pins: Vec<PricePin> =
        serde_json::from_value(row.pins).map_err(|e| corrupt(RESOLVE, "pins", &e.to_string()))?;
    Ok(StoredResolve {
        query_digest,
        fetched_on: row.fetched_on,
        question: ResolveQuestion {
            catalog_tenant_id: row.catalog_tenant_id,
            revision_id: row.revision_id,
            date: row.resolve_date,
            item_id: row.item_id,
            pins,
        },
        binding_digest,
        fetched_at: row.fetched_at,
    })
}

async fn newest_resolve(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    query_digest: &Sha256Hex,
) -> Result<Option<StoredResolve>, DomainError> {
    pricing_resolve::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(pricing_resolve::Column::TenantId.eq(tenant_id))
                .add(pricing_resolve::Column::QueryDigest.eq(query_digest.as_str())),
        )
        .order_by(pricing_resolve::Column::FetchedOn, Order::Desc)
        .limit(1)
        .one(runner)
        .await
        .map_err(|e| scope_failure("read pricing resolve", e))?
        .map(resolve_to_domain)
        .transpose()
}

async fn find_resolve(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    resolve: &StoredResolve,
) -> Result<Option<StoredResolve>, DomainError> {
    pricing_resolve::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(pricing_resolve::Column::TenantId.eq(tenant_id))
                .add(pricing_resolve::Column::QueryDigest.eq(resolve.query_digest.as_str()))
                .add(pricing_resolve::Column::FetchedOn.eq(resolve.fetched_on)),
        )
        .one(runner)
        .await
        .map_err(|e| scope_failure("read pricing resolve", e))?
        .map(resolve_to_domain)
        .transpose()
}

async fn insert_resolve(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    resolve: &StoredResolve,
) -> Result<StoredResolve, DomainError> {
    let pins = serde_json::to_value(&resolve.question.pins).map_err(|e| json_error("pins", e))?;
    let active = pricing_resolve::ActiveModel {
        tenant_id: Set(tenant_id),
        query_digest: Set(resolve.query_digest.as_str().to_owned()),
        fetched_on: Set(resolve.fetched_on),
        catalog_tenant_id: Set(resolve.question.catalog_tenant_id),
        revision_id: Set(resolve.question.revision_id),
        resolve_date: Set(resolve.question.date),
        item_id: Set(resolve.question.item_id),
        pins: Set(pins),
        binding_digest: Set(resolve.binding_digest.as_str().to_owned()),
        fetched_at: Set(resolve.fetched_at),
    };
    let inserted = pricing_resolve::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert pricing resolve", e))?
        .on_conflict_raw(
            OnConflict::columns([
                pricing_resolve::Column::TenantId,
                pricing_resolve::Column::QueryDigest,
                pricing_resolve::Column::FetchedOn,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(resolve.clone()),
        // The first fetch of a day stands, and is what the caller gets.
        Err(e) if not_inserted(&e) => find_resolve(runner, scope, tenant_id, resolve)
            .await?
            .ok_or_else(|| {
                DomainError::Concurrent(
                    "a pricing resolve was not found after a conflict".to_owned(),
                )
            }),
        Err(e) => Err(scope_failure("insert pricing resolve", e)),
    }
}

#[async_trait]
impl PricingBindingRepo for OrmPricingRepo {
    async fn insert_answer(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        answer: &StoredAnswer,
    ) -> Result<StoreOutcome, DomainError> {
        insert_answer(tx, scope, tenant_id, answer).await
    }

    async fn find_answer(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        binding_digest: &Sha256Hex,
    ) -> Result<Option<StoredAnswer>, DomainError> {
        find_answer(tx, scope, tenant_id, binding_digest).await
    }

    async fn newest_resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        query_digest: &Sha256Hex,
    ) -> Result<Option<StoredResolve>, DomainError> {
        newest_resolve(tx, scope, tenant_id, query_digest).await
    }

    async fn insert_resolve(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        resolve: &StoredResolve,
    ) -> Result<StoredResolve, DomainError> {
        insert_resolve(tx, scope, tenant_id, resolve).await
    }
}

// ---- derived declarations ----------------------------------------------------------------------

fn declaration_to_domain(
    row: derived_declaration::Model,
) -> Result<StoredDeclaration, DomainError> {
    let digest =
        Sha256Hex::parse(&row.digest).map_err(|_| corrupt(DECLARATION, "digest", &row.digest))?;
    let declaration: DerivedDeclaration = serde_json::from_value(row.declaration)
        .map_err(|e| corrupt(DECLARATION, "declaration", &e.to_string()))?;
    let version = u32::try_from(row.version)
        .map_err(|_| corrupt(DECLARATION, "version", &row.version.to_string()))?;
    Ok(StoredDeclaration {
        code: row.code,
        version,
        digest,
        declaration,
        fetched_at: row.fetched_at,
    })
}

async fn find_declaration(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    code: &str,
    version: u32,
) -> Result<Option<StoredDeclaration>, DomainError> {
    derived_declaration::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(derived_declaration::Column::TenantId.eq(tenant_id))
                .add(derived_declaration::Column::Code.eq(code))
                .add(derived_declaration::Column::Version.eq(i64::from(version))),
        )
        .one(runner)
        .await
        .map_err(|e| scope_failure("read derived declaration", e))?
        .map(declaration_to_domain)
        .transpose()
}

async fn insert_declaration(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    declaration: &StoredDeclaration,
) -> Result<StoredDeclaration, DomainError> {
    let body = serde_json::to_value(&declaration.declaration)
        .map_err(|e| json_error("derived declaration", e))?;
    let active = derived_declaration::ActiveModel {
        tenant_id: Set(tenant_id),
        code: Set(declaration.code.clone()),
        version: Set(i64::from(declaration.version)),
        digest: Set(declaration.digest.as_str().to_owned()),
        declaration: Set(body),
        fetched_at: Set(declaration.fetched_at),
    };
    let inserted = derived_declaration::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("insert derived declaration", e))?
        .on_conflict_raw(
            OnConflict::columns([
                derived_declaration::Column::TenantId,
                derived_declaration::Column::Code,
                derived_declaration::Column::Version,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match inserted {
        Ok(_) => Ok(declaration.clone()),
        Err(e) if not_inserted(&e) => find_declaration(
            runner,
            scope,
            tenant_id,
            &declaration.code,
            declaration.version,
        )
        .await?
        .ok_or_else(|| {
            DomainError::Concurrent(
                "a derived declaration was not found after a conflict".to_owned(),
            )
        }),
        Err(e) => Err(scope_failure("insert derived declaration", e)),
    }
}

#[async_trait]
impl DeclarationRepo for OrmPricingRepo {
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        code: &str,
        version: u32,
    ) -> Result<Option<StoredDeclaration>, DomainError> {
        find_declaration(tx, scope, tenant_id, code, version).await
    }

    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        tenant_id: Uuid,
        declaration: &StoredDeclaration,
    ) -> Result<StoredDeclaration, DomainError> {
        insert_declaration(tx, scope, tenant_id, declaration).await
    }
}
