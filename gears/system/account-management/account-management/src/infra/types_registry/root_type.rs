//! Persistent reconciliation of the configured platform-root tenant type.
//!
//! The legacy `TypesRegistryClient` is intentionally not used here: this path
//! must prove that the schema was admitted into authoritative storage shared by
//! all replicas.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use account_management_sdk::gts::TenantTypeEnvelopeV1;
use gts::GtsSchema;
use serde_json::{Value, json};
use toolkit_gts::GTS_ID_URI_PREFIX;
use types_registry_sdk::{
    CandidateStatus, EntityKind, EntitySnapshot, LifecycleStatus, RegisterEntities, RegisterItem,
    TypesRegistryEntities,
};
use uuid::Uuid;

use crate::domain::root_type::{RootTypeConfig, TENANT_TYPE_BASE};
const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
const RECONCILE_DEADLINE: Duration = Duration::from_secs(30);
const PRECONDITION_FAILED: &str = "precondition_failed";
const ALREADY_EXISTS: &str = "already_exists";
const AM_OWNING_GEAR: &str = "account-management";
const LEGACY_OWNING_GEAR: &str = "types-registry";

/// Observable successful reconciliation disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootTypeReconcileOutcome {
    Existing,
    Created,
    ConcurrentlyCreated,
    Adopted,
}

/// Reconcile the configured root type against authoritative persistent state.
///
/// Existing definitions are never revised. A semantic mismatch is lifecycle
/// fatal and requires an explicit schema/root migration.
pub async fn reconcile_root_type(
    registry: Arc<dyn TypesRegistryEntities>,
    cfg: &RootTypeConfig,
) -> anyhow::Result<RootTypeReconcileOutcome> {
    let desired = desired_root_schema(cfg)?;
    let type_id = cfg.gts_id.as_ref();

    if let Some(existing) = registry.get_entity(type_id).await? {
        // Detect concrete-root drift before creating a missing base. Once the
        // concrete contract is known to match, also verify AM's dependency.
        validate_persisted_root(&existing, cfg)?;
        let adopted = ensure_am_ownership(Arc::clone(&registry), existing, |snapshot| {
            validate_persisted_root(snapshot, cfg)
        })
        .await?;
        ensure_persistent_base(Arc::clone(&registry)).await?;
        return Ok(if adopted {
            RootTypeReconcileOutcome::Adopted
        } else {
            RootTypeReconcileOutcome::Existing
        });
    }

    // The current persistent path predates per-gear inventory push, so ensure
    // AM's abstract envelope is present in the same authoritative store. This
    // becomes an ordinary no-op once Types Registry T24 seeds/reconciles owned
    // inventory through that store. It is a separate operation so every
    // replica submits exactly the same request under the same idempotency key;
    // conditionally combining base + root would make fingerprints race.
    ensure_persistent_base(Arc::clone(&registry)).await?;

    let operation = match registry
        .register_and_await(
            idempotency_key("root-type", &desired)?,
            RegisterEntities {
                items: vec![RegisterItem {
                    gts_id: type_id.to_owned(),
                    content: desired,
                    expected_resource_version: None,
                    force: false,
                }],
                dry_run: false,
                owning_gear: AM_OWNING_GEAR.to_owned(),
            },
            RECONCILE_DEADLINE,
        )
        .await
    {
        Ok(operation) => operation,
        Err(submission_error) => {
            // The operation may have committed even if its receipt was lost.
            // Only an authoritative read of matching content can turn that
            // ambiguous failure into success.
            match registry.get_entity(type_id).await {
                Ok(Some(stored)) => {
                    validate_persisted_root(&stored, cfg)?;
                    ensure_am_ownership(Arc::clone(&registry), stored, |snapshot| {
                        validate_persisted_root(snapshot, cfg)
                    })
                    .await?;
                    return Ok(RootTypeReconcileOutcome::ConcurrentlyCreated);
                }
                Ok(None) => {
                    return Err(anyhow::anyhow!(
                        "root tenant type registration failed and {type_id} is still absent: {submission_error}"
                    ));
                }
                Err(read_error) => {
                    return Err(anyhow::anyhow!(
                        "root tenant type registration failed ({submission_error}) and its final authoritative read also failed: {read_error}"
                    ));
                }
            }
        }
    };

    let root_result = operation
        .items
        .iter()
        .find(|item| item.gts_id == type_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "root tenant type registration operation {} returned no result for {}",
                operation.operation_id,
                type_id
            )
        })?;

    let raced = match root_result.status {
        CandidateStatus::Succeeded => false,
        CandidateStatus::Failed
            if root_result.error.as_ref().is_some_and(|error| {
                matches!(error.reason.as_str(), PRECONDITION_FAILED | ALREADY_EXISTS)
            }) =>
        {
            true
        }
        other => {
            let detail = root_result.error.as_ref().map_or_else(
                || "no diagnostic".to_owned(),
                |error| format!("{}: {}", error.reason, error.message),
            );
            anyhow::bail!(
                "root tenant type {} was not admitted (status={other:?}, operation={}): {detail}",
                type_id,
                operation.operation_id
            );
        }
    };

    // Never trust the operation receipt as the persisted result. A fresh exact
    // read both verifies durability and resolves a concurrent create winner.
    let stored = registry.get_entity(type_id).await?.ok_or_else(|| {
        anyhow::anyhow!(
            "root tenant type {} is absent after completed registration operation {}",
            type_id,
            operation.operation_id
        )
    })?;
    validate_persisted_root(&stored, cfg)?;
    ensure_am_ownership(Arc::clone(&registry), stored, |snapshot| {
        validate_persisted_root(snapshot, cfg)
    })
    .await?;

    Ok(if raced {
        RootTypeReconcileOutcome::ConcurrentlyCreated
    } else {
        RootTypeReconcileOutcome::Created
    })
}

