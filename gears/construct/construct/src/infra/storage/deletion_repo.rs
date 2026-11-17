use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, FromQueryResult, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::{
    DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::DbProvider;
use crate::domain::deletion::{DeletionStore, ExpiredRows, KnownTenants, Mark, ProfileGenerations};
use crate::domain::error::DomainError;
use crate::domain::subject_settings::{SettingsWrite, SubjectKey};
use crate::domain::tenant_exit::{TenantRows, TenantTables};

use super::record_ids_entity::{self as record_ids, Entity as RecordIdEntity};
use super::review_requests_entity::{self as review_requests, Entity as ReviewRequestEntity};
use super::scope_error::map_scope_error;
use super::subject_settings_entity::{self as settings, Entity as SubjectSettingsEntity};
use super::subject_settings_repo::coverage;
use super::tenants_entity::{self as tenants, Entity as TenantEntity};

/// [`DeletionStore`] and [`ProfileGenerations`] over Construct's own tables,
/// through the secure ORM.
///
/// @cpt-dod:cpt-cf-construct-dod-erasure-storage:p1
pub struct DeletionTables {
    db: Arc<DbProvider>,
    /// The tenants this instance noted already, so an add notes each once.
    /// Only a cache: a panic while it was held leaves a set that is still
    /// right, so a poisoned lock is taken as it is.
    noted: Mutex<BTreeSet<Uuid>>,
}

impl DeletionTables {
    #[must_use]
    pub fn new(db: Arc<DbProvider>) -> Self {
        Self {
            db,
            noted: Mutex::default(),
        }
    }
}

/// Add `tenant_id` to `construct__tenants` unless it is there; another
/// instance may add it at the same time.
async fn insert_tenant(
    conn: &impl DBRunner,
    tenant_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), DomainError> {
    let row = tenants::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        first_seen: ActiveValue::Set(now),
    };
    match TenantEntity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(tenant_id), &row)
        .map_err(map_scope_error)?
        .exec(conn)
        .await
    {
        Ok(_) => Ok(()),
        Err(err) if err.is_unique_violation() => Ok(()),
        Err(err) => Err(map_scope_error(err)),
    }
}

#[async_trait]
impl KnownTenants for DeletionTables {
    async fn note(&self, tenant_id: Uuid) -> Result<(), DomainError> {
        if self
            .noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&tenant_id)
        {
            return Ok(());
        }
        let conn = self.db.conn().map_err(DomainError::from)?;
        insert_tenant(&conn, tenant_id, OffsetDateTime::now_utc()).await?;
        self.noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tenant_id);
        Ok(())
    }
}

fn of_key(key: SubjectKey) -> Condition {
    Condition::all()
        .add(settings::Column::TenantId.eq(key.tenant_id))
        .add(settings::Column::SubjectId.eq(key.subject_id))
}

/// The settings row of `key`, under `scope`.
async fn row<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    key: SubjectKey,
) -> Result<Option<settings::Model>, DomainError> {
    SubjectSettingsEntity::find()
        .secure()
        .scope_with(scope)
        .filter(of_key(key))
        .one(conn)
        .await
        .map_err(map_scope_error)
}

#[derive(FromQueryResult)]
struct TenantRow {
    tenant_id: Uuid,
}

#[derive(FromQueryResult)]
struct RecordKeyRow {
    record_key: String,
}

/// How many expired record identities one statement removes, so the first
/// run after a period is set does not hold the table intake writes to in
/// one long delete.
pub(crate) const EXPIRE_BATCH: u64 = 1000;

/// Remove the tenant's record identities that also match `which`,
/// [`EXPIRE_BATCH`] at a time, so a tenant with many does not hold the table
/// intake writes to in one long delete; how many.
async fn remove_record_ids(
    conn: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    which: Condition,
) -> Result<u64, DomainError> {
    let mut removed = 0;
    loop {
        let batch: Vec<RecordKeyRow> = RecordIdEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(record_ids::Column::TenantId.eq(tenant_id))
                    .add(which.clone()),
            )
            .limit(EXPIRE_BATCH)
            .project_all(conn, |query| {
                query
                    .select_only()
                    .column(record_ids::Column::RecordKey)
                    .into_model()
            })
            .await
            .map_err(map_scope_error)?;
        let found = batch.len();
        if found == 0 {
            break;
        }
        removed += RecordIdEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(record_ids::Column::TenantId.eq(tenant_id))
                    .add(
                        record_ids::Column::RecordKey
                            .is_in(batch.into_iter().map(|row| row.record_key)),
                    ),
            )
            .exec(conn)
            .await
            .map_err(map_scope_error)?
            .rows_affected;
        if u64::try_from(found).unwrap_or(u64::MAX) < EXPIRE_BATCH {
            break;
        }
    }
    Ok(removed)
}

