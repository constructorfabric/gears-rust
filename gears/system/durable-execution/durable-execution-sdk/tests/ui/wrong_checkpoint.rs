use cf_gears_durable_execution_sdk::prelude::*;
async fn read(client: &DurableExecutionClient, ctx: &toolkit_security::SecurityContext, handle: &RunHandle<u32,String>) {
    let _:u32 = client.step_result(ctx,handle,&42u32).await.unwrap();
}
fn main() {}
