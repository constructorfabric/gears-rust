//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-store:p1
//! @cpt-dod:cpt-cf-bss-products-dod-quota-set-validation:p1
//! Quota catalog persistence (P-D-270, P-D-271, P-D-272, P-D-274): mapping versions with their
//! profile, definition versions with their resource types, and availability observations, every
//! read and write scoped by the caller's `AccessScope` and the tenant.
//!
//! Every row here is inserted once and never written again: the tables' triggers refuse every
//! `UPDATE` and `DELETE`. A definition version's current availability is its highest revision, and
//! a list reads it for the whole page in one grouped statement, never once per row. Each version
//! column converts to the SDK's [`QuotaVersion`] with a checked conversion; a stored row that does
//! not decode is a corrupt row naming its table, tenant and id (`quota_intake_repo`).
use super::quota_intake_repo::{
    QUOTA_PAGE, corrupt, list_error, object_to_store, stored_object, stored_version,
    version_to_store,
};
use super::{SkuListError, driver_failure};
use crate::domain::quota::{Availability, DefinitionFilter, DefinitionVersion, MappingVersion};
use crate::infra::storage::{
    RepoError, RepoRefusal,
    entity::{
        quota_availability, quota_definition_resource_type, quota_definition_version,
        quota_mapping_version,
    },
};
use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaOwnerRef, QuotaVersion};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query, SelectStatement};
use sea_orm::{ColumnTrait, Condition, EntityTrait, FromQueryResult, Order, QuerySelect, Set};
use std::collections::{BTreeSet, HashMap};
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping, paginate_odata_try};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, secure_insert_many,
};
use toolkit_odata::filter::{FieldKind, FilterField, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page, SortDir};
use uuid::Uuid;

const MAPPING_TABLE: &str = "products_quota_mapping_version";
const DEFINITION_TABLE: &str = "products_quota_definition_version";
const AVAILABILITY_TABLE: &str = "products_quota_availability";

/// A definition version and its current availability, as the reads answer them.
pub type DefinitionRow = (DefinitionVersion, Availability);

/// Any unique violation of an approval's rows: a concurrent approval took the number (P-D-270).
fn contended(context: &str, e: ScopeError) -> RepoError {
    if e.is_unique_violation() {
        return RepoError::Refused(RepoRefusal::QuotaApprovalContended);
    }
    driver_failure(context.to_owned(), e)
}

/// The stored token of an availability state.
const fn state_token(state: QuotaAvailabilityState) -> &'static str {
    match state {
        QuotaAvailabilityState::Enabled => "enabled",
        QuotaAvailabilityState::Disabled => "disabled",
        QuotaAvailabilityState::Withdrawn => "withdrawn",
    }
}

fn parse_state(token: &str) -> Option<QuotaAvailabilityState> {
    match token {
        "enabled" => Some(QuotaAvailabilityState::Enabled),
        "disabled" => Some(QuotaAvailabilityState::Disabled),
        "withdrawn" => Some(QuotaAvailabilityState::Withdrawn),
        _ => None,
    }
}

fn mapping_of(m: quota_mapping_version::Model) -> Result<MappingVersion, RepoError> {
    let (t, key) = (
        m.tenant_id,
        format!("{} version {}", m.mapping_id, m.version),
    );
    Ok(MappingVersion {
        mapping_id: m.mapping_id,
        version: stored_version(m.version, MAPPING_TABLE, t, &key, "version")?,
        target_id: m.target_id,
        capability_key: m.capability_key,
        plugin_instance_id: m.plugin_instance_id,
        target_key: m.target_key,
        mapping_key: m.mapping_key,
        mapping_json: stored_object(m.mapping_json, MAPPING_TABLE, t, &key, "mapping_json")?,
        digest: m.digest,
        profile_id: m.profile_id,
        profile_version: stored_version(
            m.profile_version,
            MAPPING_TABLE,
            t,
            &key,
            "profile_version",
        )?,
        profile_json: stored_object(m.profile_json, MAPPING_TABLE, t, &key, "profile_json")?,
        profile_digest: m.profile_digest,
        created_by: m.created_by,
        created_at: m.created_at,
    })
}

