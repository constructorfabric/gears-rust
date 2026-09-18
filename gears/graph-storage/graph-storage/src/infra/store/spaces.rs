//! Boot-time resolution of the deployment's active embedding space.
//!
//! Runs once, at init, and the result is a number the request paths carry in
//! memory: no read of `embedding_space` ever happens on a request. That is
//! deliberate — the active space cannot change while the process runs, since
//! changing it is the model-migration lifecycle (ADR-0005), and re-reading it
//! per request would only invite a different answer mid-batch.

use graph_storage_sdk::models::EmbeddingSpaceId;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{Db, SecureEntityExt, SecureInsertExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::storage::entity::embedding_space;
use crate::infra::storage::migrations::m0003_embedding_space::{FIRST_EPOCH, STATE_ACTIVE};

/// What boot decided about the deployment's vectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceResolution {
    /// The active space is the provider's own. `epoch` stamps every vector it
    /// writes, and the vector arm serves exactly this epoch.
    Active { epoch: i64 },
    /// A space is recorded and the active provider is not it. Vector search is
    /// refused until a re-embedding migration reconciles them; every other
    /// path is unaffected, because only vectors are incomparable.
    Mismatched {
        recorded_identity: String,
        recorded_epoch: i64,
    },
}

/// The nil tenant, under which the deployment-wide row lives.
fn deployment_scope() -> AccessScope {
    AccessScope::for_tenant(Uuid::nil())
}

/// Read the active space, opening one on first boot.
///
/// A first boot writes the provider's identity and adopts it. A later boot
/// with the same identity adopts the recorded epoch. A later boot with a
/// *different* identity does **not** open a new epoch: opening one silently
/// would strand every stored vector in a space nothing searches, which is
/// exactly the invisible corruption ADR-0005 refuses. It reports the mismatch
/// and leaves the decision to the operator.
///
/// # Errors
///
/// Propagates storage failures; a deployment that cannot read its own
/// embedding space must not boot into a guess.
pub async fn resolve(db: &Db, space: &EmbeddingSpaceId) -> anyhow::Result<SpaceResolution> {
    let scope = deployment_scope();
    let conn = db.conn()?;

    if let Some(row) = active_row(&conn, &scope).await? {
        return Ok(decide(row, space));
    }

    // Two replicas booting together both find nothing and both insert. The
    // partial unique index lets exactly one win, so a failure here is first
    // read as "someone else opened the space" and only reported if it was
    // not: an insert that raced is not an error, and an insert that failed
    // for any other reason must not be swallowed into a guess.
    if let Err(error) = open_first_space(&conn, &scope, space).await {
        return match active_row(&conn, &scope).await? {
            Some(row) => Ok(decide(row, space)),
            None => Err(error),
        };
    }

    Ok(SpaceResolution::Active { epoch: FIRST_EPOCH })
}

fn decide(row: embedding_space::Model, space: &EmbeddingSpaceId) -> SpaceResolution {
    if row.identity_hash == space.identity_hash {
        SpaceResolution::Active { epoch: row.epoch }
    } else {
        SpaceResolution::Mismatched {
            recorded_identity: row.identity_hash,
            recorded_epoch: row.epoch,
        }
    }
}

async fn active_row(
    conn: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
) -> anyhow::Result<Option<embedding_space::Model>> {
    Ok(embedding_space::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(embedding_space::Column::State.eq(STATE_ACTIVE)))
        .one(conn)
        .await?)
}

async fn open_first_space(
    conn: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    space: &EmbeddingSpaceId,
) -> anyhow::Result<()> {
    let now = time::OffsetDateTime::now_utc();
    let active = embedding_space::ActiveModel {
        tenant_id: ActiveValue::Set(Uuid::nil()),
        epoch: ActiveValue::Set(FIRST_EPOCH),
        identity_hash: ActiveValue::Set(space.identity_hash.clone()),
        model_artifact: ActiveValue::Set(space.model_artifact.clone()),
        tokenizer_artifact: ActiveValue::Set(space.tokenizer_artifact.clone()),
        preprocessing: ActiveValue::Set(space.preprocessing.clone()),
        pooling: ActiveValue::Set(space.pooling.clone()),
        normalization: ActiveValue::Set(space.normalization.clone()),
        dimension: ActiveValue::Set(i32::try_from(space.dimension).unwrap_or(i32::MAX)),
        state: ActiveValue::Set(STATE_ACTIVE.to_owned()),
        created_at: ActiveValue::Set(now),
        activated_at: ActiveValue::Set(Some(now)),
    };
    // scope_unchecked: an INSERT cannot subtree-clamp a row that does not
    // exist yet. Same reasoning as type registration.
    embedding_space::Entity::insert(active)
        .secure()
        .scope_unchecked(scope)?
        .exec(conn)
        .await?;
    Ok(())
}
