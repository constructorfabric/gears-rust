use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    let builder = WorkflowBuilder::<u32>::new("typed.invalid.v1")
        .then(Step::new("count", Duration::from_secs(1), |_, n: u32| async move { Ok(n) }));
    let count = builder.checkpoint();
    let _ = builder.then(Step::with_deps("use", Duration::from_secs(1), &count, |_, n: u32, text: String| async move {
        Ok(format!("{n}{text}"))
    }));
}
