//! The data-backed half of a type update: counting a type's live rows and
//! re-validating them against a candidate schema.
//!
//! Nothing here decides admission — [`crate::domain::evolution`] does, from
//! the schemas alone. This module answers the one question the schemas cannot:
//! do *these* rows satisfy the candidate? It is reached only when the
//! comparison could not prove inclusion and the caller asked for it, so a
//! provably compatible update never reads a row.
//!
//! Batched by keyset over the primary key rather than by OFFSET: the scan runs
//! inside the update's transaction, and an offset walk re-reads its own prefix
//! once per batch.

use graph_storage_sdk::models::{ItemError, ItemFamily, TypeKind};
use graph_storage_sdk::plugin_api::GraphStoreError;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::collections::BTreeMap;
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt};

use crate::domain::ontology::ChainValidator;
use crate::infra::storage::entity::{edge, node};
use crate::infra::store::map_scope_err;

/// How a re-validating scan is paced, how much of a failure it reports, and
/// when it must give up.
///
/// The budget is here because this is the one store operation whose duration
/// scales with a tenant's data: measured on the stand, 250 000 rows validate
/// in ~13 s, which is past the interactive deadline. The row ceiling bounds
/// the work; the budget bounds the wait.
#[derive(Clone, Copy)]
pub(crate) struct ScanBounds {
    /// Rows per batch.
    pub batch: u64,
    /// Offending rows named in the refusal. Enough to see the pattern, not
    /// enough to make the refusal itself a data export.
    pub max_reported: usize,
    /// What is left of the operation's absolute deadline.
    pub budget: graph_storage_sdk::models::RemainingBudget,
}

/// Give up between batches when the caller's deadline is spent.
///
/// Checked per batch rather than per row: a batch is one statement, and the
/// caller cannot be answered mid-statement anyway.
fn still_within(bounds: ScanBounds) -> Result<(), GraphStoreError> {
    if bounds.budget.is_exhausted() {
        return Err(GraphStoreError::Deadline);
    }
    Ok(())
}

/// Producer keys of every endpoint in the batch.
///
/// An edge's validated document names its endpoints by producer key, not by
/// internal id. One query per batch, never one per edge.
async fn endpoint_keys(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    rows: &[edge::Model],
) -> Result<BTreeMap<i64, String>, GraphStoreError> {
    let mut ids: Vec<i64> = rows
        .iter()
        .flat_map(|row| [row.src_node_id, row.dst_node_id])
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(node::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(node::Column::Id.is_in(ids)))
        .all(tx)
        .await
        .map_err(map_scope_err)?
        .into_iter()
        .map(|model| (model.id, model.node_key))
        .collect())
}

/// The instance an edge row presents for validation, as ingest composed it.
fn edge_instance(
    model: &edge::Model,
    keys: &BTreeMap<i64, String>,
    type_id: &str,
) -> serde_json::Value {
    let mut instance = serde_json::json!({
        "type": type_id,
        "src_node_key": keys.get(&model.src_node_id).cloned().unwrap_or_default(),
        "dst_node_key": keys.get(&model.dst_node_id).cloned().unwrap_or_default(),
    });
    if let Some(discriminator) = &model.discriminator {
        instance["discriminator"] = serde_json::Value::String(discriminator.clone());
    }
    instance["payload"] = model.payload.clone();
    instance
}

/// Live rows of one interned type.
pub(crate) async fn count_live(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    kind: TypeKind,
    interned: i32,
) -> Result<u64, GraphStoreError> {
    match kind {
        TypeKind::Node => node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::GtsNodeTypeId.eq(interned))
                    .add(node::Column::DeletedAt.is_null()),
            )
            .count(tx)
            .await
            .map_err(map_scope_err),
        TypeKind::Edge => edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.eq(interned))
                    .add(edge::Column::DeletedAt.is_null()),
            )
            .count(tx)
            .await
            .map_err(map_scope_err),
        // Attributes are payload fragments, not storable rows: there is
        // nothing to re-validate, and reporting zero would read as "checked".
        TypeKind::Attribute => Err(GraphStoreError::Unsupported {
            what: "re-validating an attribute type; it has no rows",
        }),
    }
}