fn definition_of(m: quota_definition_version::Model) -> Result<DefinitionVersion, RepoError> {
    let (t, key) = (
        m.tenant_id,
        format!("{} version {}", m.definition_id, m.version),
    );
    Ok(DefinitionVersion {
        definition_id: m.definition_id,
        version: stored_version(m.version, DEFINITION_TABLE, t, &key, "version")?,
        candidate_id: m.candidate_id,
        sku_id: m.sku_id,
        sku_version: stored_version(m.sku_version, DEFINITION_TABLE, t, &key, "sku_version")?,
        metric_uuid: m.metric_uuid,
        subject_type_uuid: m.subject_type_uuid,
        profile_id: m.profile_id,
        mapping_id: m.mapping_id,
        mapping_version: stored_version(
            m.mapping_version,
            DEFINITION_TABLE,
            t,
            &key,
            "mapping_version",
        )?,
        content_json: stored_object(m.content_json, DEFINITION_TABLE, t, &key, "content_json")?,
        digest: m.digest,
        created_by: m.created_by,
        created_at: m.created_at,
    })
}

fn availability_of(m: quota_availability::Model) -> Result<Availability, RepoError> {
    let (t, key) = (
        m.tenant_id,
        format!(
            "{} version {} revision {}",
            m.definition_id, m.version, m.revision
        ),
    );
    let state = parse_state(&m.state)
        .ok_or_else(|| corrupt(AVAILABILITY_TABLE, t, &key, &format!("state {}", m.state)))?;
    let qualification = match (
        m.qualification_id,
        m.qualification_version,
        m.qualification_digest,
    ) {
        (None, None, None) => None,
        (Some(id), Some(version), Some(digest)) => Some(QuotaOwnerRef {
            id,
            version: stored_version(
                version,
                AVAILABILITY_TABLE,
                t,
                &key,
                "qualification_version",
            )?,
            digest,
        }),
        _ => {
            return Err(corrupt(
                AVAILABILITY_TABLE,
                t,
                &key,
                "a partial qualification",
            ));
        }
    };
    Ok(Availability {
        definition_id: m.definition_id,
        version: stored_version(m.version, AVAILABILITY_TABLE, t, &key, "version")?,
        revision: stored_version(m.revision, AVAILABILITY_TABLE, t, &key, "revision")?,
        state,
        reason_code: m.reason_code,
        qualification,
        observed_at: m.observed_at,
        decided_by: m.decided_by,
    })
}

fn lineage(tenant_id: Uuid, target_id: Uuid) -> Condition {
    Condition::all()
        .add(quota_mapping_version::Column::TenantId.eq(tenant_id))
        .add(quota_mapping_version::Column::TargetId.eq(target_id))
}

/// The latest mapping version of the lineage `(target, capability_key)`.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn latest_mapping(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    target_id: Uuid,
    capability_key: &str,
) -> Result<Option<MappingVersion>, RepoError> {
    quota_mapping_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            lineage(tenant_id, target_id)
                .add(quota_mapping_version::Column::CapabilityKey.eq(capability_key)),
        )
        .order_by(quota_mapping_version::Column::Version, Order::Desc)
        .limit(1)
        .one(runner)
        .await
        .map_err(|e| driver_failure("latest quota mapping".into(), e))?
        .map(mapping_of)
        .transpose()
}

/// `(capability_key, max(version))` of the target's lineages, one grouped read.
fn latest_mapping_numbers(tenant_id: Uuid, target_id: Uuid) -> SelectStatement {
    Query::select()
        .column(quota_mapping_version::Column::CapabilityKey)
        .expr(
            Expr::col((
                quota_mapping_version::Entity,
                quota_mapping_version::Column::Version,
            ))
            .max(),
        )
        .from(quota_mapping_version::Entity)
        .and_where(quota_mapping_version::Column::TenantId.eq(tenant_id))
        .and_where(quota_mapping_version::Column::TargetId.eq(target_id))
        .group_by_col(quota_mapping_version::Column::CapabilityKey)
        .to_owned()
}