async fn ensure_persistent_base(registry: Arc<dyn TypesRegistryEntities>) -> anyhow::Result<()> {
    if let Some(existing) = registry.get_entity(TENANT_TYPE_BASE).await? {
        validate_base_snapshot(&existing)?;
        ensure_am_ownership(Arc::clone(&registry), existing, validate_base_snapshot).await?;
        return Ok(());
    }

    let content = TenantTypeEnvelopeV1::<()>::gts_schema_with_refs();
    let operation = match registry
        .register_and_await(
            idempotency_key("tenant-type-base", &content)?,
            RegisterEntities {
                items: vec![RegisterItem {
                    gts_id: TENANT_TYPE_BASE.to_owned(),
                    content,
                    expected_resource_version: None,
                    force: false,
                }],
                dry_run: false,
                owning_gear: AM_OWNING_GEAR.to_owned(),
            },
            RECONCILE_DEADLINE,
        )
        .await
    {
        Ok(operation) => operation,
        Err(submission_error) => match registry.get_entity(TENANT_TYPE_BASE).await {
            Ok(Some(stored)) => {
                validate_base_snapshot(&stored)?;
                ensure_am_ownership(Arc::clone(&registry), stored, validate_base_snapshot).await?;
                return Ok(());
            }
            Ok(None) => {
                return Err(anyhow::anyhow!(
                    "AM tenant-type base registration failed and the base is still absent: {submission_error}"
                ));
            }
            Err(read_error) => {
                return Err(anyhow::anyhow!(
                    "AM tenant-type base registration failed ({submission_error}) and its final authoritative read also failed: {read_error}"
                ));
            }
        },
    };
    let result = operation
        .items
        .iter()
        .find(|item| item.gts_id == TENANT_TYPE_BASE)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "tenant-type base registration operation {} returned no result",
                operation.operation_id
            )
        })?;
    let admitted_or_raced = result.status == CandidateStatus::Succeeded
        || (result.status == CandidateStatus::Failed
            && result.error.as_ref().is_some_and(|error| {
                matches!(error.reason.as_str(), PRECONDITION_FAILED | ALREADY_EXISTS)
            }));
    if !admitted_or_raced {
        let detail = result.error.as_ref().map_or_else(
            || "no diagnostic".to_owned(),
            |error| format!("{}: {}", error.reason, error.message),
        );
        anyhow::bail!(
            "AM tenant-type base was not admitted (status={:?}, operation={}): {detail}",
            result.status,
            operation.operation_id
        );
    }
    let stored = registry
        .get_entity(TENANT_TYPE_BASE)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "AM tenant-type base is absent after completed registration operation {}",
                operation.operation_id
            )
        })?;
    validate_base_snapshot(&stored)?;
    ensure_am_ownership(registry, stored, validate_base_snapshot).await?;
    Ok(())
}

