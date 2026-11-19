//! RG-local GTS type identity dictionary (`gts_type`).
//!
//! Groups and memberships store a `SMALLINT` key; this module maps it to and
//! from the full GTS type identifier. The dictionary holds no definitions: a
//! row proves nothing about the type in the types registry (existence,
//! admission, lifecycle), and rows are never updated or deleted, so reads and
//! cleanup keep working for types the registry no longer has.
//!
//! Only [`ensure_key`] writes, and only the write paths that link a new group
//! or membership call it -- after the registry admitted the type (before
//! `BEGIN`), inside the same transaction as the row that references the key,
//! so a failed operation leaves no dictionary row behind. Reads, filters and
//! deletes look keys up and treat a missing row as "nothing stored under this
//! type".

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
/// Reads first, so the common case (type already known) inserts nothing and,
/// on PostgreSQL, consumes no identity value: the `SMALLINT` sequence is
/// spent only by genuinely new types, by a first use whose transaction rolls
/// back, and by the losing side of a first-use race.
///
/// Callers run this inside their write transaction, so the row commits or
/// rolls back with the group or membership that references it. The insert
/// is `ON CONFLICT (schema_id) DO NOTHING`, so a concurrent first use never
/// raises a unique violation (which would abort a PostgreSQL transaction).
/// Under `SERIALIZABLE`, a conflicting row committed after this
/// transaction's snapshot makes PostgreSQL fail the insert with a
/// serialization error; driver errors stay typed ([`DomainError::Database`]),
/// so that reaches the caller's retry loop, whose next attempt reads the
/// winner's row. At weaker levels (and on SQLite) the conflict is reported
/// as "not inserted" and the committed row is re-read.
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

/// `SELECT id FROM gts_type WHERE schema_id IN (schema_ids)`: a subquery for
/// SQL predicates on a stored key column. An identifier with no row
/// contributes nothing, so `key IN (...)` is false and `key NOT IN (...)` is
/// true for it (both columns are `NOT NULL`) -- the boolean meaning of the
/// public string predicate survives `NOT`, `OR` and `in`.
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