/// The latest mapping version of each lineage of the target, by capability key, in ONE statement.
/// # Errors
/// Scoped storage failures; a stored row that does not decode.
pub async fn approved_mappings_of_target(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    target_id: Uuid,
) -> Result<Vec<MappingVersion>, RepoError> {
    quota_mapping_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            lineage(tenant_id, target_id).add(
                Expr::tuple([
                    Expr::col((
                        quota_mapping_version::Entity,
                        quota_mapping_version::Column::CapabilityKey,
                    )),
                    Expr::col((
                        quota_mapping_version::Entity,
                        quota_mapping_version::Column::Version,
                    )),
                ])
                .in_subquery(latest_mapping_numbers(tenant_id, target_id)),
            ),
        )
        .order_by(quota_mapping_version::Column::CapabilityKey, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("approved quota mappings".into(), e))?
        .into_iter()
        .map(mapping_of)
        .collect()
}

/// Insert a mapping version as given.
/// # Errors
/// `CONTENDED` (`QuotaApprovalContended`) when a concurrent approval took the version; a JSON
/// column that is not an object; a version beyond the stored range; scoped storage failures.
pub async fn insert_mapping(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    mapping: &MappingVersion,
) -> Result<(), RepoError> {
    let model = quota_mapping_version::ActiveModel {
        tenant_id: Set(tenant_id),
        mapping_id: Set(mapping.mapping_id),
        version: Set(version_to_store(mapping.version, "mapping version")?),
        target_id: Set(mapping.target_id),
        capability_key: Set(mapping.capability_key.clone()),
        plugin_instance_id: Set(mapping.plugin_instance_id.clone()),
        target_key: Set(mapping.target_key.clone()),
        mapping_key: Set(mapping.mapping_key.clone()),
        mapping_json: Set(object_to_store(
            &mapping.mapping_json,
            MAPPING_TABLE,
            "mapping_json",
        )?),
        digest: Set(mapping.digest.clone()),
        profile_id: Set(mapping.profile_id),
        profile_version: Set(version_to_store(
            mapping.profile_version,
            "profile version",
        )?),
        profile_json: Set(object_to_store(
            &mapping.profile_json,
            MAPPING_TABLE,
            "profile_json",
        )?),
        profile_digest: Set(mapping.profile_digest.clone()),
        created_by: Set(mapping.created_by),
        created_at: Set(mapping.created_at),
    };
    quota_mapping_version::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("quota mapping scope".into(), e))?
        .exec(runner)
        .await
        .map(|_| ())
        .map_err(|e| contended("insert quota mapping", e))
}

#[derive(Debug, FromQueryResult)]
struct HighestVersion {
    version: Option<i64>,
}

/// The definition's highest version, in ONE statement; `None` for a definition without one.
/// # Errors
/// Scoped storage failures; a stored version that does not decode.
pub async fn latest_definition_version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    definition_id: Uuid,
) -> Result<Option<QuotaVersion>, RepoError> {
    let rows: Vec<HighestVersion> = quota_definition_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(quota_definition_version::Column::TenantId.eq(tenant_id))
                .add(quota_definition_version::Column::DefinitionId.eq(definition_id)),
        )
        .project_all(runner, |query| {
            query
                .select_only()
                .column_as(
                    Expr::col(quota_definition_version::Column::Version).max(),
                    "version",
                )
                .into_model::<HighestVersion>()
        })
        .await
        .map_err(|e| driver_failure("latest quota definition version".into(), e))?;
    rows.into_iter()
        .find_map(|row| row.version)
        .map(|v| stored_version(v, DEFINITION_TABLE, tenant_id, definition_id, "version"))
        .transpose()
}

