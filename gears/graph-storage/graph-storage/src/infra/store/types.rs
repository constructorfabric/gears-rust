//! Ontology storage: registration, lookup and pattern resolution.
//!
//! Registration is idempotent for a byte-identical schema and conflicts for a
//! different one under the same identifier. `ON CONFLICT DO NOTHING` skipping
//! every row is **convergence, not failure** — reporting it as an error is
//! the trap a re-registration hit in the prototype (ADR-0006 § Confirmation).

use graph_storage_sdk::models::{
    EffectiveTraits, GtsTypeId, Page, TypeIdSet, TypeKind, TypeQuery, TypeRecord, TypeRegistration,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{SecureEntityExt, SecureInsertExt};

use crate::domain::ontology;
use crate::infra::storage::entity::gts_type;
use crate::infra::store::{PgGraphStore, TxStoreError, map_db_error, map_scope_err};

fn kind_to_str(kind: TypeKind) -> &'static str {
    kind.as_str()
}

fn kind_from_str(value: &str) -> Result<TypeKind, GraphStoreError> {
    match value {
        "node" => Ok(TypeKind::Node),
        "edge" => Ok(TypeKind::Edge),
        "attribute" => Ok(TypeKind::Attribute),
        other => Err(GraphStoreError::Corrupt {
            reason: format!("gts_type.kind holds `{other}`"),
        }),
    }
}

/// Read the stored trait resolution back. Hand-written, like the write side:
/// the SDK models carry no serde by contract, so the JSON shape is owned here,
/// beside the column that holds it.
fn traits_from_json(value: &serde_json::Value) -> EffectiveTraits {
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    EffectiveTraits {
        family: value
            .get("family")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        scope_managed: value
            .get("scope_managed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        emit_events: value
            .get("emit_events")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        index: strings("index"),
        full_text_search: strings("full_text_search"),
        vector_search: strings("vector_search"),
        src_types: strings("src_types"),
        dst_types: strings("dst_types"),
    }
}

fn to_record(model: gts_type::Model) -> Result<TypeRecord, GraphStoreError> {
    let effective_traits = traits_from_json(&model.effective_traits);
    let is_abstract = model
        .type_schema
        .get("x-gts-abstract")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    Ok(TypeRecord {
        type_id: model.gts_type_id,
        type_uuid: model.gts_type_uuid,
        kind: kind_from_str(&model.kind)?,
        is_abstract,
        schema: model.type_schema,
        effective_traits,
        created_at: model.created_at,
    })
}

/// Traits are stored as their resolved JSON so the shape the SDK reads and
/// the shape the column holds cannot drift.
fn traits_to_json(traits: &EffectiveTraits) -> serde_json::Value {
    serde_json::json!({
        "family": traits.family,
        "scope_managed": traits.scope_managed,
        "emit_events": traits.emit_events,
        "index": traits.index,
        "full_text_search": traits.full_text_search,
        "vector_search": traits.vector_search,
        "src_types": traits.src_types,
        "dst_types": traits.dst_types,
    })
}

pub async fn register_types(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    batch: Vec<TypeRegistration>,
) -> Result<Vec<TypeRecord>, GraphStoreError> {
    // The batch commits atomically: a partway failure leaves nothing.
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    store
        .db()
        .transaction_ref_mapped::<_, Vec<TypeRecord>, TxStoreError>(move |tx| {
            let batch = batch.clone();
            let scope = scope.clone();
            Box::pin(async move {
                register_in_tx(tenant, &scope, tx, batch)
                    .await
                    .map_err(TxStoreError::from)
            })
        })
        .await
        .map_err(|error| error.0)
}

async fn register_in_tx(
    tenant: uuid::Uuid,
    scope: &toolkit_security::AccessScope,
    tx: &impl toolkit_db::secure::DBRunner,
    batch: Vec<TypeRegistration>,
) -> Result<Vec<TypeRecord>, GraphStoreError> {
    {
        let mut out = Vec::with_capacity(batch.len());
        for registration in batch {
            // Resolve the chain from what is already registered plus
            // what this batch carries (already stored by this loop).
            let chain = ontology::ancestors(&registration.type_id);
            let mut ancestor_values = Vec::new();
            for ancestor in &chain[..chain.len().saturating_sub(1)] {
                let existing = gts_type::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .filter(Condition::all().add(gts_type::Column::GtsTypeId.eq(ancestor.clone())))
                    .one(tx)
                    .await
                    .map_err(map_scope_err)?;
                match existing {
                    Some(model) => ancestor_values.push(model.type_schema),
                    None => {
                        return Err(GraphStoreError::Validation {
                            items: vec![graph_storage_sdk::models::ItemError {
                                index: 0,
                                family: graph_storage_sdk::models::ItemFamily::Node,
                                gts_type: Some(registration.type_id.clone()),
                                pointer: None,
                                message: format!("ancestor `{ancestor}` is not registered"),
                            }],
                        });
                    }
                }
            }
            let ancestor_refs: Vec<&serde_json::Value> = ancestor_values.iter().collect();
            let descriptor =
                ontology::analyze(&registration.type_id, &registration.schema, &ancestor_refs)
                    .map_err(|error| GraphStoreError::Validation {
                        items: vec![graph_storage_sdk::models::ItemError {
                            index: 0,
                            family: graph_storage_sdk::models::ItemFamily::Node,
                            gts_type: Some(registration.type_id.clone()),
                            pointer: None,
                            message: error.to_string(),
                        }],
                    })?;

            let existing = gts_type::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(
                    Condition::all()
                        .add(gts_type::Column::GtsTypeId.eq(descriptor.type_id.clone())),
                )
                .one(tx)
                .await
                .map_err(map_scope_err)?;

            if let Some(model) = existing {
                // Byte-identical re-registration converges; a different
                // schema under one identifier is a conflict.
                if model.type_schema != descriptor.schema {
                    return Err(GraphStoreError::Conflict {
                        reason: format!(
                            "type `{}` is already registered with a different schema",
                            descriptor.type_id
                        ),
                    });
                }
                out.push(to_record(model)?);
                continue;
            }

            let active = gts_type::ActiveModel {
                tenant_id: ActiveValue::Set(tenant),
                id: ActiveValue::NotSet,
                gts_type_uuid: ActiveValue::Set(descriptor.type_uuid),
                gts_type_id: ActiveValue::Set(descriptor.type_id.clone()),
                kind: ActiveValue::Set(kind_to_str(descriptor.kind).to_owned()),
                type_schema: ActiveValue::Set(descriptor.schema.clone()),
                effective_traits: ActiveValue::Set(traits_to_json(&descriptor.effective_traits)),
                created_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
            };
            // scope_unchecked: an INSERT cannot subtree-clamp a row
            // that does not exist yet.
            let model = gts_type::Entity::insert(active)
                .secure()
                .scope_unchecked(scope)
                .map_err(map_scope_err)?
                .exec_with_returning(tx)
                .await
                .map_err(map_scope_err)?;
            out.push(to_record(model)?);
        }
        Ok(out)
    }
}

pub async fn get_type(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    id: &GtsTypeId,
) -> Result<TypeRecord, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let model = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::GtsTypeId.eq(id.clone())))
        .one(&conn)
        .await
        .map_err(map_scope_err)?
        .ok_or(GraphStoreError::NotFound)?;
    to_record(model)
}