async fn ensure_am_ownership(
    registry: Arc<dyn TypesRegistryEntities>,
    initial: EntitySnapshot,
    validate: impl Fn(&EntitySnapshot) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    let gts_id = initial.gts_id.clone();
    let mut current = initial;
    let adoption_required = current.owning_gear.as_deref() != Some(AM_OWNING_GEAR);

    for _ in 0..3 {
        // A lost ownership CAS can mean that a concurrent schema revision moved
        // the resource version. Revalidate every fresh snapshot before retrying
        // so AM never adopts content it did not inspect.
        validate(&current)?;
        match current.owning_gear.as_deref() {
            Some(AM_OWNING_GEAR) => return Ok(adoption_required),
            None | Some(LEGACY_OWNING_GEAR) => {}
            Some(other) => {
                anyhow::bail!(
                    "entity {gts_id} is owned by `{other}`, not `{AM_OWNING_GEAR}`; automatic ownership takeover is forbidden"
                );
            }
        }

        let expected_owner = current.owning_gear.clone();
        let _won = registry
            .compare_and_swap_owning_gear(
                &gts_id,
                current.resource_version,
                expected_owner,
                AM_OWNING_GEAR.to_owned(),
            )
            .await?;
        current = registry.get_entity(&gts_id).await?.ok_or_else(|| {
            anyhow::anyhow!("entity {gts_id} disappeared during ownership adoption")
        })?;
        if current.lifecycle_status != LifecycleStatus::Active {
            anyhow::bail!("entity {gts_id} was deleted during ownership adoption");
        }
    }

    anyhow::bail!(
        "entity {gts_id} ownership did not converge to `{AM_OWNING_GEAR}` after concurrent updates"
    )
}

fn validate_base_snapshot(stored: &EntitySnapshot) -> anyhow::Result<()> {
    if stored.gts_id != TENANT_TYPE_BASE
        || stored.kind != EntityKind::TypeSchema
        || stored.lifecycle_status != LifecycleStatus::Active
    {
        anyhow::bail!(
            "AM tenant-type base {TENANT_TYPE_BASE} is not an active Type Schema; automatic replacement is forbidden"
        );
    }
    let content = stored.content.as_ref().ok_or_else(|| {
        anyhow::anyhow!("AM tenant-type base {TENANT_TYPE_BASE} has no authored schema document")
    })?;
    let desired = TenantTypeEnvelopeV1::<()>::gts_schema_with_refs();
    if content != &desired {
        anyhow::bail!(
            "AM tenant-type base {TENANT_TYPE_BASE} schema conflicts with the Account Management-owned contract; explicit migration is required"
        );
    }
    Ok(())
}

fn idempotency_key(resource: &str, content: &Value) -> anyhow::Result<String> {
    let canonical = serde_json::to_vec(content)?;
    let fingerprint = Uuid::new_v5(&Uuid::NAMESPACE_URL, &canonical);
    Ok(format!("account-management-{resource}-{fingerprint}"))
}

/// Build the single AM-owned concrete root schema.
pub fn desired_root_schema(cfg: &RootTypeConfig) -> anyhow::Result<Value> {
    let type_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    let parent = types_registry_sdk::GtsTypeSchema::derive_parent_type_id(type_id)
        .ok_or_else(|| anyhow::anyhow!("root tenant type {type_id} has no concrete parent"))?;
    Ok(json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{type_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "description": "Platform-root tenant type owned by Account Management (no parents).",
        "type": "object",
        "allOf": [{ "$ref": format!("{GTS_ID_URI_PREFIX}{parent}") }],
        "x-gts-traits": {
            "allowed_parent_types": [],
            "idp_provisioning": cfg.idp_provisioning,
        }
    }))
}

