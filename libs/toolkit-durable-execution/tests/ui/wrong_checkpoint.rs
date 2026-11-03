use toolkit_durable_execution::prelude::*;
async fn read(client: &ExecutionClient, ctx: &toolkit_security::SecurityContext, handle: &RunHandle<u32,String>) {
    let _:u32 = client.step_result(ctx,handle,&42u32).await.unwrap();
}
fn main() {}
