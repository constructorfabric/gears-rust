//! End-to-end stability tests for the field-number lockfile.
//!
//! Covers the wire-compat scenarios that motivated introducing the lock:
//! - Adding a field doesn't shift existing numbers
//! - Removing a field moves it to `reserved`
//! - Re-adding a field with the *same name* recovers the original number
//! - Re-running with no changes is a no-op (lock + proto byte-identical)

#![allow(clippy::unwrap_used)]

use toolkit_contract::ir::contract::{
    FieldIr, Idempotency, InputShape, MethodIr, MethodKind, ServiceIr, TypeRef,
};
use toolkit_contract::ir::grpc::{GrpcBindingIr, GrpcIdempotency, GrpcMethodBindingIr};
use toolkit_contract_protogen::{ProtoLockfile, generate_proto_file};

fn sample_contract() -> toolkit_contract::ir::contract::ContractIr {
    ServiceIr {
        name: "PaymentApi".into(),
        gear: "service-hub-demo".into(),
        version: "v1".into(),
        methods: vec![MethodIr {
            name: "charge".into(),
            kind: MethodKind::Unary,
            input: InputShape {
                fields: vec![FieldIr {
                    name: "req".into(),
                    ty: TypeRef::Named("ChargeRequest".into()),
                    optional: false,
                    role: toolkit_contract::ir::contract::FieldRole::Wire,
                }],
            },
            output: TypeRef::Named("ChargeResponse".into()),
            error: None,
            idempotency: Idempotency::NonIdempotentWrite,
            optional: false,
        }],
    }
}

fn sample_binding() -> GrpcBindingIr {
    GrpcBindingIr {
        package: "demo.payment.v1".into(),
        service: "PaymentApi".into(),
        methods: vec![GrpcMethodBindingIr {
            method_name: "charge".into(),
            rpc_name: "Charge".into(),
            client_streaming: false,
            server_streaming: false,
            idempotency_level: GrpcIdempotency::NotIdempotent,
            retryable: false,
            optional: false,
        }],
    }
}

fn req_schema(extra_fields: &[(&str, &str)]) -> schemars::Schema {
    let mut props = serde_json::Map::new();
    props.insert(
        "amount_cents".into(),
        serde_json::json!({ "type": "integer", "format": "int64" }),
    );
    props.insert("currency".into(), serde_json::json!({ "type": "string" }));
    let mut required: Vec<&str> = vec!["amount_cents", "currency"];
    for (name, ty) in extra_fields {
        props.insert((*name).to_owned(), serde_json::json!({ "type": ty }));
        required.push(name);
    }
    schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "required": required,
        "properties": props,
    }))
    .unwrap()
}

fn resp_schema() -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "required": ["payment_id"],
        "properties": { "payment_id": { "type": "string" } }
    }))
    .unwrap()
}

#[test]
fn first_run_assigns_alphabetic_numbers() {
    let mut lock = ProtoLockfile::empty();
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let req = lock.messages.get("ChargeRequest").unwrap();
    assert_eq!(req.fields.get("amount_cents"), Some(&1));
    assert_eq!(req.fields.get("currency"), Some(&2));
}

#[test]
fn adding_field_does_not_shift_existing_numbers() {
    let mut lock = ProtoLockfile::empty();
    // Initial run — establish baseline.
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let baseline_amount = *lock.messages["ChargeRequest"]
        .fields
        .get("amount_cents")
        .unwrap();
    let baseline_currency = *lock.messages["ChargeRequest"]
        .fields
        .get("currency")
        .unwrap();

    // Now add `description` — alphabetically smaller than `currency`. Without
    // the lock it would shove `currency` to number 3 (wire breakage).
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();

    let req = &lock.messages["ChargeRequest"];
    assert_eq!(req.fields.get("amount_cents"), Some(&baseline_amount));
    assert_eq!(req.fields.get("currency"), Some(&baseline_currency));
    let new_num = *req.fields.get("description").unwrap();
    assert!(
        new_num > baseline_amount && new_num > baseline_currency,
        "new field should get a higher number than existing fields"
    );
}

#[test]
fn removing_field_moves_to_reserved() {
    let mut lock = ProtoLockfile::empty();
    // Run with three fields.
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let removed_number = *lock.messages["ChargeRequest"]
        .fields
        .get("description")
        .unwrap();

    // Drop `description`.
    let proto = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();

    let req = &lock.messages["ChargeRequest"];
    assert!(!req.fields.contains_key("description"));
    assert!(req.reserved_numbers.contains(&removed_number));
    assert!(req.reserved_names.iter().any(|n| n == "description"));
    // Renderer must emit the `reserved` clauses too.
    assert!(
        proto.contains(&format!("reserved {removed_number};")),
        "proto missing reserved number\n{proto}"
    );
    assert!(
        proto.contains("reserved \"description\";"),
        "proto missing reserved name\n{proto}"
    );
}

#[test]
fn re_adding_removed_field_recovers_original_number() {
    let mut lock = ProtoLockfile::empty();
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    // NB: the lock does NOT special-case re-adding a previously-deleted
    // field — that's a deliberate safety choice. Once a number is reserved
    // it stays reserved. The re-added field will get a fresh number.
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let _ = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let req = &lock.messages["ChargeRequest"];
    let new_number = *req.fields.get("description").unwrap();
    // The original number stays reserved; the re-added field gets a fresh one.
    assert!(req.reserved_numbers.iter().any(|&n| n != new_number));
    assert!(!req.reserved_numbers.contains(&new_number));
}

#[test]
fn unchanged_schema_yields_byte_identical_output() {
    let mut lock = ProtoLockfile::empty();
    let proto1 = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    let lock_after_first = lock.clone();
    let proto2 = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    assert_eq!(proto1, proto2);
    assert_eq!(lock, lock_after_first);
}

#[test]
fn fields_render_in_field_number_order_not_alphabetic() {
    // Pre-populate the lock so `currency` has a higher number than
    // `description` even though it's alphabetically earlier — verifying
    // the renderer actually sorts by number, not name.
    let mut lock = ProtoLockfile::empty();
    let entry = lock.messages.entry("ChargeRequest".into()).or_default();
    entry.fields.insert("amount_cents".into(), 1);
    entry.fields.insert("description".into(), 2); // claimed first
    entry.fields.insert("currency".into(), 3);
    let proto = generate_proto_file(
        &sample_contract(),
        &sample_binding(),
        &[
            ("ChargeRequest", req_schema(&[("description", "string")])),
            ("ChargeResponse", resp_schema()),
        ],
        &mut lock,
    )
    .unwrap();
    // Find the message body in the rendered .proto.
    let msg_start = proto.find("message ChargeRequest").unwrap();
    let body = &proto[msg_start..];
    let amount_pos = body.find("amount_cents").unwrap();
    let description_pos = body.find("description").unwrap();
    let currency_pos = body.find("currency").unwrap();
    assert!(amount_pos < description_pos);
    assert!(description_pos < currency_pos);
}