/// Validate every live row of the type against `validator`, reporting the
/// first `max_reported` failures.
///
/// The whole scan runs even once a failure is found: a caller fixing a model
/// wants the shape of the problem, not its first instance — the same reason
/// ingest reports every item error in one answer.
pub(crate) async fn revalidate(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    kind: TypeKind,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    match kind {
        TypeKind::Node => revalidate_nodes(scope, tx, type_id, interned, validator, bounds).await,
        TypeKind::Edge => revalidate_edges(scope, tx, type_id, interned, validator, bounds).await,
        TypeKind::Attribute => Err(GraphStoreError::Unsupported {
            what: "re-validating an attribute type; it has no rows",
        }),
    }
}

/// The instance a node row presents for validation.
///
/// Exactly what ingest validated when the row was written (`domain::service`
/// § `check_node_item`): the producer-authored document, never the
/// gear-assigned envelope. Composed here from storage rather than shared with
/// the ingest path because the two start from different shapes — but if they
/// ever disagree, a row admitted at ingest could be refused by a re-validation
/// that changed nothing, so the comment is the contract.
fn node_instance(model: &node::Model) -> serde_json::Value {
    let mut instance = serde_json::json!({
        "node_key": model.node_key,
        "type": serde_json::Value::Null,
    });
    instance["name"] = serde_json::Value::String(model.name.clone());
    instance["payload"] = model.payload.clone();
    instance
}

async fn revalidate_nodes(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    let mut errors: Vec<ItemError> = Vec::new();
    let mut after: i64 = i64::MIN;
    let mut index = 0usize;
    loop {
        still_within(bounds)?;
        let rows = node::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(node::Column::GtsNodeTypeId.eq(interned))
                    .add(node::Column::DeletedAt.is_null())
                    .add(node::Column::Id.gt(after)),
            )
            .order_by(node::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(errors);
        }
        for model in &rows {
            after = model.id;
            let mut instance = node_instance(model);
            instance["type"] = serde_json::Value::String(type_id.to_owned());
            for (pointer, message) in validator.validate(&instance) {
                if errors.len() < bounds.max_reported {
                    errors.push(ItemError {
                        index,
                        family: ItemFamily::Node,
                        gts_type: Some(type_id.to_owned()),
                        pointer: Some(pointer),
                        message: format!("node `{}`: {message}", model.node_key),
                    });
                }
            }
            index += 1;
        }
    }
}

async fn revalidate_edges(
    scope: &toolkit_security::AccessScope,
    tx: &impl DBRunner,
    type_id: &str,
    interned: i32,
    validator: &ChainValidator,
    bounds: ScanBounds,
) -> Result<Vec<ItemError>, GraphStoreError> {
    let mut errors: Vec<ItemError> = Vec::new();
    let mut after: i64 = i64::MIN;
    let mut index = 0usize;
    loop {
        still_within(bounds)?;
        let rows = edge::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.eq(interned))
                    .add(edge::Column::DeletedAt.is_null())
                    .add(edge::Column::Id.gt(after)),
            )
            .order_by(edge::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(errors);
        }
        let keys = endpoint_keys(scope, tx, &rows).await?;

        for model in &rows {
            after = model.id;
            let instance = edge_instance(model, &keys, type_id);
            for (pointer, message) in validator.validate(&instance) {
                if errors.len() < bounds.max_reported {
                    errors.push(ItemError {
                        index,
                        family: ItemFamily::Edge,
                        gts_type: Some(type_id.to_owned()),
                        pointer: Some(pointer),
                        message: format!("edge `{}`: {message}", model.edge_key),
                    });
                }
            }
            index += 1;
        }
    }
}

/// What a migration pass did, or would do.
pub(crate) struct MigrationOutcome {
    pub rows_scanned: u64,
    pub rows_rewritten: u64,
    /// Rows that do not satisfy the candidate even after the steps. Non-empty
    /// means the whole operation is refused: the plan does not do what the
    /// caller believes it does.
    pub failures: Vec<ItemError>,
}