/// Insert a definition version and the resource types it counts, each once.
/// # Errors
/// `CONTENDED` (`QuotaApprovalContended`) when a concurrent approval took the version or
/// already published this candidate; a JSON column that is not an object; a version beyond the
/// stored range; scoped storage failures.
pub async fn insert_definition(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    definition: &DefinitionVersion,
    resource_type_uuids: &[Uuid],
) -> Result<(), RepoError> {
    let version = version_to_store(definition.version, "definition version")?;
    let model = quota_definition_version::ActiveModel {
        tenant_id: Set(tenant_id),
        definition_id: Set(definition.definition_id),
        version: Set(version),
        candidate_id: Set(definition.candidate_id),
        sku_id: Set(definition.sku_id),
        sku_version: Set(version_to_store(definition.sku_version, "SKU version")?),
        metric_uuid: Set(definition.metric_uuid),
        subject_type_uuid: Set(definition.subject_type_uuid),
        profile_id: Set(definition.profile_id),
        mapping_id: Set(definition.mapping_id),
        mapping_version: Set(version_to_store(
            definition.mapping_version,
            "mapping version",
        )?),
        content_json: Set(object_to_store(
            &definition.content_json,
            DEFINITION_TABLE,
            "content_json",
        )?),
        digest: Set(definition.digest.clone()),
        created_by: Set(definition.created_by),
        created_at: Set(definition.created_at),
    };
    quota_definition_version::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("quota definition scope".into(), e))?
        .exec(runner)
        .await
        .map_err(|e| contended("insert quota definition", e))?;
    let types: BTreeSet<Uuid> = resource_type_uuids.iter().copied().collect();
    let rows = types
        .into_iter()
        .map(
            |resource_type_uuid| quota_definition_resource_type::ActiveModel {
                tenant_id: Set(tenant_id),
                definition_id: Set(definition.definition_id),
                version: Set(version),
                resource_type_uuid: Set(resource_type_uuid),
            },
        )
        .collect();
    secure_insert_many::<quota_definition_resource_type::Entity>(rows, scope, runner)
        .await
        .map_err(|e| driver_failure("insert quota definition resource types".into(), e))
}

/// Insert an availability observation as given.
/// # Errors
/// `CONTENDED` (`QuotaApprovalContended`) when a concurrent write took the revision; a version
/// beyond the stored range; scoped storage failures.
pub async fn insert_availability(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    availability: &Availability,
) -> Result<(), RepoError> {
    let qualification = availability.qualification.as_ref();
    let model = quota_availability::ActiveModel {
        tenant_id: Set(tenant_id),
        definition_id: Set(availability.definition_id),
        version: Set(version_to_store(
            availability.version,
            "definition version",
        )?),
        revision: Set(version_to_store(
            availability.revision,
            "availability revision",
        )?),
        state: Set(state_token(availability.state).to_owned()),
        reason_code: Set(availability.reason_code.clone()),
        qualification_id: Set(qualification.map(|q| q.id)),
        qualification_version: Set(qualification
            .map(|q| version_to_store(q.version, "qualification version"))
            .transpose()?),
        qualification_digest: Set(qualification.map(|q| q.digest.clone())),
        observed_at: Set(availability.observed_at),
        decided_by: Set(availability.decided_by),
    };
    quota_availability::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| driver_failure("quota availability scope".into(), e))?
        .exec(runner)
        .await
        .map(|_| ())
        .map_err(|e| contended("insert quota availability", e))
}

/// `(definition_id, version, max(revision))` of the definitions, one grouped read.
fn current_revisions(tenant_id: Uuid, definitions: &[Uuid]) -> SelectStatement {
    Query::select()
        .column(quota_availability::Column::DefinitionId)
        .column(quota_availability::Column::Version)
        .expr(
            Expr::col((
                quota_availability::Entity,
                quota_availability::Column::Revision,
            ))
            .max(),
        )
        .from(quota_availability::Entity)
        .and_where(quota_availability::Column::TenantId.eq(tenant_id))
        .and_where(quota_availability::Column::DefinitionId.is_in(definitions.iter().copied()))
        .group_by_col(quota_availability::Column::DefinitionId)
        .group_by_col(quota_availability::Column::Version)
        .to_owned()
}

