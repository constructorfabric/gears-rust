use toolkit_durable_execution::prelude::*;
use std::{sync::Arc, time::Duration};

struct Mailer {
    sent: std::sync::atomic::AtomicU32,
}
impl Activity for Mailer {
    type Input = String;
    type Output = u32;
    async fn execute(&self, ctx: ActivityContext, to: String) -> Result<u32, ActivityError> {
        let _ = (ctx.idempotency_key(), to);
        "1".parse::<u32>().or_retryable("mail_rejected")?;
        Ok(self.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

async fn register(registry: WorkflowRegistry) -> Result<WorkflowRef<String, u32>, Box<dyn std::error::Error>> {
    let workflow = WorkflowBuilder::<String>::new("typed.activity.v1")
        .then(Step::activity("send", Duration::from_secs(1), Mailer { sent: 0.into() }).retry(RetryPolicy::no_retry()))
        .build()?;
    Ok(registry.register_workflow(workflow).await?.reference)
}

fn main() {
    let _ = register;
    let _ = StartOptions::new().idempotency_key("k").coalescing_key("c");
    let _ = CancelOptions::new(1).reason("user");
}