/// Apply `plan` to every live row of the type, validate the result against the
/// candidate, and write it — or, in a dry run, count what would change.
///
/// Read-modify-write in memory rather than a rendered `jsonb` expression, and
/// deliberately: the document that is validated has to be the document that is
/// written, and a `jsonb_set` chain in SQL beside a step engine in Rust is two
/// implementations of one migration. Every row is read anyway to validate it,
/// so the only cost of doing it here is the cost we were paying regardless.
///
/// One statement per changed row. The row ceiling is what keeps that honest:
/// a migration is bounded work inside one request, and a graph too large for
/// that needs the asynchronous form the plan leaves out of scope.
pub(crate) async fn migrate(
    who: Migrator<'_>,
    tx: &impl DBRunner,
    what: Migrating<'_>,
    bounds: ScanBounds,
) -> Result<MigrationOutcome, GraphStoreError> {
    match what.kind {
        TypeKind::Node => migrate_nodes(who, tx, what, bounds).await,
        TypeKind::Edge => migrate_edges(who, tx, what, bounds).await,
        TypeKind::Attribute => Err(GraphStoreError::Unsupported {
            what: "migrating an attribute type; it has no rows",
        }),
    }
}

/// The authority a migration writes under.
#[derive(Clone, Copy)]
pub(crate) struct Migrator<'a> {
    pub scope: &'a toolkit_security::AccessScope,
    /// Stamped on every rewritten row (`fr-audit-envelope`): a migration is a
    /// write, and a write records who made it.
    pub subject: &'a graph_storage_sdk::models::Subject,
    /// A dry run reads and validates but writes nothing.
    pub dry_run: bool,
}

/// The type being migrated, and what to migrate it with.
#[derive(Clone, Copy)]
pub(crate) struct Migrating<'a> {
    pub type_id: &'a str,
    pub kind: TypeKind,
    pub interned: i32,
    pub plan: &'a crate::domain::migration::Plan,
    pub validator: &'a ChainValidator,
    /// Declared `full_text_search` paths, to recompose the row's lexical text.
    pub full_text_search: &'a [String],
    /// Whether the type declares any `vector_search` path: if it does, a
    /// changed payload makes the stored vector describe text the row no longer
    /// has, so its epoch is cleared and the next ingest re-embeds it.
    pub vectorized: bool,
}

async fn migrate_nodes(
    who: Migrator<'_>,
    tx: &impl DBRunner,
    what: Migrating<'_>,
    bounds: ScanBounds,
) -> Result<MigrationOutcome, GraphStoreError> {
    let mut out = MigrationOutcome {
        rows_scanned: 0,
        rows_rewritten: 0,
        failures: Vec::new(),
    };
    let mut after: i64 = i64::MIN;
    loop {
        still_within(bounds)?;
        let rows = node::Entity::find()
            .secure()
            .scope_with(who.scope)
            .filter(
                Condition::all()
                    .add(node::Column::GtsNodeTypeId.eq(what.interned))
                    .add(node::Column::DeletedAt.is_null())
                    .add(node::Column::Id.gt(after)),
            )
            .order_by(node::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(out);
        }
        for model in &rows {
            after = model.id;
            out.rows_scanned += 1;
            let mut payload = model.payload.clone();
            let changed = what.plan.apply(&mut payload);

            let mut instance = node_instance(model);
            instance["type"] = serde_json::Value::String(what.type_id.to_owned());
            instance["payload"] = payload.clone();
            let violations = what.validator.validate(&instance);
            if !violations.is_empty() {
                for (pointer, message) in violations {
                    if out.failures.len() < bounds.max_reported {
                        out.failures.push(ItemError {
                            index: usize::try_from(out.rows_scanned - 1).unwrap_or(usize::MAX),
                            family: ItemFamily::Node,
                            gts_type: Some(what.type_id.to_owned()),
                            pointer: Some(pointer),
                            message: format!(
                                "node `{}` does not satisfy the candidate after the migration: \
                                 {message}",
                                model.node_key
                            ),
                        });
                    }
                }
                continue;
            }
            if !changed {
                continue;
            }
            out.rows_rewritten += 1;
            if who.dry_run {
                continue;
            }
            let search_text = crate::infra::store::ingest::compose_search_text(
                Some(model.name.as_str()),
                Some(&payload),
                what.full_text_search,
            );
            let mut update = node::Entity::update_many()
                .col_expr(node::Column::Payload, Expr::value(payload))
                .col_expr(node::Column::SearchText, Expr::value(search_text))
                // The compare-and-set target moves with the row. Without this a
                // producer holding the pre-migration version would overwrite the
                // migrated row and undo the migration in silence.
                .col_expr(node::Column::Version, Expr::value(model.version + 1))
                .col_expr(
                    node::Column::UpdatedAt,
                    Expr::value(time::OffsetDateTime::now_utc()),
                )
                .col_expr(
                    node::Column::UpdatedBySubjectId,
                    Expr::value(who.subject.subject_id),
                )
                .col_expr(
                    node::Column::UpdatedBySubjectType,
                    Expr::value(who.subject.subject_type.clone()),
                );
            if what.vectorized {
                update = update.col_expr(
                    node::Column::EmbeddingEpoch,
                    Expr::value(Option::<i64>::None),
                );
            }
            update
                .filter(Condition::all().add(node::Column::Id.eq(model.id)))
                .secure()
                .scope_with(who.scope)
                .exec(tx)
                .await
                .map_err(map_scope_err)?;
        }
    }
}

