//! `#[derive(ProtoBridge)]` enum variants must be unit-only — variants with
//! payload must fail.

use toolkit_contract::ProtoBridge;

mod stubs {
    pub enum Status {
        Pending,
        Completed(i64),
    }
}

#[derive(ProtoBridge)]
#[proto_bridge(stub = "crate::stubs::Status")]
pub enum Status {
    Pending,
    Completed(i64),
}

fn main() {}
