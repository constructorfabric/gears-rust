//! Repositories accept any scoped transaction or connection runner.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-scoped-repositories:p1
use super::RepoError;
use toolkit_db::secure::ScopeError;
pub mod approval_repo;
pub mod audit_repo;
pub mod book_repo;
pub mod dimension_repo;
pub mod idempotency_repo;
pub mod plan_item_repo;
pub mod plan_repo;
pub mod plan_revision_repo;
pub mod plan_summary;
pub mod price_book_entry_repo;
pub mod price_repo;
pub mod reference_op_repo;
pub mod settings_repo;
/// The latest of `instant` over a group, as text [`latest_instant`] reads back (D-441): on
/// Postgres the `timestamptz` maximum rendered in UTC to the microsecond it keeps; on `SQLite`,
/// where an instant is RFC 3339 text whose fraction has as many digits as it needs (so `…00Z`
/// sorts after `…00.5Z`, and `…00.41868Z` after `…00.418681Z`, P-D-213), the maximum of a
/// fixed-width key that pads the fraction to nine digits, so the text sorts as time. Pricing
/// writes every instant in UTC (`Z`); `NULL` stays out of the maximum.
//
// Raw SQL, on purpose (whole-branch review PS-02): the maximum must compare as time on both
// dialects, and neither half has a portable spelling in sea-query: Postgres renders the
// `timestamptz` maximum with `to_char … AT TIME ZONE 'UTC'`, and `SQLite`, which stores RFC 3339
// text with a fraction of any width, pads that fraction with `substr`/`rtrim` so the text sorts
// as time. The instant is the only operand, and it stays an expression of the scoped select.
// Upstream gears use the same pattern in repository code: account-management
// `infra/lease/manager.rs` (`Expr::cust("NOW()")`, `INTERVAL`) and
// `infra/storage/repo_impl/retention.rs` (`make_interval`, `julianday`), and settings-service
// `infra/storage/search_repo.rs` (`LIKE … ESCAPE`, the JSON null checks). A toolkit-db helper
// would be a change to a foreign crate, proposed upstream on its own (owner, O3/O4).
#[must_use]
pub fn latest(
    backend: sea_orm::DbBackend,
    instant: sea_orm::sea_query::Expr,
) -> sea_orm::sea_query::Expr {
    use sea_orm::sea_query::Expr;
    if backend == sea_orm::DbBackend::Postgres {
        Expr::cust_with_expr(
            r#"to_char(MAX($1) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US')"#,
            instant,
        )
    } else {
        Expr::cust_with_exprs(
            "MAX(substr(?, 1, 19) || substr(CASE WHEN substr(?, 20, 1) = '.' \
             THEN rtrim(substr(?, 20), 'Z') ELSE '.' END || '000000000', 1, 10))",
            [instant.clone(), instant.clone(), instant],
        )
    }
}
/// The instant [`latest`] rendered, in UTC.
/// # Errors
/// `CorruptRow` for text that is not such an instant (a stored instant pricing did not write).
pub fn latest_instant(text: Option<&str>) -> Result<Option<time::OffsetDateTime>, RepoError> {
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond]"
    );
    text.map(|t| {
        time::PrimitiveDateTime::parse(t, &format)
            .map(time::PrimitiveDateTime::assume_utc)
            .map_err(|e| RepoError::CorruptRow(format!("the latest instant {t:?}: {e}")))
    })
    .transpose()
}
/// `lower(expr)`, folded through [`book_repo::PG_FOLD_COLLATION`] on Postgres and through the
/// database's own `lower()` on `SQLite`.
///
/// Raw SQL, on purpose: sea-query has no `COLLATE` on an expression, and the fold must name the
/// ICU collation, or a `C`-locale database folds ASCII only. The folded text stays a bound value.
pub(super) fn folded(
    backend: sea_orm::DbBackend,
    expr: sea_orm::sea_query::Expr,
) -> sea_orm::sea_query::Expr {
    use sea_orm::sea_query::Expr;
    if backend == sea_orm::DbBackend::Postgres {
        Expr::cust_with_expr(
            format!(r#"lower($1 COLLATE "{}")"#, book_repo::PG_FOLD_COLLATION),
            expr,
        )
    } else {
        Expr::expr(sea_orm::sea_query::Func::lower(expr))
    }
}

/// `lower(column) LIKE lower(pattern) ESCAPE '\'` over any of `columns`. The caller's text is
/// matched literally (`%`, `_` and `\` escaped). Both sides fold the same way.
pub(super) fn text_like_any(
    text: &str,
    backend: sea_orm::DbBackend,
    columns: impl IntoIterator<Item = sea_orm::sea_query::Expr>,
) -> sea_orm::Condition {
    use sea_orm::sea_query::{BinOper, Expr, ExprTrait};
    use toolkit_db::odata::sea_orm_filter::escape_like;
    let pattern = format!("%{}%", escape_like(text));
    columns
        .into_iter()
        .fold(sea_orm::Condition::any(), |any, column| {
            let lowered = folded(backend, Expr::val(pattern.clone())).binary(
                BinOper::Escape,
                Expr::Constant(sea_orm::Value::Char(Some('\\'))),
            );
            any.add(folded(backend, column).binary(BinOper::Like, lowered))
        })
}

/// Preserve the driver's variant for serializable retries.
#[must_use]
pub fn driver_failure(context: String, error: ScopeError) -> RepoError {
    match error {
        ScopeError::Db(source) => RepoError::Driver { context, source },
        other => RepoError::Db(format!("{context}: {other}")),
    }
}
/// The conflict of a price's `(entry, version_no)` key: the create and the PATCH retry on it with
/// the entry's next number, matched on this one symbol, never a second literal (PS-41).
pub const PRICE_VERSION_TAKEN: &str = "PRICE_VERSION_TAKEN";
/// Identify named Postgres constraints and `SQLite` unique column/index diagnostics.
#[must_use]
pub fn unique_code(message: &str) -> Option<&'static str> {
    if message.contains("pricing_price_book_tenant_id_code_key")
        || message.contains("pricing_price_book.tenant_id, pricing_price_book.code")
    {
        Some("BOOK_CODE_TAKEN")
    } else if message.contains("pricing_price_book_entry_key") {
        Some("ENTRY_KEY_TAKEN")
    } else if message.contains("pricing_price_approved_start") {
        Some("WINDOW_OVERLAP")
    } else if message.contains("pricing_price_price_book_entry_id_version_no_key")
        || message.contains("pricing_price.price_book_entry_id, pricing_price.version_no")
    {
        Some(PRICE_VERSION_TAKEN)
    } else if message.contains("pricing_dimension_key_pkey")
        || message.contains("pricing_dimension_key.tenant_id, pricing_dimension_key.key")
    {
        Some("DIM_KEY_TAKEN")
    } else {
        plan_unique_code(message)
    }
}
/// The phase 3 keys, and phase 8's scheduled index (D-446). Postgres names the index or
/// constraint; `SQLite` names the columns, which a partial index shares with its siblings: the
/// three single-column revision indexes read alike there and are told apart by
/// `plan_revision_repo`, which knows the state it wrote. Longer column lists are matched before the
/// single column they begin with.
fn plan_unique_code(message: &str) -> Option<&'static str> {
    if message.contains("pricing_plan_code")
        || message.contains("pricing_plan.tenant_id, pricing_plan.code")
    {
        Some("PLAN_CODE_TAKEN")
    } else if message.contains("pricing_plan_revision_no")
        || message.contains("pricing_plan_revision.plan_id, pricing_plan_revision.rev_no")
    {
        Some("REVISION_NO_TAKEN")
    } else if message.contains("pricing_plan_revision_open") {
        Some("REVISION_DRAFT_EXISTS")
    } else if message.contains("pricing_plan_revision_published") {
        Some("REVISION_PUBLISHED_EXISTS")
    } else if message.contains("pricing_plan_revision_scheduled") {
        Some("REVISION_SCHEDULED_EXISTS")
    } else if message.contains("pricing_plan_item_sku")
        || message.contains("pricing_plan_item.revision_id, pricing_plan_item.sku_id")
    {
        Some("ITEM_SKU_TAKEN")
    } else {
        None
    }
}
fn map_unique(context: String, error: ScopeError) -> RepoError {
    if error.is_unique_violation()
        && let Some(code) = unique_code(&error.to_string())
    {
        return RepoError::Conflict { code };
    }
    driver_failure(context, error)
}
/// Refuse a lock that names no approval unit of the tenant.
async fn unit_exists(
    runner: &impl toolkit_db::secure::DBRunner,
    scope: &toolkit_db::secure::AccessScope,
    tenant: uuid::Uuid,
    unit: uuid::Uuid,
    context: &str,
) -> Result<(), RepoError> {
    let parent = approval_repo::find_unit(runner, scope, tenant, unit)
        .await
        .map_err(|error| {
            error.db_err().map_or_else(
                || RepoError::Db(error.to_string()),
                |source| RepoError::Driver {
                    context: context.to_owned(),
                    source: source.clone(),
                },
            )
        })?;
    if parent.is_none() {
        return Err(RepoError::Conflict {
            code: "UNIT_NOT_FOUND",
        });
    }
    Ok(())
}
fn matched(rows: u64, code: &'static str) -> Result<(), RepoError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(RepoError::Conflict { code })
    }
}

pub mod usage_policy_repo;

pub mod acceptance_repo;

pub mod hold_repo;

pub mod commercial_command_repo;
