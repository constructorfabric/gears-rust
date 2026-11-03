use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    WorkflowBuilder::<Vec<u32>>::new("wrong.fanout.v1")
        .for_each(Step::new("item", Duration::from_secs(1), |_, _: String| async { Ok(1u32) }), FanOutOptions::default());
}
