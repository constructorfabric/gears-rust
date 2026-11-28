//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-quota-rules-store:p1
//! Scoped persistence of a revision's quota rules and their set row (D-541, D-543, D-545, D-546).
//!
//! A revision's set is its rules and one set row with the digest its writer computed: no read
//! computes a digest again (D-541). A revision holds a set row exactly when it holds a rule: a
//! replace with no rules deletes the set row, and a revision without a set row reads the empty set
//! and the empty set's digest. Every write runs in the caller's transaction. The draft fence is
//! the conditional revision write that [`replace`] makes first (`plan_revision_repo::bump_draft`);
//! the item fence is the rule's foreign key to its item (D-543).
//!
//! **One snapshot.** A set is read in two statements, the set rows and the rules, which must
//! agree. [`for_revision`] and [`for_revisions`] therefore take the caller's transaction (a
//! [`DbTx`], which a plain connection is not), so both statements see one snapshot: on Postgres
//! each statement on a connection takes its own, and a replace committed between the two would
//! read as a corrupt row or as another set's digest. That transaction is one serializable
//! transaction, as every Pricing transaction is (`support::transaction_door_read` for a read): a
//! default-isolation transaction on Postgres also gives each statement its own snapshot.
use super::driver_failure;
use crate::domain::plan::RevisionState;
use crate::infra::storage::{
    RepoError,
    entity::{plan_quota_rule as rule_e, plan_quota_set as set_e, plan_revision},
};
use sea_orm::sea_query::{Expr, Query, SelectStatement};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use std::collections::BTreeMap;
use std::sync::LazyLock;
use toolkit_db::DbTx;
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

/// A revision's stored set: its digest as written, its rules ordered by `rule_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSet {
    /// The set digest as its writer computed it: 64 lowercase hex characters.
    pub digest: String,
    /// The rules, ordered by `rule_id`.
    pub rules: Vec<rule_e::Model>,
}

/// The empty set's digest, computed once.
static EMPTY_DIGEST: LazyLock<String> = LazyLock::new(|| {
    crate::infra::usage_policy_wire::digest_text(bss_pricing_sdk::digest::quota_rules_digest(&[]))
});

/// The lowercase hex digest of the empty set: a revision without a set row reads it.
#[must_use]
pub fn empty_digest() -> String {
    EMPTY_DIGEST.clone()
}

fn rules_of(tenant: Uuid, revision: Uuid) -> Condition {
    Condition::all()
        .add(rule_e::Column::TenantId.eq(tenant))
        .add(rule_e::Column::RevisionId.eq(revision))
}

fn set_of(tenant: Uuid, revision: Uuid) -> Condition {
    Condition::all()
        .add(set_e::Column::TenantId.eq(tenant))
        .add(set_e::Column::RevisionId.eq(revision))
}

/// The tenant's unlocked draft revisions: only their rules are deleted.
fn unlocked_drafts(tenant: Uuid) -> SelectStatement {
    Query::select()
        .column(plan_revision::Column::Id)
        .from(plan_revision::Entity)
        .and_where(plan_revision::Column::TenantId.eq(tenant))
        .and_where(plan_revision::Column::State.eq(RevisionState::Draft.as_str()))
        .and_where(plan_revision::Column::PendingUnitId.is_null())
        .to_owned()
}

/// One revision's set from its set row and its rules: no set row and no rule is the empty set. A
/// rule without a set row, a set row without a rule, or a count that disagrees with the rules is a
/// corrupt row: a revision holds a set row exactly when it holds a rule.
fn stored(
    revision: Uuid,
    set: Option<set_e::Model>,
    rules: Vec<rule_e::Model>,
) -> Result<StoredSet, RepoError> {
    let Some(set) = set else {
        if rules.is_empty() {
            return Ok(StoredSet {
                digest: empty_digest(),
                rules,
            });
        }
        return Err(RepoError::CorruptRow(format!(
            "plan quota rules of revision {revision} without a set row"
        )));
    };
    if rules.is_empty() || usize::try_from(set.rule_count).ok() != Some(rules.len()) {
        return Err(RepoError::CorruptRow(format!(
            "plan quota set of revision {revision} counts {} rules, {} are stored",
            set.rule_count,
            rules.len()
        )));
    }
    Ok(StoredSet {
        digest: set.quota_rules_digest,
        rules,
    })
}

