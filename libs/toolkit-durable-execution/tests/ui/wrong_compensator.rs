use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() {
    Step::new("item", Duration::from_secs(1), |_, n: u32| async move { Ok(n.to_string()) })
        .compensate(Compensation::new(Duration::from_secs(1), |_, _: String, _: String| async { Ok(()) }));
}
