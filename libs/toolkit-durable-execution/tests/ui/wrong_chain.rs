use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    let _ = WorkflowBuilder::<u32>::new("typed.invalid.v1")
        .then(Step::new("first", Duration::from_secs(1), |_, n: u32| async move { Ok(n.to_string()) }))
        .then(Step::new("second", Duration::from_secs(1), |_, n: u32| async move { Ok(n) }));
}
