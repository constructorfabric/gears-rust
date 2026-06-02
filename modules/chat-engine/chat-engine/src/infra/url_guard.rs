//! Outbound-URL safety guard.
//!
//! Plugin configs ship URLs (`webhook-compat.endpoint`,
//! `llm_gateway.gateway_url`) that Chat Engine then dials with operator
//! credentials attached. Letting those URLs through unvalidated turns
//! either plugin into a Server-Side Request Forgery pivot — a misconfigured
//! tenant could point the webhook at `http://169.254.169.254/...` (cloud
//! metadata), at the in-cluster API server, or at a local debug port and
//! exfiltrate the attached auth header.
//!
//! [`validate_outbound_url`] is the single chokepoint that every
//! plugin-config validator MUST call before persisting a URL. It enforces:
//!
//! 1. The URL parses as an **absolute** URL (`url::Url::parse` succeeds AND
//!    the result has a host) — bare paths, scheme-relative URLs, and
//!    `data:` / `file:` schemes are all rejected at this gate.
//! 2. The scheme is `https`. `http` is rejected by default so a typo or
//!    misconfiguration cannot silently downgrade an internal hop to
//!    cleartext.
//! 3. The host is **not** a literal IP in any of the SSRF-sensitive ranges
//!    (loopback, link-local incl. `169.254.169.254`, private RFC1918, ULA,
//!    multicast, unspecified, broadcast, CGNAT, documentation,
//!    IPv4-mapped IPv6 covering the same ranges).
//! 4. The host is not the literal `localhost`.
//!
//! ## Known gap
//!
//! This is a **parse-time** check. A hostname that resolves to a safe
//! address at config time can still resolve to an internal address at
//! send time (DNS rebinding) — defending against that requires a custom
//! resolver pinned to the parsed IP, which is out of scope here. Track in
//! a follow-up if the threat model warrants it.
//
// @cpt-cf-chat-engine-ssrf-guard:p17

use std::net::{Ipv4Addr, Ipv6Addr};

use chat_engine_sdk::error::PluginError;
use url::{Host, Url};

/// Validate that `raw` is a safe outbound HTTPS URL. `key_name` is the
/// config key the value came from (e.g. `"endpoint"`, `"gateway_url"`) —
/// surfaced in error messages so misconfigurations point operators at
/// the exact field that needs editing. Values themselves are never
/// echoed, mirroring the debug-redaction contract.
///
/// Returns the parsed [`Url`] on success — callers may use it for
/// per-host metric labels or to canonicalise the persisted value.
///
/// # Errors
///
/// Returns [`PluginError::InvalidInput`] when any of the rules in the
/// module-level doc are violated. Each variant carries a short reason so
/// it shows up in logs without surfacing the operator-supplied URL.
pub fn validate_outbound_url(raw: &str, key_name: &str) -> Result<Url, PluginError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PluginError::invalid_input(format!(
            "{key_name} must not be empty",
        )));
    }

    let url = Url::parse(trimmed).map_err(|e| {
        PluginError::invalid_input_with(format!("{key_name} is not a valid absolute URL"), e)
    })?;

    if url.scheme() != "https" {
        return Err(PluginError::invalid_input(format!(
            "{key_name} must use the https scheme; got `{}`",
            url.scheme(),
        )));
    }

    let host = url.host().ok_or_else(|| {
        PluginError::invalid_input(format!("{key_name} must include a host component"))
    })?;

    match host {
        Host::Ipv4(v4) => {
            if is_disallowed_ipv4(v4) {
                return Err(PluginError::invalid_input(format!(
                    "{key_name} resolves to a disallowed IPv4 address range \
                     (loopback / link-local / private / multicast / reserved)",
                )));
            }
        }
        Host::Ipv6(v6) => {
            if is_disallowed_ipv6(v6) {
                return Err(PluginError::invalid_input(format!(
                    "{key_name} resolves to a disallowed IPv6 address range \
                     (loopback / link-local / unique-local / multicast / reserved)",
                )));
            }
        }
        Host::Domain(name) => {
            let lower = name.trim_end_matches('.').to_ascii_lowercase();
            if lower == "localhost" || lower.ends_with(".localhost") {
                return Err(PluginError::invalid_input(format!(
                    "{key_name} must not point at the loopback hostname (`localhost`)",
                )));
            }
        }
    }

    Ok(url)
}

/// True when `addr` is an IPv4 address Chat Engine must never dial as an
/// outbound webhook target. Covers every SSRF-relevant special-purpose
/// block, including the metadata-service link-local /16
/// (`169.254.0.0/16`) and the carrier-grade NAT block
/// (`100.64.0.0/10`).
fn is_disallowed_ipv4(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    addr.is_loopback()
        || addr.is_private()
        || addr.is_unspecified()
        || addr.is_multicast()
        || addr.is_broadcast()
        || addr.is_documentation()
        // 0.0.0.0/8 — "this network" (RFC 1122)
        || o[0] == 0
        // 169.254.0.0/16 — link-local AND cloud metadata service
        || (o[0] == 169 && o[1] == 254)
        // 100.64.0.0/10 — carrier-grade NAT (RFC 6598)
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
}

