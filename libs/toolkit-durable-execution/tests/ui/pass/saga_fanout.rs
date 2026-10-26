use toolkit_durable_execution::prelude::*;
use std::time::Duration;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let builder = WorkflowBuilder::<Vec<u32>>::new("typed.saga-fanout.v1").saga()
        .for_each(Step::new("reserve", Duration::from_secs(1), |_, n: u32| async move { Ok(n.to_string()) })
            .compensate(Compensation::new(Duration::from_secs(1), |_, _: u32, _: String| async { Ok(()) })),
            FanOutOptions::default().concurrency(4));
    let checkpoint = builder.checkpoint();
    let branch: StepRef<String> = checkpoint.branch(0);
    let workflow = builder.then(Step::new("join", Duration::from_secs(1), |_, v: Vec<String>| async move { Ok(v.len()) })).build()?;
    let handle: RunHandle<Vec<u32>, usize> = workflow.reference().attach(RunId::from(uuid::Uuid::nil()));
    assert_eq!(handle.workflow().fingerprint(), branch.fingerprint()?);
    Ok(())
}
