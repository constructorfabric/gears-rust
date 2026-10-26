use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    WorkflowBuilder::<u32>::new("wrong.branches.v1")
        .then(Step::new("pair", Duration::from_secs(1), |_, n: u32| async move { Ok((n,n)) }))
        .checkpoint().branches();
}