/// One revision's set, TWO statements in the caller's transaction (one snapshot); no set row is
/// the empty set.
/// # Errors
/// Storage failures; `CorruptRow` for rules without a set row, a set row without rules or a count
/// that disagrees.
pub async fn for_revision(
    runner: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    revision: Uuid,
) -> Result<StoredSet, RepoError> {
    let set = set_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(set_of(tenant, revision))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find plan quota set".into(), e))?;
    let rules = rule_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(rules_of(tenant, revision))
        .order_by(rule_e::Column::RuleId, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan quota rules of a revision".into(), e))?;
    stored(revision, set, rules)
}

/// The sets of every revision among `revisions`, TWO statements in the caller's transaction (one
/// snapshot) whatever their number (an empty list included); a revision without rules is absent
/// from the map, and its caller answers [`empty_digest`] for it.
/// # Errors
/// Storage failures; `CorruptRow` for rules without a set row, a set row without rules or a count
/// that disagrees.
pub async fn for_revisions(
    runner: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    revisions: &[Uuid],
) -> Result<BTreeMap<Uuid, StoredSet>, RepoError> {
    let sets = set_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(set_e::Column::TenantId.eq(tenant))
                .add(set_e::Column::RevisionId.is_in(revisions.iter().copied())),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan quota sets of revisions".into(), e))?;
    let rules = rule_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(rule_e::Column::TenantId.eq(tenant))
                .add(rule_e::Column::RevisionId.is_in(revisions.iter().copied())),
        )
        .order_by(rule_e::Column::RevisionId, Order::Asc)
        .order_by(rule_e::Column::RuleId, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan quota rules of revisions".into(), e))?;
    let mut sets: BTreeMap<Uuid, set_e::Model> =
        sets.into_iter().map(|s| (s.revision_id, s)).collect();
    let mut grouped: BTreeMap<Uuid, Vec<rule_e::Model>> = BTreeMap::new();
    for rule in rules {
        grouped.entry(rule.revision_id).or_default().push(rule);
    }
    let mut out = BTreeMap::new();
    for (revision, rules) in grouped {
        out.insert(revision, stored(revision, sets.remove(&revision), rules)?);
    }
    // A set row left over names no rule: a corrupt row.
    for (revision, set) in sets {
        stored(revision, Some(set), Vec::new())?;
    }
    Ok(out)
}

fn rule_count(rules: &[rule_e::Model]) -> Result<i32, RepoError> {
    i32::try_from(rules.len())
        .map_err(|_| RepoError::Db(format!("{} plan quota rules in one set", rules.len())))
}

async fn insert_rules(
    runner: &impl DBRunner,
    scope: &AccessScope,
    rules: Vec<rule_e::Model>,
) -> Result<(), RepoError> {
    let models = rules
        .into_iter()
        .map(|m| rule_e::ActiveModel {
            revision_id: Set(m.revision_id),
            rule_id: Set(m.rule_id),
            tenant_id: Set(m.tenant_id),
            item_id: Set(m.item_id),
            definition_id: Set(m.definition_id),
            definition_version: Set(m.definition_version),
            definition_digest: Set(m.definition_digest),
            dimension_value: Set(m.dimension_value),
            logical_binding_key: Set(m.logical_binding_key),
            subject_selector: Set(m.subject_selector),
            scope_selection_json: Set(m.scope_selection_json),
            amount: Set(m.amount),
            unit_ref: Set(m.unit_ref),
            basis: Set(m.basis),
            evidence_json: Set(m.evidence_json),
            availability_json: Set(m.availability_json),
            created_at: Set(m.created_at),
        })
        .collect();
    toolkit_db::secure::secure_insert_many::<rule_e::Entity>(models, scope, runner)
        .await
        .map_err(|e| driver_failure("insert plan quota rules".into(), e))
}

/// The revision's set row, written again: updated when it exists, else inserted.
#[expect(
    clippy::too_many_arguments,
    reason = "the set row's key, its content and its writer are the write's operands"
)]
async fn write_set(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revision: Uuid,
    digest: String,
    count: i32,
    by: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let updated = set_e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(set_e::Column::QuotaRulesDigest, Expr::value(digest.clone()))
        .col_expr(set_e::Column::RuleCount, Expr::value(count))
        .col_expr(set_e::Column::UpdatedBy, Expr::value(by))
        .col_expr(set_e::Column::UpdatedAt, Expr::value(now))
        .filter(set_of(tenant, revision))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("update plan quota set".into(), e))?;
    if updated.rows_affected == 1 {
        return Ok(());
    }
    let active = set_e::ActiveModel {
        revision_id: Set(revision),
        tenant_id: Set(tenant),
        quota_rules_digest: Set(digest),
        rule_count: Set(count),
        updated_by: Set(by),
        updated_at: Set(now),
    };
    set_e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert plan quota set scope".into(), e))?
        .exec(runner)
        .await
        .map_err(|e| driver_failure("insert plan quota set".into(), e))?;
    Ok(())
}

