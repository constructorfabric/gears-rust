//! The category repository over an in-memory database: what the real foreign
//! key makes of a delete.

use serde_json::json;
use toolkit_security::AccessScope;

use super::CategoryRepo;
use crate::domain::category::CategoryRepository;
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn deleting_a_category_a_declaration_references_is_a_conflict_not_an_internal_error() {
    // The authoritative no-orphan guard: `ON DELETE RESTRICT` refuses the
    // delete and `map_delete_error` must read that as the refusal it is.
    let h = ResolutionHarness::new().await;
    h.declare("one", scope_class::LOCAL, json!(true)).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let category = CategoryRepo
        .find(&conn, &scope, h.category_id())
        .await
        .expect("lookup")
        .expect("the harness's category");

    let refused = CategoryRepo
        .delete(&conn, &scope, category.id, category.updated_at)
        .await;

    assert!(
        matches!(refused, Err(DomainError::Conflict { .. })),
        "{refused:?}"
    );
}

/// A driver error with a chosen SQLSTATE and a message in no known language.
#[derive(Debug)]
struct CodedDriverError(&'static str);

impl std::fmt::Display for CodedDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("verletzt Fremdschluessel-Beschraenkung")
    }
}

impl std::error::Error for CodedDriverError {}

impl sea_orm::sqlx::error::DatabaseError for CodedDriverError {
    fn message(&self) -> &'static str {
        "verletzt Fremdschluessel-Beschraenkung"
    }

    fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(self.0))
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> sea_orm::sqlx::error::ErrorKind {
        sea_orm::sqlx::error::ErrorKind::Other
    }
}

fn driver_refusal(code: &'static str) -> toolkit_db::secure::ScopeError {
    let driver = sea_orm::sqlx::Error::Database(Box::new(CodedDriverError(code)));
    toolkit_db::secure::ScopeError::Db(sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(
        std::sync::Arc::new(driver),
    )))
}

#[test]
fn a_restrict_refusal_is_a_conflict_by_its_code_whatever_language_the_message_is_in() {
    // `lc_messages` can put the message in any language; the SQLSTATE cannot
    // change. `23001` and `1811` are the RESTRICT answers `SeaORM` does not
    // file under a foreign-key violation.
    for code in ["23001", "23503", "1811", "787"] {
        let mapped = super::map_delete_error(&driver_refusal(code));
        assert!(
            matches!(mapped, DomainError::Conflict { .. }),
            "{code}: {mapped:?}"
        );
    }
    let other = super::map_delete_error(&driver_refusal("40001"));
    assert!(matches!(other, DomainError::Internal { .. }), "{other:?}");
}