/// True when `addr` is an IPv6 address Chat Engine must never dial.
/// Covers loopback (`::1`), unspecified (`::`), link-local
/// (`fe80::/10`), unique-local (`fc00::/7`), multicast (`ff00::/8`),
/// and embedded IPv4-mapped forms (`::ffff:0:0/96`) that aim a v6 URL at
/// a forbidden v4 range.
fn is_disallowed_ipv6(addr: Ipv6Addr) -> bool {
    if addr.is_loopback() || addr.is_unspecified() || addr.is_multicast() {
        return true;
    }
    let segs = addr.segments();
    // Unique local addresses fc00::/7
    if (segs[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local fe80::/10
    if (segs[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // IPv4-mapped (`::ffff:0:0/96`) and IPv4-compatible — collapse onto
    // the IPv4 check so callers cannot smuggle a forbidden v4 host
    // through a v6 URL.
    if let Some(v4) = addr.to_ipv4_mapped() {
        return is_disallowed_ipv4(v4);
    }
    if let Some(v4) = addr.to_ipv4() {
        return is_disallowed_ipv4(v4);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_https_domain() {
        let url = validate_outbound_url("https://api.example.com/v1", "endpoint").expect("ok");
        assert_eq!(url.host_str(), Some("api.example.com"));
    }

    #[test]
    fn rejects_empty_string() {
        let err = validate_outbound_url("   ", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
        assert!(err.to_string().contains("endpoint"));
    }

    #[test]
    fn rejects_bare_path() {
        // Non-absolute — would otherwise be sent as a relative POST against
        // the reqwest client's base URL (here: nothing → panic at send
        // time). Catching at parse-time gives a typed config error instead.
        let err = validate_outbound_url("/internal/api", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_http_scheme() {
        let err = validate_outbound_url("http://api.example.com/v1", "endpoint").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("https"), "must surface scheme requirement: {msg}");
    }

    #[test]
    fn rejects_data_scheme() {
        let err = validate_outbound_url("data:application/json,{}", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_localhost_hostname() {
        let err = validate_outbound_url("https://localhost/path", "endpoint").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("localhost"),
            "must call out localhost: {msg}",
        );
    }

    #[test]
    fn rejects_localhost_subdomain() {
        // `*.localhost` resolves to the loopback per RFC 6761; the parser
        // accepts it as a domain so we must catch it explicitly.
        let err = validate_outbound_url("https://api.localhost/v1", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_loopback_ipv4() {
        let err = validate_outbound_url("https://127.0.0.1/x", "endpoint").unwrap_err();
        assert!(err.to_string().contains("IPv4"));
    }

    #[test]
    fn rejects_cloud_metadata_ip() {
        // 169.254.169.254 — AWS / GCP / Azure IMDS endpoint. Must NEVER
        // be reachable through a tenant-configured URL.
        let err =
            validate_outbound_url("https://169.254.169.254/latest/meta-data/", "endpoint")
                .unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_private_rfc1918_ipv4_ranges() {
        for raw in [
            "https://10.0.0.1/x",
            "https://10.255.255.255/x",
            "https://172.16.0.1/x",
            "https://172.31.255.254/x",
            "https://192.168.1.1/x",
        ] {
            let err = validate_outbound_url(raw, "endpoint")
                .unwrap_err_or_else_helper(raw);
            assert!(matches!(err, PluginError::InvalidInput { .. }), "{raw}");
        }
    }

    #[test]
    fn rejects_zero_network_ipv4() {
        let err = validate_outbound_url("https://0.0.0.0/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_multicast_ipv4() {
        let err = validate_outbound_url("https://224.0.0.1/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_cgnat_ipv4() {
        let err = validate_outbound_url("https://100.64.0.1/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_loopback_ipv6() {
        let err = validate_outbound_url("https://[::1]/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_unique_local_ipv6() {
        let err = validate_outbound_url("https://[fd12:3456:789a::1]/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_link_local_ipv6() {
        let err = validate_outbound_url("https://[fe80::1]/x", "endpoint").unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn rejects_ipv4_mapped_loopback_in_ipv6() {
        // `::ffff:127.0.0.1` is an IPv4-mapped form of loopback; without
        // explicit handling this would slip past `IpAddr::is_loopback()`
        // because the IPv6 representation is NOT `::1`.
        let err = validate_outbound_url("https://[::ffff:127.0.0.1]/x", "endpoint")
            .unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn accepts_global_public_ipv4() {
        // 8.8.8.8 — a clearly public IP. We don't want to over-block.
        validate_outbound_url("https://8.8.8.8/v1", "endpoint").expect("public IPs are allowed");
    }

    #[test]
    fn key_name_appears_in_error_messages() {
        let err = validate_outbound_url("http://localhost/x", "gateway_url").unwrap_err();
        assert!(err.to_string().contains("gateway_url"));
    }

    // --- internal test helpers ------------------------------------------

    trait UnwrapErrOr<T, E> {
        fn unwrap_err_or_else_helper(self, ctx: &str) -> E;
    }
    impl<T: std::fmt::Debug, E> UnwrapErrOr<T, E> for Result<T, E> {
        fn unwrap_err_or_else_helper(self, ctx: &str) -> E {
            match self {
                Ok(v) => panic!("expected Err for {ctx}, got Ok({v:?})"),
                Err(e) => e,
            }
        }
    }
}
