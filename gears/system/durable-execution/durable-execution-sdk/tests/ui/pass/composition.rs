use cf_gears_durable_execution_sdk::prelude::*;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let builder = WorkflowBuilder::<u32>::new("typed.valid.v1")
        .then(Step::new("first", |_, n: u32| async move { Ok(n + 1) })
            .timeout(Duration::from_secs(1)));
    let checkpoint = builder.checkpoint();
    let dependency = checkpoint.clone();
    let _workflow = builder.parallel((
        Step::new("number", |_, n: u32| async move { Ok(n) })
            .timeout(Duration::from_secs(1)),
        Step::new("text", |_, n: u32| async move { Ok(n.to_string()) })
            .timeout(Duration::from_secs(1)),
    )).then(Step::new("join", move |ctx, (n, text): (u32, String)| {
        let checkpoint = checkpoint.clone();
        async move { Ok(format!("{n}:{text}:{}", ctx.checkpoint(&checkpoint)?)) }
    }).timeout(Duration::from_secs(1)).uses(&dependency)).build()?;
    Ok(())
}