/// Compare AM-owned semantic fields, not the complete authored JSON document.
pub fn validate_persisted_root(
    stored: &EntitySnapshot,
    cfg: &RootTypeConfig,
) -> anyhow::Result<()> {
    let expected_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    if stored.gts_id != expected_id {
        anyhow::bail!(
            "root tenant type read returned unexpected id: expected {expected_id}, found {}",
            stored.gts_id
        );
    }
    let expected_uuid = gts::GtsId::try_new(expected_id)
        .map_err(|error| anyhow::anyhow!("root tenant type {expected_id} is invalid: {error}"))?
        .to_uuid();
    if stored.gts_uuid != expected_uuid {
        anyhow::bail!(
            "root tenant type {expected_id} has persisted UUID {}, expected deterministic UUID {expected_uuid}; explicit registry repair is required",
            stored.gts_uuid
        );
    }
    if stored.kind != EntityKind::TypeSchema {
        anyhow::bail!("root tenant type {expected_id} is not stored as a Type Schema");
    }
    if stored.lifecycle_status != LifecycleStatus::Active {
        anyhow::bail!("root tenant type {expected_id} is deleted; automatic revival is forbidden");
    }

    let content = stored.content.as_ref().ok_or_else(|| {
        anyhow::anyhow!("root tenant type {expected_id} has no authored schema document")
    })?;
    validate_authored_contract(content, expected_id)?;

    let traits = stored.effective_traits.as_ref().ok_or_else(|| {
        anyhow::anyhow!("root tenant type {expected_id} has no materialized effective traits")
    })?;
    let parents = traits
        .get("allowed_parent_types")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "root tenant type {expected_id} effective allowed_parent_types is not an array"
            )
        })?;
    if !parents.is_empty() {
        anyhow::bail!(
            "root tenant type {expected_id} drift: expected allowed_parent_types=[], found {parents:?}"
        );
    }
    let found_idp = traits
        .get("idp_provisioning")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "root tenant type {expected_id} effective idp_provisioning is not a boolean"
            )
        })?;
    if found_idp != cfg.idp_provisioning {
        anyhow::bail!(
            "root tenant type {expected_id} drift: configured idp_provisioning={}, persisted effective value={found_idp}",
            cfg.idp_provisioning
        );
    }
    Ok(())
}

fn validate_authored_contract(content: &Value, type_id: &str) -> anyhow::Result<()> {
    let object = content
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("root tenant type {type_id} schema is not an object"))?;
    let authored_id = object
        .get("$id")
        .and_then(Value::as_str)
        .and_then(|id| id.strip_prefix(GTS_ID_URI_PREFIX))
        .ok_or_else(|| anyhow::anyhow!("root tenant type {type_id} has an invalid $id"))?;
    if authored_id != type_id {
        anyhow::bail!("root tenant type {type_id} authored $id names {authored_id}");
    }
    let dialect = object
        .get("$schema")
        .and_then(Value::as_str)
        .map(|value| value.trim_end_matches('#'));
    if dialect != Some(JSON_SCHEMA_DRAFT_07.trim_end_matches('#')) {
        anyhow::bail!("root tenant type {type_id} must use JSON Schema draft-07");
    }
    if object.get("type").and_then(Value::as_str) != Some("object") {
        anyhow::bail!("root tenant type {type_id} must declare type=object");
    }
    if object.get("x-gts-abstract").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!("root tenant type {type_id} must be concrete, not abstract");
    }

    let parent = types_registry_sdk::GtsTypeSchema::derive_parent_type_id(type_id)
        .ok_or_else(|| anyhow::anyhow!("root tenant type {type_id} has no derivation parent"))?;
    let expected_ref = format!("{GTS_ID_URI_PREFIX}{parent}");
    let all_of = object
        .get("allOf")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("root tenant type {type_id} must declare one parent $ref")
        })?;
    let valid_ref = all_of.len() == 1
        && all_of[0].as_object().is_some_and(|item| {
            item.len() == 1 && item.get("$ref").and_then(Value::as_str) == Some(&expected_ref)
        });
    if !valid_ref {
        anyhow::bail!("root tenant type {type_id} must derive only from its chain parent {parent}");
    }

    // Annotation fields are deployment-neutral. Any other validation-bearing
    // keyword changes the AM-owned root contract and is treated as drift.
    let allowed: BTreeSet<&str> = [
        "$comment",
        "$id",
        "$schema",
        "allOf",
        "description",
        "examples",
        "title",
        "type",
        "x-gts-traits",
    ]
    .into_iter()
    .collect();
    let unexpected: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if !unexpected.is_empty() {
        anyhow::bail!(
            "root tenant type {type_id} contains incompatible AM-owned schema fields: {unexpected:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "root_type_tests.rs"]
mod tests;
