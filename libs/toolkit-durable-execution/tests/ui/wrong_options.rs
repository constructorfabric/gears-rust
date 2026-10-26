use toolkit_durable_execution::prelude::*;
fn main() {
    let _ = StartOptions { idempotency_key: None, coalescing_key: None, expected_fingerprint: None };
}
