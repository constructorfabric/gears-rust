/// Install the FIPS-validated AWS-LC crypto provider as the process-wide default.
///
/// This **must** be called before any TLS configuration, HTTP client, database
/// connection, or JWT operation is created. It sets the global [`rustls::crypto::CryptoProvider`]
/// that all downstream consumers (rustls, sqlx, jsonwebtoken, pingora, etc.) will use.
///
/// Outputs to stderr (always visible, even before tracing is initialized) and
/// to `tracing::info!` (visible in structured logs if a subscriber is active).
///
/// # Process exit
///
/// Exits with code 1 if another crypto provider has already been installed.
pub fn init_fips_crypto_provider() {
    if let Err(_existing) = rustls::crypto::default_fips_provider().install_default() {
        eprintln!(
            "[FIPS] FATAL: failed to install FIPS crypto provider - another provider is already installed"
        );
        std::process::exit(1);
    }

    eprintln!("[FIPS] FIPS-140-3 crypto provider installed (AWS-LC FIPS module)");
    tracing::info!("FIPS-140-3 crypto provider installed (AWS-LC FIPS module)");
}
