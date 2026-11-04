use toolkit_durable_execution::prelude::*;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let builder = WorkflowBuilder::<u32>::new("typed.valid.v1")
        .then(Step::new("first", Duration::from_secs(1), |_, n: u32| async move { Ok(n + 1) }));
    let first = builder.checkpoint();
    let builder = builder.parallel((
        Step::new("number", Duration::from_secs(1), |_, n: u32| async move { Ok(n) }),
        Step::new("text", Duration::from_secs(1), |_, n: u32| async move { Ok(n.to_string()) }),
    ));
    let (number, _text) = builder.checkpoint().branches();
    let _workflow = builder
        .then(Step::with_deps(
            "join",
            Duration::from_secs(1),
            (&first, &number),
            |_, (n, text): (u32, String), (first, number): (u32, u32)| async move {
                Ok(format!("{n}:{text}:{first}:{number}"))
            },
        ))
        .build()?;
    Ok(())
}
