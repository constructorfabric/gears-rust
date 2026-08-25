//! `#[derive(ProtoBridge)]` only supports structs with named fields — must fail
//! on a tuple struct.

use toolkit_contract::ProtoBridge;

mod stubs {
    pub struct Request(pub i64);
}

#[derive(ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::Request")]
pub struct Request(pub i64);

fn main() {}