async fn migrate_edges(
    who: Migrator<'_>,
    tx: &impl DBRunner,
    what: Migrating<'_>,
    bounds: ScanBounds,
) -> Result<MigrationOutcome, GraphStoreError> {
    let mut out = MigrationOutcome {
        rows_scanned: 0,
        rows_rewritten: 0,
        failures: Vec::new(),
    };
    let mut after: i64 = i64::MIN;
    loop {
        still_within(bounds)?;
        let rows = edge::Entity::find()
            .secure()
            .scope_with(who.scope)
            .filter(
                Condition::all()
                    .add(edge::Column::GtsEdgeTypeId.eq(what.interned))
                    .add(edge::Column::DeletedAt.is_null())
                    .add(edge::Column::Id.gt(after)),
            )
            .order_by(edge::Column::Id, sea_orm::Order::Asc)
            .limit(bounds.batch)
            .all(tx)
            .await
            .map_err(map_scope_err)?;
        if rows.is_empty() {
            return Ok(out);
        }
        let keys = endpoint_keys(who.scope, tx, &rows).await?;
        for model in &rows {
            after = model.id;
            out.rows_scanned += 1;
            let mut payload = model.payload.clone();
            let changed = what.plan.apply(&mut payload);

            let mut instance = edge_instance(model, &keys, what.type_id);
            instance["payload"] = payload.clone();
            let violations = what.validator.validate(&instance);
            if !violations.is_empty() {
                for (pointer, message) in violations {
                    if out.failures.len() < bounds.max_reported {
                        out.failures.push(ItemError {
                            index: usize::try_from(out.rows_scanned - 1).unwrap_or(usize::MAX),
                            family: ItemFamily::Edge,
                            gts_type: Some(what.type_id.to_owned()),
                            pointer: Some(pointer),
                            message: format!(
                                "edge `{}` does not satisfy the candidate after the migration: \
                                 {message}",
                                model.edge_key
                            ),
                        });
                    }
                }
                continue;
            }
            if !changed {
                continue;
            }
            out.rows_rewritten += 1;
            if who.dry_run {
                continue;
            }
            edge::Entity::update_many()
                .col_expr(edge::Column::Payload, Expr::value(payload))
                .col_expr(
                    edge::Column::UpdatedAt,
                    Expr::value(time::OffsetDateTime::now_utc()),
                )
                .col_expr(
                    edge::Column::UpdatedBySubjectId,
                    Expr::value(who.subject.subject_id),
                )
                .col_expr(
                    edge::Column::UpdatedBySubjectType,
                    Expr::value(who.subject.subject_type.clone()),
                )
                .filter(Condition::all().add(edge::Column::Id.eq(model.id)))
                .secure()
                .scope_with(who.scope)
                .exec(tx)
                .await
                .map_err(map_scope_err)?;
        }
    }
}
