//! Log-safe decode diagnostics (D-44, D-218): the serde failure category is logged, never the
//! serde message, which quotes the offending input (a client body or stored content).

/// The serde failure category token: `syntax`, `data`, `eof` or `io`.
#[must_use]
pub fn serde_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
        serde_json::error::Category::Io => "io",
    }
}
