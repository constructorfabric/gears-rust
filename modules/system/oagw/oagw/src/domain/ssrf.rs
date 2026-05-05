use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns `true` if the IP address is in a private, loopback, link-local,
/// or otherwise non-globally-routable range that should be blocked by SSRF
/// protections.
///
/// Blocked IPv4 ranges:
/// - `127.0.0.0/8`       — loopback
/// - `10.0.0.0/8`        — RFC 1918 private
/// - `172.16.0.0/12`     — RFC 1918 private
/// - `192.168.0.0/16`    — RFC 1918 private
/// - `169.254.0.0/16`    — link-local (includes AWS metadata `169.254.169.254`)
/// - `0.0.0.0/8`         — "this" network
/// - `100.64.0.0/10`     — shared address space (RFC 6598, carrier-grade NAT)
/// - `192.0.0.0/24`      — IETF protocol assignments (except 192.0.0.9, 192.0.0.10)
/// - `192.0.2.0/24`      — TEST-NET-1 (documentation)
/// - `192.88.99.0/24`    — 6to4 relay anycast (deprecated, RFC 7526)
/// - `198.18.0.0/15`     — benchmarking (RFC 2544)
/// - `198.51.100.0/24`   — TEST-NET-2 (documentation)
/// - `203.0.113.0/24`    — TEST-NET-3 (documentation)
/// - `224.0.0.0/4`       — multicast
/// - `240.0.0.0/4`       — reserved for future use
/// - `255.255.255.255`   — broadcast
///
/// Blocked IPv6 ranges:
/// - `::1`                 — loopback
/// - `::`                  — unspecified
/// - `fe80::/10`           — link-local
/// - `fc00::/7`            — unique local address (ULA)
/// - `::ffff:0:0/96`       — IPv4-mapped (re-checked against IPv4 rules)
/// - `::a.b.c.d/96`        — IPv4-compatible (deprecated, RFC 4291 §2.5.5.1)
/// - `ff00::/8`            — multicast
/// - `100::/64`            — discard prefix (RFC 6666)
/// - `2001:db8::/32`       — documentation
/// - `2001::/32`           — Teredo (NAT traversal; embeds server/client IPs)
/// - `2002::/16`           — 6to4 (embeds IPv4; re-checked against IPv4 rules)
/// - `0:0:0:0:ffff:0::/96` — IPv4-translated (RFC 6052 §2.1; re-checked)
/// - `64:ff9b::/96`        — NAT64 well-known prefix (re-checked against IPv4)
#[must_use]
pub fn is_ssrf_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_ssrf_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_ssrf_blocked_ipv6(v6),
    }
}

/// Hostnames blocked at config time to fail fast with a clear SSRF error.
/// Other cloud metadata services use IPs (e.g. 169.254.169.254) and are
/// caught by the IP deny-list.
const BLOCKED_HOSTNAMES: &[&str] = &[
    "localhost",
    "localhost.localdomain",
    "metadata.google.internal",
];

/// Returns `Some(reason)` if the hostname is in the static SSRF deny-list.
#[must_use]
pub fn is_ssrf_blocked_hostname(host: &str) -> Option<&'static str> {
    let normalized = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    BLOCKED_HOSTNAMES.iter().copied().find(|&b| normalized == b)
}