/// The current availability of each of `rows`, in ONE grouped read; a definition version without
/// one is a corrupt row, never a row left out.
async fn with_current_availability(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    rows: Vec<DefinitionVersion>,
) -> Result<Vec<DefinitionRow>, RepoError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let definitions: Vec<Uuid> = rows
        .iter()
        .map(|d| d.definition_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut current: HashMap<(Uuid, QuotaVersion), Availability> = HashMap::new();
    for model in quota_availability::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(quota_availability::Column::TenantId.eq(tenant_id))
                .add(quota_availability::Column::DefinitionId.is_in(definitions.iter().copied()))
                .add(
                    Expr::tuple([
                        Expr::col((
                            quota_availability::Entity,
                            quota_availability::Column::DefinitionId,
                        )),
                        Expr::col((
                            quota_availability::Entity,
                            quota_availability::Column::Version,
                        )),
                        Expr::col((
                            quota_availability::Entity,
                            quota_availability::Column::Revision,
                        )),
                    ])
                    .in_subquery(current_revisions(tenant_id, &definitions)),
                ),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("current quota availability".into(), e))?
    {
        let availability = availability_of(model)?;
        current.insert(
            (availability.definition_id, availability.version),
            availability,
        );
    }
    rows.into_iter()
        .map(|definition| {
            let availability = current
                .remove(&(definition.definition_id, definition.version))
                .ok_or_else(|| {
                    corrupt(
                        DEFINITION_TABLE,
                        tenant_id,
                        format!(
                            "{} version {}",
                            definition.definition_id, definition.version
                        ),
                        "no availability",
                    )
                })?;
            Ok((definition, availability))
        })
        .collect()
}

/// One definition version with its current availability (the highest revision).
/// # Errors
/// Scoped storage failures; a stored row that does not decode, or a version without availability.
pub async fn find_definition(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    definition_id: Uuid,
    version: QuotaVersion,
) -> Result<Option<DefinitionRow>, RepoError> {
    // A version beyond the `bigint` column names no stored row.
    let Ok(stored) = i64::try_from(version.get()) else {
        return Ok(None);
    };
    let Some(model) = quota_definition_version::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(quota_definition_version::Column::TenantId.eq(tenant_id))
                .add(quota_definition_version::Column::DefinitionId.eq(definition_id))
                .add(quota_definition_version::Column::Version.eq(stored)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("find quota definition".into(), e))?
    else {
        return Ok(None);
    };
    let row = definition_of(model)?;
    Ok(
        with_current_availability(runner, scope, tenant_id, vec![row])
            .await?
            .pop(),
    )
}

/// One condition naming each `(id, version)` pair of `keys` once: `any(all(id, version), …)`;
/// `None` when no pair is left, since a version beyond the `bigint` column names no stored row.
fn version_pairs<C: ColumnTrait>(
    keys: &[(Uuid, QuotaVersion)],
    id_column: C,
    version_column: C,
) -> Option<Condition> {
    let pairs: BTreeSet<(Uuid, i64)> = keys
        .iter()
        .filter_map(|(id, version)| Some((*id, i64::try_from(version.get()).ok()?)))
        .collect();
    (!pairs.is_empty()).then(|| {
        pairs
            .into_iter()
            .fold(Condition::any(), |any, (id, version)| {
                any.add(
                    Condition::all()
                        .add(id_column.eq(id))
                        .add(version_column.eq(version)),
                )
            })
    })
}

