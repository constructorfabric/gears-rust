use cf_gears_durable_execution_sdk::prelude::*;
async fn read(client: &DurableExecutionClient, ctx: &toolkit_security::SecurityContext, handle: &RunHandle<u32,String>) {
    let _: u32 = client.result(ctx, handle).await.unwrap();
}
fn main() {}
