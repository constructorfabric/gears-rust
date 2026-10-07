use cf_gears_durable_execution_sdk::prelude::*;
use std::time::Duration;
fn main() {
    let _ = WorkflowBuilder::<u32>::new("typed.invalid.v1").parallel((
        Step::new("number", |_, n: u32| async move { Ok(n) }).timeout(Duration::from_secs(1)),
        Step::new("text", |_, n: String| async move { Ok(n) }).timeout(Duration::from_secs(1)),
    ));
}
