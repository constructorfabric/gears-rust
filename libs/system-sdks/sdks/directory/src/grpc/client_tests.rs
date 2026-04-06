use super::*;

#[tokio::test]
async fn test_grpc_client_can_be_constructed() {
    // Smoke test to ensure types compile and connect
    let endpoint = tonic::transport::Endpoint::from_static("http://[::1]:50051");

    // We can't actually connect without a server, but we can construct the client type
    // This ensures the API is correct
    let channel_result = endpoint.connect().await;

    // It's expected to fail since there's no server, but if it does somehow succeed:
    if let Ok(channel) = channel_result {
        let _client = DirectoryGrpcClient::from_channel(channel);
    }
}
