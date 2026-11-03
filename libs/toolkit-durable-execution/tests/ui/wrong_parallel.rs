use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    let _ = WorkflowBuilder::<u32>::new("typed.invalid.v1").parallel((
        Step::new("number", Duration::from_secs(1), |_, n: u32| async move { Ok(n) }),
        Step::new("text", Duration::from_secs(1), |_, n: String| async move { Ok(n) }),
    ));
}
