//! Platform policy repositories. Transaction ownership stays in the store.
use crate::infra::storage::entity::{policy, policy_operation_log, policy_version};
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{ColumnTrait, EntityTrait, JoinType, Order, QueryFilter, QuerySelect, RelationTrait};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;

/// The one non-terminal version state a policy pointer may name.
const ACTIVE: &str = "active";

/// Read a header, optionally locking it for a transition.
/// # Errors
/// The secure database error.
pub async fn header(
    runner: &impl DBRunner,
    id: &str,
    lock: bool,
) -> Result<Option<policy::Model>, ScopeError> {
    let mut query = policy::Entity::find().filter(policy::Column::Id.eq(id));
    if lock {
        query = query.lock(LockType::Update);
    }
    query
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(runner)
        .await
}

/// Exact active scope lookup. Fallback must be selected inside one transaction.
/// # Errors
/// The secure database error.
pub async fn at_scope(
    runner: &impl DBRunner,
    scope: &str,
) -> Result<Option<policy::Model>, ScopeError> {
    policy::Entity::find()
        .filter(policy::Column::ScopeKey.eq(scope))
        .filter(policy::Column::ActiveVersion.is_not_null())
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(runner)
        .await
}

/// Exact-scope active version in one statement. Joining the immutable scope to
/// the active row prevents delete/recreate from splitting scope resolution and
/// version resolution across different snapshots.
///
/// # Errors
/// The secure database error.
pub async fn active_at_scope(
    runner: &impl DBRunner,
    scope: &str,
) -> Result<Option<policy_version::Model>, ScopeError> {
    policy_version::Entity::find()
        .join(JoinType::InnerJoin, policy_version::Relation::Policy.def())
        .filter(policy::Column::ScopeKey.eq(scope))
        .filter(policy::Column::ActiveVersion.is_not_null())
        .filter(policy_version::Column::State.eq(ACTIVE))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(runner)
        .await
}

/// The active version of one policy, read by state rather than by following
/// the header's pointer.
///
/// One statement, so the row cannot be retired between reading the pointer and
/// reading the version it names. `idx_qe_policy_one_active` makes "at most one
/// active version per policy" a database guarantee, and every transition moves
/// the state and the pointer in the same transaction, so this agrees with the
/// header without having to read it.
///
/// # Errors
/// The secure database error.
pub async fn active_version(
    runner: &impl DBRunner,
    id: &str,
) -> Result<Option<policy_version::Model>, ScopeError> {
    policy_version::Entity::find()
        .filter(policy_version::Column::PolicyId.eq(id))
        .filter(policy_version::Column::State.eq(ACTIVE))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(runner)
        .await
}

/// Every active version, for the bootstrap engine and catalogue scan. One
/// statement, so the scan sees a single consistent set rather than a sequence
/// of per-policy reads a concurrent transition could straddle.
///
/// # Errors
/// The secure database error.
pub async fn all_active_versions(
    runner: &impl DBRunner,
) -> Result<Vec<policy_version::Model>, ScopeError> {
    policy_version::Entity::find()
        .filter(policy_version::Column::State.eq(ACTIVE))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .order_by(policy_version::Column::PolicyId, Order::Asc)
        .all(runner)
        .await
}

/// Insert a new policy header; the database arbitrates scope occupancy.
/// # Errors
/// Unique violation for an occupied live scope, or another database error.
pub async fn insert_header(
    runner: &impl DBRunner,
    row: policy::ActiveModel,
) -> Result<policy::Model, ScopeError> {
    secure_insert::<policy::Entity>(row, &AccessScope::allow_all(), runner).await
}

/// Read one immutable version.
/// # Errors
/// The secure database error.
pub async fn version(
    runner: &impl DBRunner,
    id: &str,
    number: i64,
) -> Result<Option<policy_version::Model>, ScopeError> {
    policy_version::Entity::find()
        .filter(policy_version::Column::PolicyId.eq(id))
        .filter(policy_version::Column::Version.eq(number))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(runner)
        .await
}

/// Insert an immutable version payload.
/// # Errors
/// The secure database error.
pub async fn insert_version(
    runner: &impl DBRunner,
    row: policy_version::ActiveModel,
) -> Result<policy_version::Model, ScopeError> {
    secure_insert::<policy_version::Entity>(row, &AccessScope::allow_all(), runner).await
}

/// Move a version state; never replace its immutable payload.
/// # Errors
/// The secure database error.
pub async fn set_state(
    runner: &impl DBRunner,
    id: &str,
    number: i64,
    state: &str,
) -> Result<(), ScopeError> {
    policy_version::Entity::update_many()
        .col_expr(policy_version::Column::State, Expr::value(state))
        .filter(policy_version::Column::PolicyId.eq(id))
        .filter(policy_version::Column::Version.eq(number))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(runner)
        .await?;
    Ok(())
}

/// Move a locked header's pointer and high-water mark atomically.
/// # Errors
/// The secure database error.
pub async fn set_pointer(
    runner: &impl DBRunner,
    id: &str,
    active: Option<i64>,
    high_water: i64,
) -> Result<(), ScopeError> {
    policy::Entity::update_many()
        .col_expr(policy::Column::ActiveVersion, Expr::value(active))
        .col_expr(policy::Column::HighWater, Expr::value(high_water))
        .filter(policy::Column::Id.eq(id))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(runner)
        .await?;
    Ok(())
}

/// One bounded history page, ordered by version.
/// # Errors
/// The secure database error.
pub async fn history(
    runner: &impl DBRunner,
    id: &str,
    after: i64,
    limit: u64,
) -> Result<Vec<policy_version::Model>, ScopeError> {
    policy_version::Entity::find()
        .filter(policy_version::Column::PolicyId.eq(id))
        .filter(policy_version::Column::Version.gt(after))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .order_by(policy_version::Column::Version, Order::Asc)
        .limit(limit)
        .all(runner)
        .await
}

/// Append transition audit in the owning mutation transaction.
/// # Errors
/// The secure database error.
pub async fn append_audit(
    runner: &impl DBRunner,
    row: policy_operation_log::ActiveModel,
) -> Result<(), ScopeError> {
    secure_insert::<policy_operation_log::Entity>(row, &AccessScope::allow_all(), runner).await?;
    Ok(())
}