#[async_trait]
impl DeletionStore for DeletionTables {
    async fn mark(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        at: OffsetDateTime,
    ) -> Result<SettingsWrite, DomainError> {
        if let Err(refusal) = coverage(scope, key) {
            return Ok(SettingsWrite::OutOfScope(refusal));
        }
        // @cpt-begin:cpt-cf-construct-flow-erasure-request:p1:inst-request-mark
        // An insert, and an update when the row exists, each one statement:
        // two marks at once, or a mark beside a settings change, both land,
        // and a mark already set keeps its time.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let new = settings::ActiveModel {
            tenant_id: ActiveValue::Set(key.tenant_id),
            subject_id: ActiveValue::Set(key.subject_id),
            personalization_enabled: ActiveValue::Set(false),
            erasure_in_progress: ActiveValue::Set(true),
            profile_generation: ActiveValue::NotSet,
            erasure_marked_at: ActiveValue::Set(Some(at)),
            erasure_attempted_at: ActiveValue::NotSet,
        };
        let inserted = SubjectSettingsEntity::insert(new.clone())
            .secure()
            .scope_with_model(scope, &new)
            .map_err(map_scope_error)?
            .exec(&conn)
            .await;
        match inserted {
            Ok(_) => return Ok(SettingsWrite::Written),
            Err(err) if !err.is_unique_violation() => return Err(map_scope_error(err)),
            Err(_) => {}
        }
        SubjectSettingsEntity::update_many()
            .secure()
            .scope_with(scope)
            .filter(of_key(key))
            .col_expr(settings::Column::PersonalizationEnabled, Expr::value(false))
            .col_expr(settings::Column::ErasureInProgress, Expr::value(true))
            .col_expr(
                settings::Column::ErasureMarkedAt,
                Func::coalesce([
                    Expr::col(settings::Column::ErasureMarkedAt),
                    Expr::value(at),
                ])
                .into(),
            )
            .exec(&conn)
            .await
            .map_err(map_scope_error)?;
        Ok(SettingsWrite::Written)
        // @cpt-end:cpt-cf-construct-flow-erasure-request:p1:inst-request-mark
    }

    async fn marked(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<Option<Mark>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let Some(stored) = row(&conn, scope, key)
            .await?
            .filter(|stored| stored.erasure_in_progress)
        else {
            return Ok(None);
        };
        Ok(Some(Mark {
            // A mark from before its time was kept counts from the epoch:
            // it is old enough to finish.
            at: stored
                .erasure_marked_at
                .unwrap_or(OffsetDateTime::UNIX_EPOCH),
            generation: generation_of(key, stored.profile_generation)?,
        }))
    }

    async fn finish(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        marked_at: OffsetDateTime,
    ) -> Result<bool, DomainError> {
        let scope = scope.clone();
        // The erasure this run started: a mark kept without its time reads
        // as the epoch, so that one matches a missing time too.
        let same_mark = if marked_at == OffsetDateTime::UNIX_EPOCH {
            Condition::any()
                .add(settings::Column::ErasureMarkedAt.is_null())
                .add(settings::Column::ErasureMarkedAt.eq(marked_at))
        } else {
            Condition::all().add(settings::Column::ErasureMarkedAt.eq(marked_at))
        };
        DBProvider::<DomainError>::new(self.db.db())
            .transaction(move |tx| {
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-construct-algo-erasure-erase:p1:inst-erase-finish
                    let cleared = SubjectSettingsEntity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            of_key(key)
                                .add(settings::Column::ErasureInProgress.eq(true))
                                .add(same_mark),
                        )
                        .col_expr(settings::Column::ErasureInProgress, Expr::value(false))
                        .col_expr(
                            settings::Column::ErasureMarkedAt,
                            Expr::value(Option::<OffsetDateTime>::None),
                        )
                        .col_expr(
                            settings::Column::ErasureAttemptedAt,
                            Expr::value(Option::<OffsetDateTime>::None),
                        )
                        .col_expr(
                            settings::Column::ProfileGeneration,
                            Expr::col(settings::Column::ProfileGeneration).add(1),
                        )
                        .exec(tx)
                        .await
                        .map_err(map_scope_error)?;
                    if cleared.rows_affected == 0 {
                        return Ok(false);
                    }
                    ReviewRequestEntity::delete_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(review_requests::Column::TenantId.eq(key.tenant_id))
                                .add(review_requests::Column::SubjectId.eq(key.subject_id)),
                        )
                        .exec(tx)
                        .await
                        .map_err(map_scope_error)?;
                    RecordIdEntity::delete_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(record_ids::Column::TenantId.eq(key.tenant_id))
                                .add(record_ids::Column::SubjectId.eq(key.subject_id)),
                        )
                        .exec(tx)
                        .await
                        .map_err(map_scope_error)?;
                    Ok(true)
                    // @cpt-end:cpt-cf-construct-algo-erasure-erase:p1:inst-erase-finish
                })
            })
            .await
    }

    async fn attempted(&self, key: SubjectKey, at: OffsetDateTime) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-attempt
        let conn = self.db.conn().map_err(DomainError::from)?;
        SubjectSettingsEntity::update_many()
            .secure()
            .scope_with(&AccessScope::for_tenant(key.tenant_id))
            .filter(of_key(key).add(settings::Column::ErasureInProgress.eq(true)))
            .col_expr(settings::Column::ErasureAttemptedAt, Expr::value(Some(at)))
            .exec(&conn)
            .await
            .map_err(map_scope_error)?;
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-attempt
    }

    async fn under_way(
        &self,
        marked_before: OffsetDateTime,
        limit: u64,
    ) -> Result<Vec<SubjectKey>, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-find
        let conn = self.db.conn().map_err(DomainError::from)?;
        // Construct's own bookkeeping across tenants: the job runs as
        // Construct for every tenant, and each erasure then runs in its own.
        let rows = SubjectSettingsEntity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all()
                    .add(settings::Column::ErasureInProgress.eq(true))
                    .add(
                        Condition::any()
                            .add(settings::Column::ErasureMarkedAt.is_null())
                            .add(settings::Column::ErasureMarkedAt.lt(marked_before)),
                    ),
            )
            // The longest untried first: an erasure that keeps failing goes
            // behind the others once it was tried. A mark from before its
            // time was kept counts from the epoch, as in `marked`.
            .order_by(
                Expr::expr(Func::coalesce([
                    Expr::col(settings::Column::ErasureAttemptedAt),
                    Expr::col(settings::Column::ErasureMarkedAt),
                    Expr::value(OffsetDateTime::UNIX_EPOCH),
                ])),
                sea_orm::Order::Asc,
            )
            .limit(limit)
            .all(&conn)
            .await
            .map_err(map_scope_error)?;
        Ok(rows
            .into_iter()
            .map(|row| SubjectKey {
                tenant_id: row.tenant_id,
                subject_id: row.subject_id,
            })
            .collect())
        // @cpt-end:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-find
    }

    async fn expire(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        before: OffsetDateTime,
    ) -> Result<ExpiredRows, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-expire:p2:inst-expire-rows
        let conn = self.db.conn().map_err(DomainError::from)?;
        // Only closed requests: an open one is closed by retention's removal
        // of its fact, with its event, and goes on the next run.
        let review_requests = ReviewRequestEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(review_requests::Column::TenantId.eq(tenant_id))
                    .add(review_requests::Column::CreatedAt.lt(before))
                    .add(review_requests::Column::State.eq(super::review_requests_repo::CLOSED)),
            )
            .exec(&conn)
            .await
            .map_err(map_scope_error)?
            .rows_affected;
        let record_ids = remove_record_ids(
            &conn,
            scope,
            tenant_id,
            Condition::all().add(record_ids::Column::ReceivedAt.lt(before)),
        )
        .await?;
        Ok(ExpiredRows {
            review_requests,
            record_ids,
        })
        // @cpt-end:cpt-cf-construct-algo-retention-expire:p2:inst-expire-rows
    }

    async fn tenants(&self) -> Result<BTreeSet<Uuid>, DomainError> {
        let conn = self.db.conn().map_err(DomainError::from)?;
        let everyone = AccessScope::allow_all();
        let mut tenants = BTreeSet::new();
        let settings: Vec<TenantRow> = SubjectSettingsEntity::find()
            .secure()
            .scope_with(&everyone)
            .project_all(&conn, |query| {
                query
                    .select_only()
                    .column(settings::Column::TenantId)
                    .distinct()
                    .into_model()
            })
            .await
            .map_err(map_scope_error)?;
        let identities: Vec<TenantRow> = RecordIdEntity::find()
            .secure()
            .scope_with(&everyone)
            .project_all(&conn, |query| {
                query
                    .select_only()
                    .column(record_ids::Column::TenantId)
                    .distinct()
                    .into_model()
            })
            .await
            .map_err(map_scope_error)?;
        let requests: Vec<TenantRow> = ReviewRequestEntity::find()
            .secure()
            .scope_with(&everyone)
            .project_all(&conn, |query| {
                query
                    .select_only()
                    .column(review_requests::Column::TenantId)
                    .distinct()
                    .into_model()
            })
            .await
            .map_err(map_scope_error)?;
        let known: Vec<TenantRow> = TenantEntity::find()
            .secure()
            .scope_with(&everyone)
            .project_all(&conn, |query| {
                query
                    .select_only()
                    .column(tenants::Column::TenantId)
                    .into_model()
            })
            .await
            .map_err(map_scope_error)?;
        let known: BTreeSet<Uuid> = known.into_iter().map(|row| row.tenant_id).collect();
        tenants.extend(
            settings
                .into_iter()
                .chain(identities)
                .chain(requests)
                .map(|row| row.tenant_id),
        );
        // A tenant seen once stays known, so it is swept after retention
        // removed its rows; another instance may record it at the same time.
        let now = OffsetDateTime::now_utc();
        for tenant_id in tenants.difference(&known) {
            insert_tenant(&conn, *tenant_id, now).await?;
        }
        tenants.extend(known);
        Ok(tenants)
    }
}

