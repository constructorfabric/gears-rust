fn main() {
    #[cfg(feature = "fixture-authn")]
    {
        const PROTO: &str = "proto/bearer_validation.proto";
        println!("cargo:rerun-if-changed={PROTO}");
        tonic_prost_build::configure()
            .build_client(false)
            .build_server(true)
            .skip_debug([".test_flight_control.v1.AuthenticateRequest"])
            .compile_protos(&[PROTO], &["proto"])
            .unwrap_or_else(|error| panic!("could not compile the fixture bearer proto: {error}"));
    }
}