fn is_ssrf_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();

    // 127.0.0.0/8 — loopback
    if o[0] == 127 {
        return true;
    }

    // 10.0.0.0/8 — RFC 1918 private
    if o[0] == 10 {
        return true;
    }

    // 172.16.0.0/12 — RFC 1918 private
    // The /12 mask covers 172.16.0.0–172.31.255.255.
    // o[1] & 0xF0 isolates the top nibble; it equals 0x10 (16) for that range.
    if o[0] == 172 && (o[1] & 0xF0) == 0x10 {
        return true;
    }

    // 192.168.0.0/16 — RFC 1918 private
    if o[0] == 192 && o[1] == 168 {
        return true;
    }

    // 169.254.0.0/16 — link-local (includes cloud metadata endpoints)
    if o[0] == 169 && o[1] == 254 {
        return true;
    }

    // 0.0.0.0/8 — "this" network
    if o[0] == 0 {
        return true;
    }

    // 100.64.0.0/10 — shared address space (RFC 6598, carrier-grade NAT)
    // The /10 mask covers 100.64.0.0–100.127.255.255.
    // o[1] & 0xC0 isolates the top two bits; they equal 0x40 (64) for that range.
    if o[0] == 100 && (o[1] & 0xC0) == 0x40 {
        return true;
    }

    // 192.0.0.0/24 — IETF protocol assignments.
    // Exception: 192.0.0.9 and 192.0.0.10 are IANA-designated globally
    // reachable anycast addresses and must not be blocked.
    if o[0] == 192 && o[1] == 0 && o[2] == 0 && o[3] != 9 && o[3] != 10 {
        return true;
    }

    // 192.0.2.0/24 — TEST-NET-1 (documentation)
    if o[0] == 192 && o[1] == 0 && o[2] == 2 {
        return true;
    }

    // 192.88.99.0/24 — 6to4 relay anycast (deprecated by RFC 7526 but still
    // seen in the wild; treat as non-routable).
    if o[0] == 192 && o[1] == 88 && o[2] == 99 {
        return true;
    }

    // 198.18.0.0/15 — benchmarking (RFC 2544)
    // Covers 198.18.0.0–198.19.255.255; o[1] & 0xFE strips the low bit.
    if o[0] == 198 && (o[1] & 0xFE) == 18 {
        return true;
    }

    // 198.51.100.0/24 — TEST-NET-2 (documentation)
    if o[0] == 198 && o[1] == 51 && o[2] == 100 {
        return true;
    }

    // 203.0.113.0/24 — TEST-NET-3 (documentation)
    if o[0] == 203 && o[1] == 0 && o[2] == 113 {
        return true;
    }

    // 224.0.0.0/4 — multicast
    if (o[0] & 0xF0) == 0xE0 {
        return true;
    }

    // 240.0.0.0/4 — reserved for future use; also covers 255.255.255.255.
    if o[0] >= 240 {
        return true;
    }

    false
}

fn is_ssrf_blocked_ipv6(ip: Ipv6Addr) -> bool {
    // ::1 — loopback
    if ip == Ipv6Addr::LOCALHOST {
        return true;
    }

    // :: — unspecified
    if ip == Ipv6Addr::UNSPECIFIED {
        return true;
    }

    let s = ip.segments();

    // fe80::/10 — link-local
    if (s[0] & 0xFFC0) == 0xFE80 {
        return true;
    }

    // fc00::/7 — unique local address (ULA)
    if (s[0] & 0xFE00) == 0xFC00 {
        return true;
    }

    // ff00::/8 — multicast
    if (s[0] & 0xFF00) == 0xFF00 {
        return true;
    }

    // 100::/64 — discard prefix (RFC 6666)
    if s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0 {
        return true;
    }

    // 2001:db8::/32 — documentation
    if s[0] == 0x2001 && s[1] == 0x0DB8 {
        return true;
    }

    // 2001::/32 — Teredo (RFC 4380).
    // The Teredo header embeds both the server IPv4 (segments 2–3) and the
    // obfuscated client IPv4 (segments 6–7, stored XOR'd with 0xFFFF_FFFF).
    // Block the entire prefix unconditionally: Teredo bypasses normal routing
    // and the embedded addresses could be private even when the outer address
    // looks public.
    if s[0] == 0x2001 && s[1] == 0x0000 {
        return true;
    }

    // 2002::/16 — 6to4 (RFC 3056).
    // Bits 16–47 directly encode an IPv4 address; e.g. 2002:7f00:0001::/48
    // embeds 127.0.0.1. Re-check the embedded IPv4 against our blocklist.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return is_ssrf_blocked_ipv4(v4);
    }

    // ::ffff:0:0/96 — IPv4-mapped (e.g. ::ffff:192.168.1.1).
    // `to_ipv4_mapped` returns `Some` only for this prefix, not for
    // IPv4-compatible addresses, so the two cases are distinguished below.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_ssrf_blocked_ipv4(v4);
    }

    // ::a.b.c.d / 0:0:0:0:0:0:a.b.c.d — IPv4-compatible (deprecated, RFC 4291
    // §2.5.5.1). `to_ipv4` returns Some for both IPv4-compatible AND
    // IPv4-mapped addresses, but we've already handled the mapped case above,
    // so a non-None result here means IPv4-compatible.
    if let Some(v4) = ip.to_ipv4() {
        return is_ssrf_blocked_ipv4(v4);
    }

    // 0:0:0:0:ffff:0:a.b.c.d / 0:0:0:0:ffff:0::/96 — IPv4-translated
    // (RFC 6052 §2.1). Layout: s[0..3] = 0, s[4] = 0xFFFF, s[5] = 0,
    // s[6..7] = IPv4.
    //
    // NOTE: This is distinct from IPv4-mapped (::ffff:a.b.c.d) where
    // s[4] = 0 and s[5] = 0xFFFF. The indices here are intentionally swapped
    // relative to the mapped prefix.
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0xFFFF && s[5] == 0 {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return is_ssrf_blocked_ipv4(v4);
    }

    // 64:ff9b::/96 — NAT64 well-known prefix (RFC 6052 §2.2).
    // The last 32 bits carry the embedded IPv4 address.
    if s[0] == 0x0064 && s[1] == 0xFF9B && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return is_ssrf_blocked_ipv4(v4);
    }

    false
}