#[async_trait]
impl TenantTables for DeletionTables {
    async fn remove_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<TenantRows, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
        // Each table on its own: a run that stops between them leaves rows
        // the next run removes, and no row of the tenant matters once the
        // platform deleted it.
        let conn = self.db.conn().map_err(DomainError::from)?;
        let subject_settings = SubjectSettingsEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(settings::Column::TenantId.eq(tenant_id)))
            .exec(&conn)
            .await
            .map_err(map_scope_error)?
            .rows_affected;
        // Open requests too: the tenant's profiles go with its graph, so no
        // fact is left for a request to close on.
        let review_requests = ReviewRequestEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(review_requests::Column::TenantId.eq(tenant_id)))
            .exec(&conn)
            .await
            .map_err(map_scope_error)?
            .rows_affected;
        let record_ids = remove_record_ids(&conn, scope, tenant_id, Condition::all()).await?;
        Ok(TenantRows {
            subject_settings,
            review_requests,
            record_ids,
        })
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-rows
    }

    async fn forget_tenant(&self, scope: &AccessScope, tenant_id: Uuid) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
        let conn = self.db.conn().map_err(DomainError::from)?;
        TenantEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(tenants::Column::TenantId.eq(tenant_id)))
            .exec(&conn)
            .await
            .map_err(map_scope_error)?;
        // Only this instance's cache: another instance that noted the tenant
        // does not note it again. Nothing needs it to: a record that lands
        // later leaves a row the job finds the tenant by, and graph storage
        // refuses every profile write of an offboarded tenant. Forgetting it
        // here keeps this instance's cache true to the table.
        self.noted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&tenant_id);
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-forget
    }
}

#[async_trait]
impl ProfileGenerations for DeletionTables {
    async fn generation(&self, key: SubjectKey) -> Result<u64, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-erasure-root-key:p1:inst-root-read
        let conn = self.db.conn().map_err(DomainError::from)?;
        let generation = row(&conn, &AccessScope::for_tenant(key.tenant_id), key)
            .await?
            .map_or(0, |stored| stored.profile_generation);
        generation_of(key, generation)
        // @cpt-end:cpt-cf-construct-algo-erasure-root-key:p1:inst-root-read
    }
}

/// The subject's profile generation as the row stores it.
fn generation_of(key: SubjectKey, generation: i64) -> Result<u64, DomainError> {
    u64::try_from(generation).map_err(|_| {
        DomainError::internal(format!(
            "subject {} of tenant {} has a negative profile generation {generation}",
            key.subject_id, key.tenant_id
        ))
    })
}
