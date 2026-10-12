pub mod attachment_repo;
pub mod chat_repo;
pub mod message_attachment_repo;
pub mod message_repo;
pub mod quota_usage_repo;
pub mod reaction_repo;
pub mod thread_summary_repo;
pub mod turn_repo;
pub mod vector_store_repo;

use crate::domain::error::DomainError;

/// Effective order of a keyset-paginated list: the client's `$orderby`, or
/// `default` when it sets none, always ending with `id` so the cursor
/// predicate is a strict total order (rows tied on the other keys are
/// neither skipped nor repeated across pages). A cursor keeps the order it
/// was issued with, so the query is returned unchanged then.
pub(crate) fn with_id_tiebreaker(
    query: &toolkit_odata::ODataQuery,
    default: (&str, toolkit_odata::SortDir),
    id_dir: toolkit_odata::SortDir,
) -> toolkit_odata::ODataQuery {
    let mut q = query.clone();
    if q.cursor.is_none() {
        q.order = q
            .order
            .ensure_tiebreaker(default.0, default.1)
            .ensure_tiebreaker("id", id_dir);
    }
    q
}

/// Maps an `OData` pagination error: a bad `$filter`, `$orderby`, `limit` or
/// cursor is the client's fault (400); only DB/config failures are internal.
pub(crate) fn odata_err(e: toolkit_odata::Error) -> DomainError {
    use toolkit_odata::Error as E;
    // Exhaustive on purpose: a new variant must be classified here.
    match e {
        E::Db(msg) => DomainError::database(msg),
        E::ParsingUnavailable(msg) => DomainError::database(msg),
        client @ (E::InvalidFilter(_)
        | E::InvalidOrderByField(_)
        | E::OrderMismatch
        | E::FilterMismatch
        | E::InvalidCursor
        | E::InvalidLimit
        | E::OrderWithCursor
        | E::CursorInvalidBase64
        | E::CursorInvalidJson
        | E::CursorInvalidVersion
        | E::CursorInvalidKeys
        | E::CursorInvalidFields
        | E::CursorInvalidDirection) => DomainError::OData(client),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "repo_test.rs"]
mod repo_test;
