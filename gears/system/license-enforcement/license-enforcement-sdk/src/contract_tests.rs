//! Tests against the IR that `#[toolkit::contract]` emits.

use toolkit_contract::ir::contract::Idempotency;

use crate::license_enforcement_api_ir;

const READS: &[&str] = &[
    "get_pack",
    "list_packs",
    "get_grant",
    "list_grants",
    "get_delegation",
    "check_explain",
];

/// A method without `#[idempotency]` defaults to `NonIdempotentWrite`, which no
/// method of this contract is: every write carries an idempotency token.
#[test]
fn reads_are_safe_and_every_write_is_idempotent() {
    for method in license_enforcement_api_ir().methods {
        let expected = if READS.contains(&method.name.as_str()) {
            Idempotency::SafeRead
        } else {
            Idempotency::IdempotentWrite
        };
        assert_eq!(method.idempotency, expected, "{}", method.name);
    }
}