/// The definition versions `keys` names, each with its current availability, in TWO statements
/// whatever the set holds: the versions, then their availability grouped (the list's own read).
/// A version not stored, or outside `scope`, is absent from the answer (P-D-275).
/// # Errors
/// Scoped storage failures; a stored row that does not decode, or a version without availability.
pub async fn find_definitions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    keys: &[(Uuid, QuotaVersion)],
) -> Result<Vec<DefinitionRow>, RepoError> {
    // @cpt-begin:cpt-cf-bss-products-flow-quota-set-validate:p1:inst-quota-set-read
    use quota_definition_version as dv;
    let Some(pairs) = version_pairs(keys, dv::Column::DefinitionId, dv::Column::Version) else {
        return Ok(Vec::new());
    };
    let rows = dv::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(dv::Column::TenantId.eq(tenant_id))
                .add(pairs),
        )
        .order_by(dv::Column::DefinitionId, Order::Asc)
        .order_by(dv::Column::Version, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("find quota definitions".into(), e))?
        .into_iter()
        .map(definition_of)
        .collect::<Result<Vec<_>, _>>()?;
    with_current_availability(runner, scope, tenant_id, rows).await
    // @cpt-end:cpt-cf-bss-products-flow-quota-set-validate:p1:inst-quota-set-read
}

/// What the set judgement needs of a mapping version: its adapter target and its profile content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingFacts {
    /// The adapter target the mapping enforces on.
    pub target_id: Uuid,
    /// The profile version's content (P-D-270), as stored.
    pub profile: serde_json::Value,
}

#[derive(Debug, FromQueryResult)]
struct MappingFactRow {
    mapping_id: Uuid,
    version: i64,
    target_id: Uuid,
    profile_json: serde_json::Value,
}

/// The facts of each mapping version `keys` names, in ONE statement that reads neither the
/// mapping's own content nor its digests; absent when not stored or outside `scope`.
/// # Errors
/// Scoped storage failures; a stored row that does not decode. A version beyond the stored range
/// names no row.
pub async fn mapping_facts(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    keys: &[(Uuid, QuotaVersion)],
) -> Result<HashMap<(Uuid, QuotaVersion), MappingFacts>, RepoError> {
    // @cpt-begin:cpt-cf-bss-products-flow-quota-set-validate:p1:inst-quota-set-read
    use quota_mapping_version as mv;
    let Some(pairs) = version_pairs(keys, mv::Column::MappingId, mv::Column::Version) else {
        return Ok(HashMap::new());
    };
    let rows: Vec<MappingFactRow> = mv::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(mv::Column::TenantId.eq(tenant_id))
                .add(pairs),
        )
        .project_all(runner, |query| {
            query
                .select_only()
                .column(mv::Column::MappingId)
                .column(mv::Column::Version)
                .column(mv::Column::TargetId)
                .column(mv::Column::ProfileJson)
                .into_model::<MappingFactRow>()
        })
        .await
        .map_err(|e| driver_failure("quota mapping facts".into(), e))?;
    rows.into_iter()
        .map(|row| {
            let key = format!("{} version {}", row.mapping_id, row.version);
            let version = stored_version(row.version, MAPPING_TABLE, tenant_id, &key, "version")?;
            let profile = stored_object(
                row.profile_json,
                MAPPING_TABLE,
                tenant_id,
                &key,
                "profile_json",
            )?;
            Ok((
                (row.mapping_id, version),
                MappingFacts {
                    target_id: row.target_id,
                    profile,
                },
            ))
        })
        .collect()
    // @cpt-end:cpt-cf-bss-products-flow-quota-set-validate:p1:inst-quota-set-read
}

#[derive(Debug, FromQueryResult)]
struct DigestRow {
    definition_id: Uuid,
    version: i64,
    digest: String,
}

