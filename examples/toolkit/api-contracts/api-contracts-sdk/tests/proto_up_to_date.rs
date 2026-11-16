//! The committed `.proto` is what the contract generates. Fails on drift;
//! `TOOLKIT_PROTO_UPDATE=1 cargo test` rewrites it.
#![cfg(feature = "grpc-client")]

use std::path::PathBuf;

use cf_api_contracts_sdk::contract::payment_api_ir;
use cf_api_contracts_sdk::grpc::{payment_api_grpc_binding, payment_api_grpc_schemas};
use toolkit_contract_protogen::{ProtoSpec, SchemaCollector, check_proto};

#[test]
fn the_committed_proto_is_up_to_date() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut schemas = SchemaCollector::default();
    payment_api_grpc_schemas(&mut schemas);
    check_proto(&ProtoSpec {
        contract: payment_api_ir(),
        binding: payment_api_grpc_binding(),
        schemas,
        proto: manifest_dir.join("proto/api_contracts/payment/v1/payment.proto"),
        lock: manifest_dir.join("proto.lock.toml"),
    });
}
