//! An empty contract list provides nothing.

use toolkit_contract::provides;

#[allow(dead_code)]
#[provides(transports = [grpc], contract = [])]
struct Provider;

fn main() {}