/// The stored digest of each definition version `keys` names, in ONE statement that reads the
/// digest only; a version not stored is absent from the answer.
/// # Errors
/// Scoped storage failures; a stored version that does not decode.
pub async fn definition_digests(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    keys: &[(Uuid, QuotaVersion)],
) -> Result<HashMap<(Uuid, QuotaVersion), String>, RepoError> {
    use quota_definition_version as dv;
    // A version beyond the `bigint` column names no stored row.
    let stored: Vec<(Uuid, i64)> = keys
        .iter()
        .filter_map(|(id, version)| Some((*id, i64::try_from(version.get()).ok()?)))
        .collect();
    if stored.is_empty() {
        return Ok(HashMap::new());
    }
    let any = stored.iter().fold(Condition::any(), |any, (id, version)| {
        any.add(
            Condition::all()
                .add(dv::Column::DefinitionId.eq(*id))
                .add(dv::Column::Version.eq(*version)),
        )
    });
    let rows: Vec<DigestRow> = dv::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(dv::Column::TenantId.eq(tenant_id))
                .add(any),
        )
        .project_all(runner, |query| {
            query
                .select_only()
                .column(dv::Column::DefinitionId)
                .column(dv::Column::Version)
                .column(dv::Column::Digest)
                .into_model::<DigestRow>()
        })
        .await
        .map_err(|e| driver_failure("quota definition digests".into(), e))?;
    rows.into_iter()
        .map(|row| {
            let key = format!("{} version {}", row.definition_id, row.version);
            let version =
                stored_version(row.version, DEFINITION_TABLE, tenant_id, &key, "version")?;
            Ok(((row.definition_id, version), row.digest))
        })
        .collect()
}

/// The definition list's pager fields: `definition_id`, with `version` as the tie-break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefinitionListField {
    DefinitionId,
    Version,
}
impl FilterField for DefinitionListField {
    const FIELDS: &'static [Self] = &[Self::DefinitionId, Self::Version];
    fn name(&self) -> &'static str {
        match self {
            Self::DefinitionId => "definition_id",
            Self::Version => "version",
        }
    }
    fn kind(&self) -> FieldKind {
        match self {
            Self::DefinitionId => FieldKind::Uuid,
            Self::Version => FieldKind::I64,
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::FIELDS.iter().copied().find(|f| f.name() == name)
    }
}
/// How the pager reads the definition row for each field.
pub struct DefinitionListMapping;
impl FieldToColumn<DefinitionListField> for DefinitionListMapping {
    type Column = quota_definition_version::Column;
    fn map_field(field: DefinitionListField) -> quota_definition_version::Column {
        match field {
            DefinitionListField::DefinitionId => quota_definition_version::Column::DefinitionId,
            DefinitionListField::Version => quota_definition_version::Column::Version,
        }
    }
    fn map_value(
        _field: DefinitionListField,
        _op: FilterOp,
        value: &ODataValue,
    ) -> Result<ODataValue, String> {
        Ok(value.clone())
    }
}
impl ODataFieldMapping<DefinitionListField> for DefinitionListMapping {
    type Entity = quota_definition_version::Entity;
    fn extract_cursor_value(
        model: &quota_definition_version::Model,
        field: DefinitionListField,
    ) -> sea_orm::Value {
        match field {
            DefinitionListField::DefinitionId => sea_orm::Value::Uuid(Some(model.definition_id)),
            DefinitionListField::Version => sea_orm::Value::BigInt(Some(model.version)),
        }
    }
}

/// `EXISTS` a resource type row of the outer definition version naming `resource_type`.
fn counts_resource_type(resource_type: Uuid) -> Expr {
    use quota_definition_resource_type as rt;
    use quota_definition_version as dv;
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from(rt::Entity)
            .and_where(
                Expr::col((rt::Entity, rt::Column::TenantId))
                    .equals((dv::Entity, dv::Column::TenantId)),
            )
            .and_where(
                Expr::col((rt::Entity, rt::Column::DefinitionId))
                    .equals((dv::Entity, dv::Column::DefinitionId)),
            )
            .and_where(
                Expr::col((rt::Entity, rt::Column::Version))
                    .equals((dv::Entity, dv::Column::Version)),
            )
            .and_where(Expr::col((rt::Entity, rt::Column::ResourceTypeUuid)).eq(resource_type))
            .to_owned(),
    )
}

