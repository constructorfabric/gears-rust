//! JSON report serializer — equivalent to Python `json.dumps(..., indent=2, ensure_ascii=False)`.

use crate::model::Report;

/// Serialises the report as pretty-printed JSON.
pub fn render(rep: &Report) -> String {
    // `serde_json::to_string_pretty` defaults to a 2-space indent and never
    // ASCII-escapes non-ASCII characters, matching the Python output options.
    serde_json::to_string_pretty(rep).expect("Report is always JSON-serialisable")
}