/// Replace a draft's whole set at the version the caller read (D-543): the revision's next version
/// first (`bump_draft`), then the rules deleted and inserted, then the set row written again with
/// `digest`, which the caller computed over `rules`. A set with no rules deletes the set row, so
/// the revision reads the empty set. Every row of `rules` is a rule of `revision` in `tenant`.
/// Answers the new revision version.
/// # Errors
/// `STALE_REVISION` (the bump: a lost version, a locked or a non-draft revision); a foreign key or
/// CHECK refusal keeps its typed driver error. A row of another revision or tenant, or an empty set
/// whose digest is not [`empty_digest`], is refused before anything is written.
#[expect(
    clippy::too_many_arguments,
    reason = "the revision's fence, the set and its writer are the write's operands"
)]
pub async fn replace(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revision: Uuid,
    seen_version: i64,
    rules: Vec<rule_e::Model>,
    digest: String,
    by: Uuid,
    now: time::OffsetDateTime,
) -> Result<i64, RepoError> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-commit
    if let Some(stranger) = rules
        .iter()
        .find(|r| r.tenant_id != tenant || r.revision_id != revision)
    {
        return Err(RepoError::Db(format!(
            "plan quota rule {} of revision {} given to revision {revision}",
            stranger.rule_id, stranger.revision_id
        )));
    }
    if rules.is_empty() && digest != *EMPTY_DIGEST {
        return Err(RepoError::Db(format!(
            "an empty plan quota set of revision {revision} given the digest {digest}"
        )));
    }
    let count = rule_count(&rules)?;
    let version =
        super::plan_revision_repo::bump_draft(runner, scope, tenant, revision, seen_version, now)
            .await?;
    rule_e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(rules_of(tenant, revision))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete plan quota rules of a draft".into(), e))?;
    if count == 0 {
        set_e::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(set_of(tenant, revision))
            .exec(runner)
            .await
            .map_err(|e| driver_failure("delete plan quota set of a cleared draft".into(), e))?;
    } else {
        insert_rules(runner, scope, rules).await?;
        write_set(runner, scope, tenant, revision, digest, count, by, now).await?;
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plan-quota-rules-replace:p1:inst-quota-commit
    Ok(version)
}

/// Copy `source`'s set into the new draft `target`, each rule onto `items[old item]` with its rule
/// id, definition reference, evidence and observation, and the digest given, which the caller
/// computed over the moved rules (D-545). A source without rules writes nothing: the target reads
/// the empty set. The target's version does not move.
/// # Errors
/// A rule whose item is not in `items` is `CorruptRow`; storage failures.
#[expect(
    clippy::too_many_arguments,
    reason = "both revisions, the item map, the digest and the writer are the copy's operands"
)]
pub async fn copy(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    source: Uuid,
    target: Uuid,
    items: &BTreeMap<Uuid, Uuid>,
    digest: String,
    by: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let rules = rule_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(rules_of(tenant, source))
        .order_by(rule_e::Column::RuleId, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list plan quota rules to copy".into(), e))?;
    if rules.is_empty() {
        return Ok(());
    }
    let count = rule_count(&rules)?;
    let mut moved = Vec::with_capacity(rules.len());
    for rule in rules {
        let Some(item) = items.get(&rule.item_id).copied() else {
            return Err(RepoError::CorruptRow(format!(
                "plan quota rule {} of revision {source} names item {}, which the copy does not \
                 carry",
                rule.rule_id, rule.item_id
            )));
        };
        moved.push(rule_e::Model {
            revision_id: target,
            item_id: item,
            created_at: now,
            ..rule
        });
    }
    insert_rules(runner, scope, moved).await?;
    write_set(runner, scope, tenant, target, digest, count, by, now).await
}

/// Remove a draft's rules and set row, before its items are removed. Only an unlocked draft's
/// rules go: for any other revision this deletes nothing, and the caller's revision delete then
/// refuses.
/// # Errors
/// Storage failures.
pub async fn delete_for_draft(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    revision: Uuid,
) -> Result<(), RepoError> {
    rule_e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            rules_of(tenant, revision)
                .add(rule_e::Column::RevisionId.in_subquery(unlocked_drafts(tenant))),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete plan quota rules of a deleted draft".into(), e))?;
    set_e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            set_of(tenant, revision)
                .add(set_e::Column::RevisionId.in_subquery(unlocked_drafts(tenant))),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete plan quota set of a deleted draft".into(), e))?;
    Ok(())
}

/// Whether any rule names `item`, ONE statement.
/// # Errors
/// Storage failures.
pub async fn item_has_rules(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    item: Uuid,
) -> Result<bool, RepoError> {
    rule_e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(rule_e::Column::TenantId.eq(tenant))
                .add(rule_e::Column::ItemId.eq(item)),
        )
        .one(runner)
        .await
        .map(|rule| rule.is_some())
        .map_err(|e| driver_failure("find a plan quota rule of an item".into(), e))
}

#[cfg(test)]
#[path = "plan_quota_repo_tests.rs"]
mod plan_quota_repo_tests;