/// Human-readable reason string for a blocked IP (for error messages / logs).
///
/// Returns a static string describing *why* the address is blocked, or
/// `"non-routable address"` / `"non-routable IPv6 address"` as a catch-all for
/// ranges covered by `is_ssrf_blocked_ip` but not individually enumerated here.
/// Returns `"allowed"` if the address is not blocked (callers should check
/// `is_ssrf_blocked_ip` first to avoid that case).
#[must_use]
pub fn ssrf_block_reason(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(v4) => ssrf_block_reason_v4(v4),
        IpAddr::V6(v6) => ssrf_block_reason_v6(v6),
    }
}

fn ssrf_block_reason_v4(ip: Ipv4Addr) -> &'static str {
    let o = ip.octets();
    if o[0] == 127 {
        return "loopback address (127.0.0.0/8)";
    }
    if o[0] == 10 {
        return "private address (10.0.0.0/8)";
    }
    if o[0] == 172 && (o[1] & 0xF0) == 0x10 {
        return "private address (172.16.0.0/12)";
    }
    if o[0] == 192 && o[1] == 168 {
        return "private address (192.168.0.0/16)";
    }
    if o[0] == 169 && o[1] == 254 {
        return "link-local address (169.254.0.0/16)";
    }
    if o[0] == 0 {
        return "\"this\" network (0.0.0.0/8)";
    }
    if o[0] == 100 && (o[1] & 0xC0) == 0x40 {
        return "shared address space (100.64.0.0/10)";
    }
    if o[0] == 192 && o[1] == 0 && o[2] == 0 && o[3] != 9 && o[3] != 10 {
        return "IETF protocol assignments (192.0.0.0/24)";
    }
    if o[0] == 192 && o[1] == 0 && o[2] == 2 {
        return "documentation address TEST-NET-1 (192.0.2.0/24)";
    }
    if o[0] == 192 && o[1] == 88 && o[2] == 99 {
        return "6to4 relay anycast (192.88.99.0/24)";
    }
    if o[0] == 198 && (o[1] & 0xFE) == 18 {
        return "benchmarking address (198.18.0.0/15)";
    }
    if o[0] == 198 && o[1] == 51 && o[2] == 100 {
        return "documentation address TEST-NET-2 (198.51.100.0/24)";
    }
    if o[0] == 203 && o[1] == 0 && o[2] == 113 {
        return "documentation address TEST-NET-3 (203.0.113.0/24)";
    }
    if (o[0] & 0xF0) == 0xE0 {
        return "multicast address (224.0.0.0/4)";
    }
    if o[0] >= 240 {
        return "reserved/broadcast address (240.0.0.0/4)";
    }
    "allowed"
}