/// `EXISTS` an enabled availability of the outer definition version with no later revision: its
/// current availability is enabled.
fn currently_enabled() -> Expr {
    use quota_availability as av;
    use quota_definition_version as dv;
    let current = Alias::new("current_availability");
    let later = Alias::new("later_availability");
    let newer = Query::select()
        .expr(Expr::val(1))
        .from_as(av::Entity, later.clone())
        .and_where(
            Expr::col((later.clone(), av::Column::TenantId))
                .equals((current.clone(), av::Column::TenantId)),
        )
        .and_where(
            Expr::col((later.clone(), av::Column::DefinitionId))
                .equals((current.clone(), av::Column::DefinitionId)),
        )
        .and_where(
            Expr::col((later.clone(), av::Column::Version))
                .equals((current.clone(), av::Column::Version)),
        )
        .and_where(
            Expr::col((later, av::Column::Revision))
                .gt(Expr::col((current.clone(), av::Column::Revision))),
        )
        .to_owned();
    Expr::exists(
        Query::select()
            .expr(Expr::val(1))
            .from_as(av::Entity, current.clone())
            .and_where(
                Expr::col((current.clone(), av::Column::TenantId))
                    .equals((dv::Entity, dv::Column::TenantId)),
            )
            .and_where(
                Expr::col((current.clone(), av::Column::DefinitionId))
                    .equals((dv::Entity, dv::Column::DefinitionId)),
            )
            .and_where(
                Expr::col((current.clone(), av::Column::Version))
                    .equals((dv::Entity, dv::Column::Version)),
            )
            .and_where(
                Expr::col((current, av::Column::State))
                    .eq(state_token(QuotaAvailabilityState::Enabled)),
            )
            .and_where(Expr::exists(newer).not())
            .to_owned(),
    )
}

/// One page of the tenant's definition versions applicable to the SKU version (P-D-270): those of
/// the SKU approved at that SKU version or an earlier one, narrowed by `filter`, by
/// `definition_id` (tie-break `version`); `limit` defaults to 50 and is clamped at 200, and a
/// cursor continues the walk. TWO statements whatever the page holds: the page, and the page's
/// current availability in one grouped read.
/// # Errors
/// [`SkuListError::Query`] for a cursor the pager refuses; [`SkuListError::Repo`] for storage, a
/// stored row that does not decode, or a version without availability.
pub async fn list_definitions(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    filter: &DefinitionFilter,
    query: &ODataQuery,
) -> Result<Page<DefinitionRow>, SkuListError> {
    use quota_definition_version as dv;
    let mut query = query.clone();
    if query.cursor.is_none() && query.order.0.is_empty() {
        query.order = ODataOrderBy(vec![OrderKey {
            field: DefinitionListField::DefinitionId.name().to_owned(),
            dir: SortDir::Asc,
        }]);
    }
    // Every stored SKU version is at most `i64::MAX`, so a larger bound keeps them all.
    let sku_version = i64::try_from(filter.sku_version.get()).unwrap_or(i64::MAX);
    let mut condition = Condition::all()
        .add(dv::Column::TenantId.eq(tenant_id))
        .add(dv::Column::SkuId.eq(filter.sku_id))
        .add(dv::Column::SkuVersion.lte(sku_version));
    if let Some(subject) = filter.subject_type_uuid {
        condition = condition.add(dv::Column::SubjectTypeUuid.eq(subject));
    }
    if let Some(profile) = filter.profile_id {
        condition = condition.add(dv::Column::ProfileId.eq(profile));
    }
    if let Some(resource_type) = filter.resource_type_uuid {
        condition = condition.add(counts_resource_type(resource_type));
    }
    if filter.enabled_only {
        condition = condition.add(currently_enabled());
    }
    let select = dv::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(condition);
    let page = paginate_odata_try::<
        DefinitionListField,
        DefinitionListMapping,
        dv::Entity,
        DefinitionVersion,
        _,
        RepoError,
        _,
    >(
        select,
        runner,
        &query,
        (DefinitionListField::Version.name(), SortDir::Asc),
        QUOTA_PAGE,
        definition_of,
    )
    .await
    .map_err(|e| list_error("list quota definitions", e))?;
    let items = with_current_availability(runner, scope, tenant_id, page.items)
        .await
        .map_err(SkuListError::Repo)?;
    Ok(Page {
        items,
        page_info: page.page_info,
    })
}

#[cfg(test)]
#[path = "quota_catalog_repo_tests.rs"]
mod quota_catalog_repo_tests;
