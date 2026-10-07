//! Scoped append-only policy persistence. There is deliberately no update or delete API.
use super::driver_failure;
use crate::{
    domain::usage_policy::{entry_policy_key, validate_policy_shape},
    infra::{
        storage::{
            RepoError,
            entity::{price_book_entry, usage_rating_policy as e},
        },
        usage_policy_wire::{UsageRatingPolicy, UsageRatingPolicyInput, digest_text},
    },
};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, Set, sea_query::OnConflict};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

fn corrupt(detail: impl Into<String>) -> RepoError {
    RepoError::CorruptRow(detail.into())
}
/// Verify the persisted content against its immutable digest before returning it.
/// # Errors
/// Invalid stored shape, reference or digest is an integrity failure.
pub fn decode(row: e::Model) -> Result<UsageRatingPolicy, RepoError> {
    let content: UsageRatingPolicyInput = serde_json::from_value(row.content).map_err(|error| {
        corrupt(format!(
            "policy {} version {} content: {error}",
            row.policy_id, row.version
        ))
    })?;
    let typed = (&content).into();
    validate_policy_shape(&typed).map_err(|error| {
        corrupt(format!(
            "policy {} version {} shape {}: {}",
            row.policy_id, row.version, error.code, error
        ))
    })?;
    if row.version <= 0 || digest_text(entry_policy_key(&typed)) != row.digest {
        return Err(corrupt(format!(
            "policy {} version {} digest {}",
            row.policy_id, row.version, row.digest
        )));
    }
    Ok(UsageRatingPolicy {
        policy_id: row.policy_id,
        version: row.version.to_string(),
        digest: row.digest,
        content,
    })
}
/// Insert or reuse canonical content in the caller's entry-create transaction.
/// # Errors
/// Digest collisions with unequal content are integrity failures; storage errors preserve type.
pub async fn intern(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    actor: Uuid,
    content: &UsageRatingPolicyInput,
    now: time::OffsetDateTime,
) -> Result<UsageRatingPolicy, RepoError> {
    let typed = content.into();
    validate_policy_shape(&typed)
        .map_err(|error| corrupt(format!("policy shape {}: {error}", error.code)))?;
    let digest = digest_text(entry_policy_key(&typed));
    let model = e::ActiveModel {
        tenant_id: Set(tenant),
        policy_id: Set(Uuid::now_v7()),
        version: Set(1),
        digest: Set(digest.clone()),
        content: Set(serde_json::to_value(content)
            .map_err(|error| corrupt(format!("policy content: {error}")))?),
        created_at: Set(now),
        created_by: Set(actor),
    };
    let result = e::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("policy insert scope".into(), e))?
        .on_conflict_raw(
            OnConflict::columns([e::Column::TenantId, e::Column::Digest])
                .do_nothing()
                .to_owned(),
        )
        .exec(tx)
        .await;
    match result {
        Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => {}
        Err(e) => return Err(driver_failure("policy insert".into(), e)),
    }
    let row = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::Digest.eq(digest.clone())),
        )
        .one(tx)
        .await
        .map_err(|e| driver_failure("policy dedup read".into(), e))?
        .ok_or_else(|| corrupt(format!("policy digest {digest} was not stored")))?;
    let policy = decode(row)?;
    if &policy.content != content {
        return Err(corrupt(format!(
            "policy {} version {} digest collision",
            policy.policy_id, policy.version
        )));
    }
    Ok(policy)
}
/// Materialize policies for an entry batch in one tenant-scoped statement.
/// # Errors
/// A dangling, partial or digest-mismatched reference is an integrity failure.
pub async fn for_entries(
    tx: &impl DBRunner,
    tenant: Uuid,
    entries: &[price_book_entry::Model],
) -> Result<std::collections::BTreeMap<Uuid, UsageRatingPolicy>, RepoError> {
    let ids: Vec<Uuid> = entries.iter().filter_map(|e| e.usage_policy_id).collect();
    let rows = if ids.is_empty() {
        Vec::new()
    } else {
        e::Entity::find()
            .secure()
            .scope_with(&AccessScope::for_tenant(tenant))
            .filter(
                Condition::all()
                    .add(e::Column::TenantId.eq(tenant))
                    .add(e::Column::PolicyId.is_in(ids)),
            )
            .all(tx)
            .await
            .map_err(|e| driver_failure("entry policies".into(), e))?
    };
    let mut policies = std::collections::BTreeMap::new();
    for row in rows {
        let key = (row.policy_id, row.version);
        policies.insert(key, decode(row)?);
    }
    let mut result = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.tenant_id != tenant {
            return Err(corrupt(format!(
                "entry {} tenant {} is not {tenant}",
                entry.id, entry.tenant_id
            )));
        }
        match (
            entry.usage_policy_id,
            entry.usage_policy_version,
            &entry.usage_policy_digest,
        ) {
            (None, None, None) => {}
            (Some(id), Some(version), Some(digest)) => {
                let policy = policies.get(&(id, version)).ok_or_else(|| {
                    corrupt(format!(
                        "entry {} policy {id} version {version} is absent",
                        entry.id
                    ))
                })?;
                if &policy.digest != digest {
                    return Err(corrupt(format!(
                        "entry {} policy {id} version {version} digest {digest}",
                        entry.id
                    )));
                }
                result.insert(entry.id, policy.clone());
            }
            _ => {
                return Err(corrupt(format!(
                    "entry {} policy reference is partial",
                    entry.id
                )));
            }
        }
    }
    Ok(result)
}
