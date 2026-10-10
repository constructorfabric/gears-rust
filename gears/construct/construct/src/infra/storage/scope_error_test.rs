//! The `ScopeError` to `DomainError` mapping.

use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use uuid::Uuid;

use super::scope_error::map_scope_error;
use crate::domain::error::DomainError;

#[test]
fn map_scope_error_maps_each_variant() {
    let tenant_id = Uuid::new_v4();
    let cases: Vec<(ScopeError, &str)> = vec![
        (ScopeError::Denied("nope"), "forbidden"),
        (ScopeError::Invalid("bad"), "internal"),
        (ScopeError::TenantNotInScope { tenant_id }, "forbidden"),
        (
            ScopeError::Db(sea_orm::DbErr::Custom("boom".to_owned())),
            "database",
        ),
        (
            ScopeError::UnresolvedScopeProperty {
                element: "n",
                property: "p".to_owned(),
            },
            "internal",
        ),
    ];

    for (input, expected) in cases {
        let label = format!("{input:?}");
        let kind = match map_scope_error(input) {
            DomainError::Forbidden(_) => "forbidden",
            DomainError::Internal(_) => "internal",
            DomainError::Database(DbError::Sea(_)) => "database",
            other => panic!("{label} mapped to unexpected {other:?}"),
        };
        assert_eq!(kind, expected, "{label}");
    }
}

#[test]
fn unknown_scope_error_message_is_neutral() {
    let err = map_scope_error(ScopeError::UnresolvedScopeProperty {
        element: "n",
        property: "p".to_owned(),
    });
    let DomainError::Internal(msg) = err else {
        panic!("expected Internal");
    };
    assert!(msg.starts_with("unhandled scope error"), "{msg}");
}
