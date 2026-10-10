use cf_gears_durable_execution_sdk::prelude::*;
use std::time::Duration;
fn main() {
    let _ = WorkflowBuilder::<u32>::new("typed.invalid.v1")
        .then(Step::new("first", |_, n: u32| async move { Ok(n.to_string()) }).timeout(Duration::from_secs(1)))
        .then(Step::new("second", |_, n: u32| async move { Ok(n) }).timeout(Duration::from_secs(1)));
}
