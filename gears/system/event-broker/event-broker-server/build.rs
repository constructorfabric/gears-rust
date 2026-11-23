#[cfg(not(feature = "e2e-flight-control"))]
fn main() {}

#[cfg(feature = "e2e-flight-control")]
fn main() -> std::io::Result<()> {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../testing/e2e/test-flight-control/proto");
    let proto = directory.join("bearer_validation.proto");
    println!("cargo:rerun-if-changed={}", proto.display());
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(false)
        .skip_debug([".test_flight_control.v1.AuthenticateRequest"])
        .compile_protos(&[proto], &[directory])
}
