use event_broker_sdk::backend::BackendError;

use crate::error::{Operation, map, map_server};

fn mapped(error: &BackendError) -> String {
    format!("{error:?}")
}

#[test]
fn a_refused_statement_maps_by_operation() {
    assert_eq!(
        [
            Operation::Append,
            Operation::Read,
            Operation::Resolve,
            Operation::Watch,
            Operation::Query,
            Operation::Provision,
        ]
        .map(|op| mapped(&map_server(op, 1366, "HY000"))),
        [
            r#"AppendFailed { reason: "append: database error 1366 (SQLSTATE HY000)", detail: "" }"#,
            r#"ReadFailed { reason: "read: database error 1366 (SQLSTATE HY000)", detail: "" }"#,
            r#"ReadFailed { reason: "resolve: database error 1366 (SQLSTATE HY000)", detail: "" }"#,
            r#"ReadFailed { reason: "watch: database error 1366 (SQLSTATE HY000)", detail: "" }"#,
            r#"ReadFailed { reason: "query: database error 1366 (SQLSTATE HY000)", detail: "" }"#,
            r#"Internal("provision: database error 1366 (SQLSTATE HY000)")"#,
        ]
    );
}

#[test]
fn transient_errors_are_unavailable_in_every_operation() {
    for number in [
        1205, 1213, 1040, 1053, 1317, 1927, 3024, 1969, 1290, 1836, 1792, 3032, 4031, 1526,
    ] {
        for op in [Operation::Append, Operation::Read, Operation::Provision] {
            assert!(
                matches!(
                    map_server(op, number, "HY000"),
                    BackendError::Unavailable { .. }
                ),
                "{number} in {op}"
            );
        }
    }
    assert!(matches!(
        map_server(Operation::Read, 2000, "08S01"),
        BackendError::Unavailable { .. }
    ));
}

#[test]
fn refused_credentials_are_unavailable() {
    assert_eq!(
        mapped(&map_server(Operation::Append, 1045, "28000")),
        r#"Unavailable { reason: "append: database error 1045 (SQLSTATE 28000)", detail: "" }"#
    );
}

#[test]
fn driver_failures_name_the_kind_only() {
    assert_eq!(
        [
            mapped(&map(Operation::Read, &sqlx::Error::PoolTimedOut)),
            mapped(&map(
                Operation::Append,
                &sqlx::Error::Protocol("tenant-secret-42".to_owned())
            )),
            mapped(&map(
                Operation::Query,
                &sqlx::Error::ColumnNotFound("tenant-secret-42".to_owned())
            )),
        ],
        [
            r#"Unavailable { reason: "read: pool timed out", detail: "" }"#,
            r#"Unavailable { reason: "append: protocol error", detail: "" }"#,
            r#"Internal("query: column not found")"#,
        ]
    );
}

#[test]
fn connection_limits_and_commit_failures_are_unavailable() {
    for number in [1203, 1226, 3100, 3101, 1180] {
        for op in [Operation::Append, Operation::Read, Operation::Provision] {
            assert!(
                matches!(
                    map_server(op, number, "HY000"),
                    BackendError::Unavailable { .. }
                ),
                "{number} in {op}"
            );
        }
    }
}

#[test]
fn a_missing_table_is_a_failure_of_the_operation() {
    assert_eq!(
        [Operation::Append, Operation::Read, Operation::Provision]
            .map(|op| mapped(&map_server(op, 1146, "42S02"))),
        [
            r#"AppendFailed { reason: "append: database error 1146 (SQLSTATE 42S02)", detail: "" }"#,
            r#"ReadFailed { reason: "read: database error 1146 (SQLSTATE 42S02)", detail: "" }"#,
            r#"Internal("provision: database error 1146 (SQLSTATE 42S02)")"#,
        ]
    );
}
