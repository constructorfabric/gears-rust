//! Maps stored `SMALLINT` keys to GTS type identifiers via `gts_type`.
//!
//! Rows are immutable, so reads and cleanup work after type retirement.
//! [`ensure_key`] runs after registry admission and in the same transaction
//! as the referencing write. Reads, filters and deletes treat a missing
//! identifier as having no stored groups or memberships.

use std::collections::HashMap;

use sea_orm::sea_query::{OnConflict, Query, SelectStatement};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};
use toolkit_db::secure::{DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use toolkit_security::AccessScope;

use super::entity::gts_type::{self, Column, Entity};
use crate::domain::error::DomainError;
use crate::domain::repo::GtsTypeKey;

/// The dictionary is platform-global (no scope columns): every read and the
/// insert run under the unconstrained scope.
fn dictionary_scope() -> AccessScope {
    AccessScope::allow_all()
}

fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        // Keep the driver error typed: serialization failures stay retryable.
        ScopeError::Db(db) => DomainError::Database(db),
        other => DomainError::database(other.to_string()),
    }
}

/// The key stored for `schema_id`, if any. Never creates one.
pub async fn find_key(
    db: &impl DBRunner,
    schema_id: &str,
) -> Result<Option<GtsTypeKey>, DomainError> {
    let row = Entity::find()
        .filter(Column::SchemaId.eq(schema_id))
        .secure()
        .scope_with(&dictionary_scope())
        .one(db)
        .await
        .map_err(map_scope_error)?;
    Ok(row.map(|r| GtsTypeKey(r.id)))
}

/// The key for `schema_id`, creating the dictionary row on first use.
///
/// Look up existing keys before inserting to conserve PostgreSQL's
/// `SMALLINT` sequence. First-use races and rollbacks can still consume values.
///
/// Use the same transaction as the referencing write. With
/// `ON CONFLICT (schema_id) DO NOTHING`, a concurrent first use re-reads the
/// winner's row on SQLite or at weaker PostgreSQL isolation levels. At
/// `SERIALIZABLE`, a conflict outside the snapshot returns a typed
/// [`DomainError::Database`] for the caller to retry.
pub async fn ensure_key(db: &impl DBRunner, schema_id: &str) -> Result<GtsTypeKey, DomainError> {
    if let Some(key) = find_key(db, schema_id).await? {
        return Ok(key);
    }
    let scope = dictionary_scope();
    let am = gts_type::ActiveModel {
        schema_id: Set(schema_id.to_owned()),
        created_at: Set(time::OffsetDateTime::now_utc()),
        ..Default::default()
    };
    let inserted = Entity::insert(am.clone())
        .secure()
        .scope_with_model(&scope, &am)
        .map_err(map_scope_error)?
        .on_conflict_raw(OnConflict::column(Column::SchemaId).do_nothing().to_owned())
        .exec(db)
        .await;
    match inserted {
        Ok(result) => Ok(GtsTypeKey(result.last_insert_id)),
        // A concurrent first use committed the row between our read and insert.
        Err(ScopeError::Db(DbErr::RecordNotInserted)) => {
            find_key(db, schema_id).await?.ok_or_else(|| {
                DomainError::database(format!(
                    "GTS type dictionary row for '{schema_id}' conflicted but is not visible"
                ))
            })
        }
        Err(e) => Err(map_scope_error(e)),
    }
}

/// Resolved GTS type identifiers by dictionary key; see [`find_paths`].
pub struct TypePaths(HashMap<GtsTypeKey, String>);

impl TypePaths {
    /// The identifier for `key`. Every stored key has a row (foreign keys,
    /// rows are never deleted), so a miss is a broken database or a key that
    /// was not part of the batch -- an error, never an empty identifier.
    pub fn code(&self, key: GtsTypeKey) -> Result<String, DomainError> {
        self.0.get(&key).cloned().ok_or_else(|| missing_key(key))
    }
}

fn missing_key(key: GtsTypeKey) -> DomainError {
    DomainError::database(format!("GTS type key {} has no dictionary row", key.0))
}

/// GTS type identifiers for `keys`, batched (no per-row lookups). One query
/// per bind-parameter chunk. A key without a row fails the whole call.
pub async fn find_paths(
    db: &impl DBRunner,
    keys: impl IntoIterator<Item = GtsTypeKey>,
) -> Result<TypePaths, DomainError> {
    let mut unique: Vec<GtsTypeKey> = keys.into_iter().collect();
    unique.sort_unstable_by_key(|key| key.0);
    unique.dedup();
    let mut paths = HashMap::with_capacity(unique.len());
    for chunk in unique.chunks(toolkit_db::secure::max_bind_params_for(db)) {
        let rows = Entity::find()
            .filter(Column::Id.is_in(chunk.iter().map(|key| key.0)))
            .secure()
            .scope_with(&dictionary_scope())
            .all(db)
            .await
            .map_err(map_scope_error)?;
        paths.extend(rows.into_iter().map(|r| (GtsTypeKey(r.id), r.schema_id)));
    }
    if let Some(missing) = unique.iter().find(|k| !paths.contains_key(k)) {
        return Err(missing_key(*missing));
    }
    Ok(TypePaths(paths))
}

/// The identifier for one stored key; see [`find_paths`].
pub async fn find_path(db: &impl DBRunner, key: GtsTypeKey) -> Result<String, DomainError> {
    find_paths(db, [key]).await?.code(key)
}

/// Select dictionary keys for SQL predicates. Unknown identifiers contribute
/// no keys. Both columns are `NOT NULL`, preserving boolean semantics for
/// `IN`, `NOT IN` and compound predicates.
pub fn keys_subquery<I, S>(schema_ids: I) -> SelectStatement
where
    I: IntoIterator<Item = S>,
    S: Into<sea_orm::Value>,
{
    Query::select()
        .column(Column::Id)
        .from(Entity)
        .and_where(Column::SchemaId.is_in(schema_ids))
        .to_owned()
}

/// Literal prefix ending at the GTS segment boundary. `SUBSTR` and equality
/// keep underscores literal and preserve case on both `PostgreSQL` and `SQLite`.
pub fn family_keys(root: &str, exact_alias: Option<&str>) -> SelectStatement {
    use sea_orm::ExprTrait;
    use sea_orm::sea_query::{Alias, Expr, Func};
    let prefix = Func::cust(Alias::new("substr"))
        .arg(Expr::col(Column::SchemaId))
        .arg(1)
        .arg(Func::char_length(root));
    let condition = sea_orm::Condition::any()
        .add(prefix.eq(root))
        .add_option(exact_alias.map(|alias| Column::SchemaId.eq(alias)));
    Query::select()
        .column(Column::Id)
        .from(Entity)
        .cond_where(condition)
        .to_owned()
}

/// `SELECT id FROM gts_type WHERE schema_id LIKE '<prefix>%' OR ...`.
pub fn keys_with_prefixes_subquery(prefixes: &[String]) -> SelectStatement {
    let any = prefixes
        .iter()
        .fold(sea_orm::Condition::any(), |cond, prefix| {
            cond.add(Column::SchemaId.starts_with(prefix.as_str()))
        });
    Query::select()
        .column(Column::Id)
        .from(Entity)
        .cond_where(any)
        .to_owned()
}