fn ssrf_block_reason_v6(ip: Ipv6Addr) -> &'static str {
    if ip == Ipv6Addr::LOCALHOST {
        return "loopback address (::1)";
    }
    if ip == Ipv6Addr::UNSPECIFIED {
        return "unspecified address (::)";
    }
    let s = ip.segments();
    if (s[0] & 0xFFC0) == 0xFE80 {
        return "link-local address (fe80::/10)";
    }
    if (s[0] & 0xFE00) == 0xFC00 {
        return "unique local address (fc00::/7)";
    }
    if (s[0] & 0xFF00) == 0xFF00 {
        return "multicast address (ff00::/8)";
    }
    if s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0 {
        return "discard prefix (100::/64)";
    }
    if s[0] == 0x2001 && s[1] == 0x0DB8 {
        return "documentation address (2001:db8::/32)";
    }
    if s[0] == 0x2001 && s[1] == 0x0000 {
        return "Teredo tunneling address (2001::/32)";
    }
    // 2002::/16 — 6to4: only blocked if the embedded IPv4 is blocked.
    // Extract and re-check the embedded IPv4 (bits 16–47) to stay consistent
    // with is_ssrf_blocked_ipv6, which does the same check.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        if is_ssrf_blocked_ipv4(v4) {
            return "6to4 address with blocked embedded IPv4 (2002::/16)";
        }
        return "allowed";
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        if is_ssrf_blocked_ipv4(v4) {
            return "IPv4-mapped IPv6 address with blocked embedded IPv4 (::ffff:0:0/96)";
        }
        return "allowed";
    }
    if let Some(v4) = ip.to_ipv4() {
        if is_ssrf_blocked_ipv4(v4) {
            return "IPv4-compatible IPv6 address with blocked embedded IPv4";
        }
        return "allowed";
    }
    if s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0xFFFF && s[5] == 0 {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        if is_ssrf_blocked_ipv4(v4) {
            return "IPv4-translated address with blocked embedded IPv4 (0:0:0:0:ffff:0::/96)";
        }
        return "allowed";
    }
    if s[0] == 0x0064 && s[1] == 0xFF9B && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        if is_ssrf_blocked_ipv4(v4) {
            return "NAT64 address with blocked embedded IPv4 (64:ff9b::/96)";
        }
        return "allowed";
    }
    "allowed"
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- IPv4 blocked ranges --

    #[test]
    fn loopback_127_0_0_1_blocked() {
        assert!(is_ssrf_blocked_ip("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn loopback_127_255_255_255_blocked() {
        assert!(is_ssrf_blocked_ip("127.255.255.255".parse().unwrap()));
    }

    #[test]
    fn private_10_x_blocked() {
        assert!(is_ssrf_blocked_ip("10.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("10.255.255.255".parse().unwrap()));
    }

    #[test]
    fn private_172_16_blocked() {
        assert!(is_ssrf_blocked_ip("172.16.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("172.31.255.255".parse().unwrap()));
    }

    #[test]
    fn private_172_32_not_blocked() {
        assert!(!is_ssrf_blocked_ip("172.32.0.1".parse().unwrap()));
    }

    #[test]
    fn private_192_168_blocked() {
        assert!(is_ssrf_blocked_ip("192.168.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("192.168.255.255".parse().unwrap()));
    }

    #[test]
    fn link_local_169_254_blocked() {
        assert!(is_ssrf_blocked_ip("169.254.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("169.254.169.254".parse().unwrap())); // AWS metadata
    }

    #[test]
    fn this_network_0_x_blocked() {
        assert!(is_ssrf_blocked_ip("0.0.0.0".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("0.255.255.255".parse().unwrap()));
    }

    #[test]
    fn shared_address_space_blocked() {
        assert!(is_ssrf_blocked_ip("100.64.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("100.127.255.255".parse().unwrap()));
    }

    #[test]
    fn shared_address_space_boundary_not_blocked() {
        assert!(!is_ssrf_blocked_ip("100.128.0.0".parse().unwrap()));
    }

    #[test]
    fn multicast_blocked() {
        assert!(is_ssrf_blocked_ip("224.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("239.255.255.255".parse().unwrap()));
    }

    #[test]
    fn reserved_240_plus_blocked() {
        assert!(is_ssrf_blocked_ip("240.0.0.0".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn test_nets_blocked() {
        assert!(is_ssrf_blocked_ip("192.0.2.1".parse().unwrap())); // TEST-NET-1
        assert!(is_ssrf_blocked_ip("198.51.100.1".parse().unwrap())); // TEST-NET-2
        assert!(is_ssrf_blocked_ip("203.0.113.1".parse().unwrap())); // TEST-NET-3
    }

    #[test]
    fn benchmarking_blocked() {
        assert!(is_ssrf_blocked_ip("198.18.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("198.19.255.255".parse().unwrap()));
    }

    #[test]
    fn ietf_protocol_assignments_blocked() {
        assert!(is_ssrf_blocked_ip("192.0.0.1".parse().unwrap()));
    }

    #[test]
    fn ietf_protocol_assignments_globally_reachable_allowed() {
        // 192.0.0.9 and 192.0.0.10 are IANA-designated globally reachable anycast.
        assert!(!is_ssrf_blocked_ip("192.0.0.9".parse().unwrap()));
        assert!(!is_ssrf_blocked_ip("192.0.0.10".parse().unwrap()));
    }

    #[test]
    fn relay_6to4_anycast_blocked() {
        assert!(is_ssrf_blocked_ip("192.88.99.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("192.88.99.255".parse().unwrap()));
    }

    // -- IPv4 allowed --

    #[test]
    fn public_ipv4_allowed() {
        assert!(!is_ssrf_blocked_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_ssrf_blocked_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_ssrf_blocked_ip("93.184.216.34".parse().unwrap()));
    }

    // -- IPv6 blocked ranges --

    #[test]
    fn ipv6_loopback_blocked() {
        assert!(is_ssrf_blocked_ip("::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_unspecified_blocked() {
        assert!(is_ssrf_blocked_ip("::".parse().unwrap()));
    }

    #[test]
    fn ipv6_link_local_blocked() {
        assert!(is_ssrf_blocked_ip("fe80::1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("febf::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_ula_blocked() {
        assert!(is_ssrf_blocked_ip("fc00::1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("fd00::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_multicast_blocked() {
        assert!(is_ssrf_blocked_ip("ff02::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_documentation_blocked() {
        assert!(is_ssrf_blocked_ip("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_teredo_blocked() {
        // 2001::/32 — the entire Teredo range is blocked regardless of
        // the embedded IPv4 addresses.
        assert!(is_ssrf_blocked_ip("2001:0000::1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip(
            "2001:0000:4136:e378:8000:63bf:3fff:fdd2".parse().unwrap()
        ));
    }

    #[test]
    fn ipv6_6to4_private_embedded_blocked() {
        // 2002:7f00:0001:: embeds 127.0.0.1
        assert!(is_ssrf_blocked_ip("2002:7f00:1::".parse().unwrap()));
        // 2002:c0a8:0101:: embeds 192.168.1.1
        assert!(is_ssrf_blocked_ip("2002:c0a8:101::".parse().unwrap()));
        // 2002:a9fe:a9fe:: embeds 169.254.169.254 (AWS metadata)
        assert!(is_ssrf_blocked_ip("2002:a9fe:a9fe::".parse().unwrap()));
    }

    #[test]
    fn ipv6_6to4_public_embedded_allowed() {
        // 2002:0808:0808:: embeds 8.8.8.8 (Google DNS)
        assert!(!is_ssrf_blocked_ip("2002:808:808::".parse().unwrap()));
    }

    #[test]
    fn ipv6_mapped_ipv4_private_blocked() {
        assert!(is_ssrf_blocked_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked_ip("::ffff:192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn ipv6_mapped_ipv4_public_allowed() {
        assert!(!is_ssrf_blocked_ip("::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn ipv6_compatible_ipv4_private_blocked() {
        // ::127.0.0.1 — IPv4-compatible loopback
        assert!(is_ssrf_blocked_ip("::127.0.0.1".parse().unwrap()));
        // ::10.0.0.1 — IPv4-compatible private
        assert!(is_ssrf_blocked_ip("::10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn ipv6_compatible_ipv4_public_allowed() {
        // ::8.8.8.8 — deprecated but should pass if public
        assert!(!is_ssrf_blocked_ip("::8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn ipv6_discard_prefix_blocked() {
        assert!(is_ssrf_blocked_ip("100::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_translated_private_blocked() {
        // 0:0:0:0:ffff:0:a.b.c.d with private IPv4
        let ip: IpAddr = "::ffff:0:7f00:1".parse().unwrap();
        assert!(is_ssrf_blocked_ip(ip));
    }

    #[test]
    fn ipv6_nat64_private_blocked() {
        // 64:ff9b::169.254.169.254
        assert!(is_ssrf_blocked_ip(
            "64:ff9b::169.254.169.254".parse().unwrap()
        ));
    }

    #[test]
    fn ipv6_nat64_public_allowed() {
        // 64:ff9b::8.8.8.8
        assert!(!is_ssrf_blocked_ip("64:ff9b::8.8.8.8".parse().unwrap()));
    }

    // -- IPv6 allowed --

    #[test]
    fn public_ipv6_allowed() {
        assert!(!is_ssrf_blocked_ip(
            "2607:f8b0:4004:800::200e".parse().unwrap()
        )); // Google
    }

    // -- ssrf_block_reason coverage --

    #[test]
    fn reason_loopback_v4() {
        assert!(ssrf_block_reason("127.0.0.1".parse().unwrap()).contains("loopback"));
    }

    #[test]
    fn reason_private_10() {
        assert!(ssrf_block_reason("10.1.2.3".parse().unwrap()).contains("private"));
    }

    #[test]
    fn reason_private_172() {
        assert!(ssrf_block_reason("172.16.0.1".parse().unwrap()).contains("private"));
    }

    #[test]
    fn reason_private_192_168() {
        assert!(ssrf_block_reason("192.168.1.1".parse().unwrap()).contains("private"));
    }

    #[test]
    fn reason_link_local_v4() {
        assert!(ssrf_block_reason("169.254.169.254".parse().unwrap()).contains("link-local"));
    }

    #[test]
    fn reason_shared_space() {
        assert!(ssrf_block_reason("100.64.0.1".parse().unwrap()).contains("shared"));
    }

    #[test]
    fn reason_ietf_assignments() {
        assert!(ssrf_block_reason("192.0.0.1".parse().unwrap()).contains("IETF"));
    }

    #[test]
    fn reason_ietf_globally_reachable_allowed() {
        assert_eq!(ssrf_block_reason("192.0.0.9".parse().unwrap()), "allowed");
        assert_eq!(ssrf_block_reason("192.0.0.10".parse().unwrap()), "allowed");
    }

    #[test]
    fn reason_test_net_1() {
        assert!(ssrf_block_reason("192.0.2.1".parse().unwrap()).contains("TEST-NET-1"));
    }

    #[test]
    fn reason_6to4_relay() {
        assert!(ssrf_block_reason("192.88.99.1".parse().unwrap()).contains("6to4"));
    }

    #[test]
    fn reason_benchmarking() {
        assert!(ssrf_block_reason("198.18.0.1".parse().unwrap()).contains("benchmarking"));
    }

    #[test]
    fn reason_test_net_2() {
        assert!(ssrf_block_reason("198.51.100.1".parse().unwrap()).contains("TEST-NET-2"));
    }

    #[test]
    fn reason_test_net_3() {
        assert!(ssrf_block_reason("203.0.113.1".parse().unwrap()).contains("TEST-NET-3"));
    }

    #[test]
    fn reason_multicast_v4() {
        assert!(ssrf_block_reason("224.0.0.1".parse().unwrap()).contains("multicast"));
    }

    #[test]
    fn reason_reserved() {
        assert!(ssrf_block_reason("240.0.0.1".parse().unwrap()).contains("reserved"));
    }

    #[test]
    fn reason_loopback_v6() {
        assert!(ssrf_block_reason("::1".parse().unwrap()).contains("loopback"));
    }

    #[test]
    fn reason_unspecified_v6() {
        assert!(ssrf_block_reason("::".parse().unwrap()).contains("unspecified"));
    }

    #[test]
    fn reason_link_local_v6() {
        assert!(ssrf_block_reason("fe80::1".parse().unwrap()).contains("link-local"));
    }

    #[test]
    fn reason_ula_v6() {
        assert!(ssrf_block_reason("fc00::1".parse().unwrap()).contains("unique local"));
    }

    #[test]
    fn reason_multicast_v6() {
        assert!(ssrf_block_reason("ff02::1".parse().unwrap()).contains("multicast"));
    }

    #[test]
    fn reason_discard_prefix() {
        assert!(ssrf_block_reason("100::1".parse().unwrap()).contains("discard"));
    }

    #[test]
    fn reason_documentation_v6() {
        assert!(ssrf_block_reason("2001:db8::1".parse().unwrap()).contains("documentation"));
    }

    #[test]
    fn reason_teredo() {
        assert!(ssrf_block_reason("2001:0000::1".parse().unwrap()).contains("Teredo"));
    }

    #[test]
    fn reason_6to4_v6_private_blocked() {
        assert!(ssrf_block_reason("2002:7f00:1::".parse().unwrap()).contains("6to4"));
    }

    #[test]
    fn reason_6to4_v6_public_allowed() {
        // 2002:0808:0808:: embeds 8.8.8.8 — should not be reported as blocked
        assert_eq!(
            ssrf_block_reason("2002:808:808::".parse().unwrap()),
            "allowed"
        );
    }

    #[test]
    fn reason_mapped_v4_private() {
        assert!(ssrf_block_reason("::ffff:10.0.0.1".parse().unwrap()).contains("IPv4-mapped"));
    }

    #[test]
    fn reason_mapped_v4_public() {
        assert_eq!(
            ssrf_block_reason("::ffff:8.8.8.8".parse().unwrap()),
            "allowed"
        );
    }

    #[test]
    fn reason_compatible_v4_private() {
        assert!(ssrf_block_reason("::10.0.0.1".parse().unwrap()).contains("IPv4-compatible"));
    }

    #[test]
    fn reason_compatible_v4_public() {
        assert_eq!(ssrf_block_reason("::8.8.8.8".parse().unwrap()), "allowed");
    }

    #[test]
    fn reason_nat64_private() {
        assert!(ssrf_block_reason("64:ff9b::10.0.0.1".parse().unwrap()).contains("NAT64"));
    }

    #[test]
    fn reason_nat64_public() {
        assert_eq!(
            ssrf_block_reason("64:ff9b::8.8.8.8".parse().unwrap()),
            "allowed"
        );
    }

    #[test]
    fn reason_allowed_returns_allowed() {
        assert_eq!(ssrf_block_reason("8.8.8.8".parse().unwrap()), "allowed");
        assert_eq!(
            ssrf_block_reason("2607:f8b0:4004:800::200e".parse().unwrap()),
            "allowed"
        );
    }

    // -- is_ssrf_blocked_hostname tests --

    #[test]
    fn blocked_hostname_localhost() {
        assert_eq!(is_ssrf_blocked_hostname("localhost"), Some("localhost"));
    }

    #[test]
    fn blocked_hostname_localhost_localdomain() {
        assert_eq!(
            is_ssrf_blocked_hostname("localhost.localdomain"),
            Some("localhost.localdomain"),
        );
    }

    #[test]
    fn blocked_hostname_metadata_google() {
        assert_eq!(
            is_ssrf_blocked_hostname("metadata.google.internal"),
            Some("metadata.google.internal"),
        );
    }

    #[test]
    fn blocked_hostname_case_insensitive() {
        assert_eq!(is_ssrf_blocked_hostname("LocalHost"), Some("localhost"));
        assert_eq!(
            is_ssrf_blocked_hostname("METADATA.GOOGLE.INTERNAL"),
            Some("metadata.google.internal"),
        );
    }

    #[test]
    fn blocked_hostname_trailing_dot() {
        assert_eq!(is_ssrf_blocked_hostname("localhost."), Some("localhost"));
        assert_eq!(
            is_ssrf_blocked_hostname("metadata.google.internal."),
            Some("metadata.google.internal"),
        );
    }

    #[test]
    fn allowed_hostname_public() {
        assert_eq!(is_ssrf_blocked_hostname("api.openai.com"), None);
        assert_eq!(is_ssrf_blocked_hostname("example.com"), None);
    }

    #[test]
    fn allowed_hostname_partial_match() {
        assert_eq!(is_ssrf_blocked_hostname("notlocalhost"), None);
        assert_eq!(is_ssrf_blocked_hostname("localhost.example.com"), None);
    }
}
