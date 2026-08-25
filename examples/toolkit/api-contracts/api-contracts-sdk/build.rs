#[allow(
    clippy::unnecessary_wraps,
    reason = "the `Result` return is consumed by `?` under the `grpc-client` feature; in other feature configs the body reduces to `Ok(())`, but the signature must stay uniform across configs"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "grpc-client")]
    {
        let proto_path = "proto/api_contracts/payment/v1/payment.proto";
        println!("cargo:rerun-if-changed={proto_path}");
        tonic_prost_build::configure()
            .build_client(true)
            .build_server(true)
            .compile_protos(&[proto_path], &["proto"])?;
    }
    Ok(())
}