pub async fn list_types(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    query: TypeQuery,
) -> Result<Page<TypeRecord>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let mut select = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .order_by(gts_type::Column::GtsTypeId, sea_orm::Order::Asc);
    if let Some(kind) = query.kind {
        select = select.filter(Condition::all().add(gts_type::Column::Kind.eq(kind_to_str(kind))));
    }
    let limit = query.top.unwrap_or(store.config().projection_max_page);
    let models = select
        .limit(u64::from(limit))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    // The pattern is resolved through the platform GTS implementation, never
    // compiled into SQL: no identifier ever reaches a LIKE pattern.
    let mut items = Vec::new();
    for model in models {
        if let Some(pattern) = &query.pattern {
            let patterns = vec![pattern.clone()];
            let matches =
                ontology::matches_any_pattern(&model.gts_type_id, &patterns).map_err(|error| {
                    GraphStoreError::LimitExceeded {
                        what: error.to_string(),
                    }
                })?;
            if !matches {
                continue;
            }
        }
        items.push(to_record(model)?);
    }

    let revision = crate::infra::store::reads::revision(store, ctx).await?;
    Ok(Page {
        items,
        next_cursor: None,
        revision,
    })
}

pub async fn resolve_type_set(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    patterns: &[String],
) -> Result<TypeIdSet, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let models = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    let mut set = std::collections::BTreeSet::new();
    for model in models {
        let matches =
            ontology::matches_any_pattern(&model.gts_type_id, patterns).map_err(|error| {
                GraphStoreError::LimitExceeded {
                    what: error.to_string(),
                }
            })?;
        if matches {
            set.insert(model.gts_type_id);
        }
    }
    Ok(TypeIdSet(set))
}

/// Interned ids for the given GTS identifiers, for the write path.
pub async fn interned_ids(
    scope: &toolkit_security::AccessScope,
    runner: &impl toolkit_db::secure::DBRunner,
    type_ids: &[String],
) -> Result<std::collections::BTreeMap<String, (i32, uuid::Uuid, serde_json::Value)>, GraphStoreError>
{
    let models = gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(type_ids.to_vec())))
        .all(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(models
        .into_iter()
        .map(|m| (m.gts_type_id, (m.id, m.gts_type_uuid, m.effective_traits)))
        .collect())
}

/// Count of registered types, used by the readiness surface.
pub async fn count(store: &PgGraphStore, ctx: &StoreCtx<'_>) -> Result<u64, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .count(&conn)
        .await
        .map_err(map_scope_err)
}
