use cf_gears_durable_execution_sdk::prelude::*;
fn main() {
    let _ = StartOptions { idempotency_key: None, coalescing_key: None, expected_fingerprint: None };
}
