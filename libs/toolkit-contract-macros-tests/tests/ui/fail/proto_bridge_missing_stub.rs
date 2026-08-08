//! `#[derive(ProtoBridge)]` requires `#[proto_bridge(stub = "...")]` — must fail.

use toolkit_contract::ProtoBridge;

#[derive(ProtoBridge)]
pub struct Request {
    pub amount: i64,
}

fn main() {}
