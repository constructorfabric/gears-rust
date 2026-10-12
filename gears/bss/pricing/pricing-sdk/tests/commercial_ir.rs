//! Contract IR pins all seven methods and keeps read capability command-free.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[test]
fn commercial_classifications_and_read_only_capability_are_exact() {
    use bss_pricing_sdk::{
        acceptance::{pricing_acceptance_v1_ir, sellability_v1_ir},
        read::pricing_read_v1_ir,
    };
    use toolkit_contract::ir::contract::{FieldRole, Idempotency};
    let read = pricing_read_v1_ir();
    assert_eq!(
        read.methods
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        ["resolve", "price", "current_revision"]
    );
    let methods: Vec<_> = [read, pricing_acceptance_v1_ir(), sellability_v1_ir()]
        .into_iter()
        .flat_map(|i| i.methods)
        .collect();
    assert_eq!(methods.len(), 7);
    assert_eq!(
        methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        [
            "resolve",
            "price",
            "current_revision",
            "acceptance",
            "hold",
            "check",
            "check_fulfilment"
        ]
    );
    for method in methods {
        let command = matches!(method.name.as_str(), "check" | "hold");
        assert_eq!(
            method.idempotency,
            if command {
                Idempotency::IdempotentWrite
            } else {
                Idempotency::SafeRead
            }
        );
        assert_eq!(method.input.fields[0].role, FieldRole::SecurityContext);
        assert_eq!(
            method.input.fields.iter().any(|f| f.name == "meta"),
            command
        );
    }
}
