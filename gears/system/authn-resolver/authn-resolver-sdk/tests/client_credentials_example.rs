//! Keep the runnable example and its checked workflow identical.

#[path = "../examples/cached_client_credentials.rs"]
mod cached_client_credentials;

#[tokio::test]
async fn the_documented_cached_credentials_example_runs() -> Result<(), Box<dyn std::error::Error>>
{
    cached_client_credentials::run().await
}
