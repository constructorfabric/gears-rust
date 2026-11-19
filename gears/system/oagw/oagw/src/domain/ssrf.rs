use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;

// ---------------------------------------------------------------------------
// SSRF Policy (config)
// ---------------------------------------------------------------------------

/// SSRF protection policy.
///
/// When enabled (the default), the built-in deny-list blocks private,
/// loopback, link-local, and other non-routable IP ranges.
///
/// Evaluation order: **allow first** — if an IP/hostname matches an allow
/// list it is permitted regardless of deny lists.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SsrfPolicy {
    /// Master switch. When `false`, all SSRF checks are skipped. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Additional CIDR ranges to deny (e.g. `["10.96.0.0/12"]` for k8s service CIDRs).
    /// An IPv4 CIDR matches IPv4 addresses and the IPv6 addresses that carry
    /// one (IPv4-mapped, IPv4-compatible, IPv4-translated, NAT64
    /// `64:ff9b::/96`, 6to4), judged by that IPv4 address. An IPv6 CIDR
    /// matches IPv6 addresses other than IPv4-mapped ones, so `::/0` covers no
    /// IPv4 address. A CIDR inside `::ffff:0:0/96` (prefix 96 or longer) is
    /// its IPv4 CIDR.
    #[serde(default)]
    pub extra_deny_cidrs: Vec<String>,
    /// CIDR ranges to exempt from all deny lists (e.g. `["10.0.5.20/32"]`).
    /// Stricter than `extra_deny_cidrs`: an IPv4 CIDR exempts only the IPv4
    /// address and its IPv4-mapped form, never a NAT64, 6to4, compatible or
    /// translated address; list those as IPv6 CIDRs. `::/0` exempts no IPv4
    /// address.
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    /// Additional hostnames to deny. Entries starting with `.` are suffix
    /// matches (e.g. `".cluster.local"`). Plain entries require exact match.
    #[serde(default)]
    pub extra_deny_hostnames: Vec<String>,
    /// Hostnames to exempt from all deny lists. Same matching rules.
    #[serde(default)]
    pub allow_hostnames: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for SsrfPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            extra_deny_cidrs: Vec::new(),
            allow_cidrs: Vec::new(),
            extra_deny_hostnames: Vec::new(),
            allow_hostnames: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parsed CIDR prefix
// ---------------------------------------------------------------------------

/// Parsed CIDR prefix (e.g. `10.96.0.0/12`). An IPv4 prefix matches only
/// IPv4 addresses and an IPv6 prefix only IPv6 ones, so `::/3` does not cover
/// every IPv4 address through its `::ffff:a.b.c.d` form. An IPv4-mapped
/// address is checked as the IPv4 address it carries, and an IPv6 prefix
/// inside `::ffff:0:0/96` is stored as its IPv4 prefix.
///
/// Private to this module so that [`Cidr::parse`] stays the only constructor:
/// the matching relies on cleared host bits, a contiguous mask and mapped
/// prefixes being stored as IPv4.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cidr {
    V4 { network: u32, mask: u32 },
    V6 { network: u128, mask: u128 },
}

/// `::ffff:0:0`, the start of the IPv4-mapped block.
const IPV4_MAPPED_NETWORK: u128 = 0xFFFF_0000_0000;

impl Cidr {
    /// Parse a CIDR string (e.g. `"10.96.0.0/12"` or `"fd00::/8"`).
    fn parse(s: &str) -> Result<Self, String> {
        let (addr_str, prefix_str) = s
            .split_once('/')
            .ok_or_else(|| format!("invalid CIDR '{s}': missing '/' separator"))?;

        let addr: IpAddr = addr_str
            .trim()
            .parse()
            .map_err(|e| format!("invalid CIDR '{s}': bad address: {e}"))?;

        let raw_prefix: u8 = prefix_str
            .trim()
            .parse()
            .map_err(|e| format!("invalid CIDR '{s}': bad prefix length: {e}"))?;

        match addr {
            IpAddr::V4(v4) => {
                if raw_prefix > 32 {
                    return Err(format!(
                        "invalid CIDR '{s}': prefix length {raw_prefix} exceeds 32 for IPv4"
                    ));
                }
                Ok(Self::v4(u32::from(v4), raw_prefix))
            }
            IpAddr::V6(v6) => {
                if raw_prefix > 128 {
                    return Err(format!(
                        "invalid CIDR '{s}': prefix length {raw_prefix} exceeds 128 for IPv6"
                    ));
                }
                // Checked addresses are canonicalized, so a mapped prefix
                // would otherwise never match anything.
                if raw_prefix >= 96
                    && let Some(v4) = v6.to_ipv4_mapped()
                {
                    return Ok(Self::v4(u32::from(v4), raw_prefix - 96));
                }
                let bits = u128::from(v6);
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(raw_prefix))
                    .unwrap_or(0);
                Ok(Self::V6 {
                    network: bits & mask,
                    mask,
                })
            }
        }
    }

    fn v4(bits: u32, prefix: u8) -> Self {
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        Self::V4 {
            network: bits & mask,
            mask,
        }
    }

    /// Check whether `ip` falls within this prefix. An IPv4-mapped IPv6
    /// address is checked as its IPv4 address; no other IPv6 form matches an
    /// IPv4 prefix.
    fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip.to_canonical()) {
            (Self::V4 { .. }, IpAddr::V4(v4)) => self.contains_v4(v4),
            (Self::V6 { network, mask }, IpAddr::V6(v6)) => u128::from(v6) & mask == *network,
            _ => false,
        }
    }

    /// Like [`Self::contains`], but an IPv4 prefix also matches every IPv6
    /// address that carries an IPv4 address in it (compatible, translated,
    /// NAT64, 6to4), the same extraction the built-in deny list uses.
    fn contains_or_embeds(&self, ip: IpAddr) -> bool {
        self.contains(ip)
            || matches!(ip, IpAddr::V6(v6) if embedded_ipv4(v6).is_some_and(|v4| self.contains_v4(v4)))
    }

    fn contains_v4(&self, ip: Ipv4Addr) -> bool {
        matches!(self, Self::V4 { network, mask } if u32::from(ip) & mask == *network)
    }

    /// Whether this is an IPv6 prefix that covers all of `::ffff:0:0/96`.
    /// Shorter than /96 by construction: a longer one is stored as IPv4.
    fn covers_ipv4_mapped_block(&self) -> bool {
        matches!(self, Self::V6 { network, mask } if IPV4_MAPPED_NETWORK & mask == *network)
    }
}

// ---------------------------------------------------------------------------
// SSRF Guard (runtime)
// ---------------------------------------------------------------------------

/// Pre-compiled SSRF guard for runtime IP and hostname checks.
///
/// Built once from [`SsrfPolicy`] at gear init and shared via `Arc`.
#[domain_model]
pub struct SsrfGuard {
    enabled: bool,
    extra_deny: Vec<Cidr>,
    allow: Vec<Cidr>,
    extra_deny_hostnames: Vec<String>,
    allow_hostnames: Vec<String>,
}

impl std::fmt::Debug for SsrfGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsrfGuard")
            .field("enabled", &self.enabled)
            .field("extra_deny_count", &self.extra_deny.len())
            .field("allow_count", &self.allow.len())
            .field("extra_deny_hostnames", &self.extra_deny_hostnames)
            .field("allow_hostnames", &self.allow_hostnames)
            .finish()
    }
}

impl SsrfGuard {
    /// Build from config, parsing all CIDR strings. Returns `Err` on malformed input.
    pub fn from_config(cfg: &SsrfPolicy) -> Result<Self, String> {
        let extra_deny = parse_cidr_list("extra_deny_cidrs", &cfg.extra_deny_cidrs)?;
        let allow = parse_cidr_list("allow_cidrs", &cfg.allow_cidrs)?;
        let extra_deny_hostnames = cfg
            .extra_deny_hostnames
            .iter()
            .map(|s| s.strip_suffix('.').unwrap_or(s).to_ascii_lowercase())
            .collect();
        let allow_hostnames = cfg
            .allow_hostnames
            .iter()
            .map(|s| s.strip_suffix('.').unwrap_or(s).to_ascii_lowercase())
            .collect();
        Ok(Self {
            enabled: cfg.enabled,
            extra_deny,
            allow,
            extra_deny_hostnames,
            allow_hostnames,
        })
    }

    /// Disabled guard (all checks return "allowed").
    #[must_use]
    #[allow(dead_code)]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            extra_deny: Vec::new(),
            allow: Vec::new(),
            extra_deny_hostnames: Vec::new(),
            allow_hostnames: Vec::new(),
        }
    }

    /// Whether the guard is active.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Check whether an IP is blocked (allow → builtin deny → extra deny).
    #[must_use]
    pub fn is_ip_blocked(&self, ip: IpAddr) -> bool {
        if !self.enabled {
            return false;
        }
        // Allow-list takes precedence.
        if self.is_allowed_ip(ip) {
            return false;
        }
        // Built-in deny-list.
        if is_ssrf_blocked_ip(ip) {
            return true;
        }
        self.is_extra_denied_ip(ip)
    }

    // Unlike deny, allow does not look through NAT64, 6to4, compatible or
    // translated forms: an IPv4 entry exempting a private address must not
    // also exempt the IPv6 forms the built-in list refuses for that address.
    fn is_allowed_ip(&self, ip: IpAddr) -> bool {
        self.allow.iter().any(|cidr| cidr.contains(ip))
    }

    // An IPv4 entry also catches the IPv6 forms that carry the address, which
    // a NAT64 gateway or 6to4 relay delivers to it.
    fn is_extra_denied_ip(&self, ip: IpAddr) -> bool {
        self.extra_deny
            .iter()
            .any(|cidr| cidr.contains_or_embeds(ip))
    }

    /// Check a DNS result for an endpoint. An explicit hostname exemption also
    /// covers that hostname's private resolved addresses (a cluster service's
    /// RFC 1918 or ULA address, say), but never a loopback, unspecified,
    /// link-local, multicast or cloud-metadata one: a rebound or hijacked
    /// exempted name must not reach the metadata endpoint. Only `allow_cidrs`
    /// admits those. Ordinary hostnames undergo the full IP check on every
    /// resolution to prevent DNS rebinding.
    #[must_use]
    pub fn is_resolved_ip_blocked(&self, host: &str, ip: IpAddr) -> bool {
        if !self.enabled {
            return false;
        }
        if self.is_exempt_hostname(host) {
            return !self.is_allowed_ip(ip) && is_never_exempt_ip(ip);
        }
        self.is_hostname_blocked(host).is_some() || self.is_ip_blocked(ip)
    }

    /// Why [`Self::is_resolved_ip_blocked`] refused `ip` for `host`: the
    /// hostname's deny pattern when the name itself is denied, otherwise the
    /// address's reason.
    #[must_use]
    pub fn resolved_block_reason<'a>(&'a self, host: &str, ip: IpAddr) -> &'a str {
        if self.is_exempt_hostname(host) {
            return "loopback, link-local or metadata address: allow_hostnames does not exempt it";
        }
        self.is_hostname_blocked(host)
            .unwrap_or_else(|| self.ip_block_reason(ip))
    }

    fn is_exempt_hostname(&self, host: &str) -> bool {
        let normalized = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        self.allow_hostnames
            .iter()
            .any(|pattern| hostname_matches(&normalized, pattern))
    }

    /// Human-readable block reason, or `"allowed"`.
    #[must_use]
    pub fn ip_block_reason(&self, ip: IpAddr) -> &str {
        if !self.enabled {
            return "allowed";
        }
        if self.is_allowed_ip(ip) {
            return "allowed";
        }
        if is_ssrf_blocked_ip(ip) {
            return ssrf_block_reason(ip);
        }
        if self.is_extra_denied_ip(ip) {
            return "blocked by extra_deny_cidrs";
        }
        "allowed"
    }

    /// Check whether a hostname is blocked (allow → builtin deny → extra deny).
    /// Returns `Some(matched_pattern)` if blocked, `None` if allowed.
    #[must_use]
    pub fn is_hostname_blocked<'a>(&'a self, host: &str) -> Option<&'a str> {
        if !self.enabled {
            return None;
        }
        let normalized = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        // Allow-list takes precedence.
        for pattern in &self.allow_hostnames {
            if hostname_matches(&normalized, pattern) {
                return None;
            }
        }
        // Built-in hostname deny-list.
        if let Some(matched) = is_ssrf_blocked_hostname(host) {
            return Some(matched);
        }
        // Extra deny hostnames.
        for pattern in &self.extra_deny_hostnames {
            if hostname_matches(&normalized, pattern) {
                return Some(pattern.as_str());
            }
        }
        None
    }
}

fn parse_cidr_list(list: &str, entries: &[String]) -> Result<Vec<Cidr>, String> {
    entries
        .iter()
        .map(|entry| {
            let cidr = Cidr::parse(entry)?;
            // Such an entry used to match every IPv4 address through its
            // mapped form; flag configs that relied on it.
            if cidr.covers_ipv4_mapped_block() {
                tracing::warn!(
                    list,
                    cidr = %entry,
                    "ssrf_policy.{list} entry '{entry}' no longer covers IPv4 addresses; \
                     list an IPv4 CIDR (e.g. 0.0.0.0/0) to cover them"
                );
            }
            Ok(cidr)
        })
        .collect()
}

/// Suffix (`.cluster.local`) or exact match against a normalized hostname.
fn hostname_matches(host: &str, pattern: &str) -> bool {
    if pattern.starts_with('.') {
        host.ends_with(pattern) || host == &pattern[1..]
    } else {
        host == pattern
    }
}

// ---------------------------------------------------------------------------
// Built-in deny-lists
// ---------------------------------------------------------------------------

/// Check whether an IP is in a non-globally-routable range.
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
/// - `::a.b.c.d/96`        — IPv4-compatible (deprecated, RFC 4291 §2.5.5.1;
///   denied whatever it carries)
/// - `ff00::/8`            — multicast
/// - `100::/64`            — discard prefix (RFC 6666)
/// - `2001:db8::/32`       — documentation
/// - `2001::/32`           — Teredo (NAT traversal; embeds server/client IPs)
/// - `2002::/16`           — 6to4 (embeds IPv4; re-checked against IPv4 rules)
/// - `0:0:0:0:ffff:0::/96` — IPv4-translated (RFC 6052 §2.1; re-checked)
/// - `64:ff9b::/96`        — NAT64 well-known prefix (re-checked against IPv4)
/// - `64:ff9b:1::/48`      — local-use NAT64 (RFC 8215)
/// - `100:0:0:1::/64`      — dummy prefix (RFC 9780)
/// - `2001::/23`           — IETF protocol assignments (RFC 2928), except the
///   globally reachable `2001:1::1`, `2001:1::2`, `2001:1::3`, `2001:3::/32`,
///   `2001:4:112::/48`, `2001:20::/28` and `2001:30::/28`; covers
///   benchmarking `2001:2::/48` and the deprecated ORCHID `2001:10::/28`
/// - `3fff::/20`           — documentation (RFC 9637)
/// - `5f00::/16`           — SRv6 segment identifiers (RFC 9602)
/// - `fec0::/10`           — site-local (deprecated, RFC 3879)
/// - everything else outside `2000::/3` — not allocated for global unicast
///
/// The IPv6 list follows the IANA IPv6 Special-Purpose Address Registry
/// (2025-10-09): every block not globally reachable is denied. Outside
/// `2000::/3` only the mapped, translated and NAT64 forms are allowed, judged
/// by the IPv4 address they carry, so a block assigned there later is denied
/// without a change.
#[must_use]
pub fn is_ssrf_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_ssrf_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_ssrf_blocked_ipv6(v6),
    }
}

/// Addresses an `allow_hostnames` exemption never lifts: loopback,
/// unspecified / "this network", link-local (where most cloud metadata
/// endpoints live), multicast and broadcast, and the metadata endpoints outside
/// link-local. IPv6 forms that embed an IPv4 address are judged by it,
/// IPv4-compatible ones included: Linux `sit0` tunnels those to that address.
fn is_never_exempt_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_never_exempt_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_ipv4(v6) {
                return is_never_exempt_ipv4(v4);
            }
            let s = v6.segments();
            v6 == Ipv6Addr::LOCALHOST
                || v6 == Ipv6Addr::UNSPECIFIED
                || (s[0] & 0xFFC0) == 0xFE80
                || (s[0] & 0xFF00) == 0xFF00
                // AWS IMDS over IPv6.
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
                // GCP metadata server over IPv6.
                || v6 == Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254)
        }
    }
}

fn is_never_exempt_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 127
        || o[0] == 0
        || (o[0] == 169 && o[1] == 254)
        || (o[0] & 0xF0) == 0xE0
        || ip == Ipv4Addr::BROADCAST
        // Alibaba Cloud metadata, inside the shared 100.64.0.0/10 space.
        || ip == Ipv4Addr::new(100, 100, 100, 200)
        // Oracle Cloud's legacy metadata endpoint.
        || ip == Ipv4Addr::new(192, 0, 0, 192)
}

/// The IPv4 address an IPv6 address carries: mapped, compatible, translated,
/// NAT64 or 6to4.
fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4() {
        // Mapped (`::ffff:a.b.c.d`) and compatible (`::a.b.c.d`); `::1` and `::`
        // are judged as IPv6 instead.
        if ip != Ipv6Addr::LOCALHOST && ip != Ipv6Addr::UNSPECIFIED {
            return Some(v4);
        }
        return None;
    }
    let s = ip.segments();
    let tail = || Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    if s[..4] == [0, 0, 0, 0] && s[4] == 0xFFFF && s[5] == 0 {
        return Some(tail());
    }
    if s[0] == 0x0064 && s[1] == 0xFF9B && s[2..6] == [0, 0, 0, 0] {
        return Some(tail());
    }
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    None
}

/// Hostnames blocked at config time. Cloud metadata IPs (169.254.169.254)
/// are caught by the IP deny-list instead.
const BLOCKED_HOSTNAMES: &[&str] = &["localhost", "localhost.localdomain"];

/// Check the static hostname deny-list. Returns the matched entry or `None`.
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

    if ipv6_special_purpose_reason(ip).is_some() {
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

    // ::a.b.c.d — IPv4-compatible (deprecated, RFC 4291 §2.5.5.1), denied
    // whatever it carries. A Linux host with `sit0` up tunnels it to the IPv4
    // address in its last 32 bits; elsewhere it leaves as native IPv6 for an
    // address outside 2000::/3. Neither reading is safe to allow. `to_ipv4`
    // also returns Some for mapped addresses, handled above; `::` and `::1` too.
    if ip.to_ipv4().is_some() {
        return true;
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

    !is_global_unicast_space(ip)
}

/// Whether `ip` is in `2000::/3`, the only IPv6 space IANA allocates for
/// global unicast. Outside it, an address is reachable only through internal
/// routing, unless it is a mapped, translated or NAT64 address carrying an
/// IPv4 address (checked before this).
fn is_global_unicast_space(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xE000) == 0x2000
}

/// The reason for denying an address in a non-globally-reachable IANA
/// special-purpose block that carries no IPv4 address to judge it by.
/// Teredo is handled by the callers.
fn ipv6_special_purpose_reason(ip: Ipv6Addr) -> Option<&'static str> {
    let s = ip.segments();
    // Deprecated, but networks that still route it treat it as private.
    if (s[0] & 0xFFC0) == 0xFEC0 {
        return Some("site-local address (fec0::/10)");
    }
    // The operator picks where the IPv4 address sits, so it cannot be
    // extracted and re-checked as with 64:ff9b::/96.
    if s[0] == 0x0064 && s[1] == 0xFF9B && s[2] == 0x0001 {
        return Some("local-use NAT64 address (64:ff9b:1::/48)");
    }
    if s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 1 {
        return Some("dummy prefix (100:0:0:1::/64)");
    }
    if s[0] == 0x3FFF && s[1] < 0x1000 {
        return Some("documentation address (3fff::/20)");
    }
    if s[0] == 0x5F00 {
        return Some("SRv6 segment identifier (5f00::/16)");
    }
    if s[0] == 0x2001 && s[1] < 0x0200 {
        if s[1] == 0x0002 && s[2] == 0 {
            return Some("benchmarking address (2001:2::/48)");
        }
        if (s[1] & 0xFFF0) == 0x0010 {
            return Some("deprecated ORCHID address (2001:10::/28)");
        }
        let global = (s[1] == 0x0001 && s[2..7] == [0; 5] && (1..=3).contains(&s[7]))
            || s[1] == 0x0003
            || (s[1] == 0x0004 && s[2] == 0x0112)
            || (0x0020..=0x003F).contains(&s[1]);
        if !global {
            return Some("IETF protocol assignment (2001::/23)");
        }
    }
    None
}

/// Human-readable reason for a blocked IP, or `"allowed"`.
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
    if let Some(reason) = ipv6_special_purpose_reason(ip) {
        return reason;
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
    if ip.to_ipv4().is_some() {
        return "deprecated IPv4-compatible address (::/96)";
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
    if !is_global_unicast_space(ip) {
        return "address outside global unicast space (2000::/3)";
    }
    "allowed"
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tracing_test::traced_test;

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
    fn ipv6_compatible_ipv4_public_blocked() {
        // Nothing translates ::8.8.8.8 to 8.8.8.8; it would leave as IPv6.
        assert!(is_ssrf_blocked_ip("::8.8.8.8".parse().unwrap()));
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
        assert_eq!(
            ssrf_block_reason("::10.0.0.1".parse().unwrap()),
            "deprecated IPv4-compatible address (::/96)"
        );
    }

    #[test]
    fn ipv4_compatible_boundaries() {
        let compatible = "deprecated IPv4-compatible address (::/96)";
        for (addr, reason) in [
            ("::", "unspecified address (::)"),
            ("::1", "loopback address (::1)"),
            ("::2", compatible),
            ("::0.0.0.2", compatible),
            ("::ffff:0", compatible),
            ("::ffff:ffff", compatible),
            ("::1:0:0", "address outside global unicast space (2000::/3)"),
            (
                "::fffe:0:1",
                "address outside global unicast space (2000::/3)",
            ),
            (
                "::ffff:0:0",
                "IPv4-mapped IPv6 address with blocked embedded IPv4 (::ffff:0:0/96)",
            ),
        ] {
            let ip: IpAddr = addr.parse().unwrap();
            assert_eq!(ssrf_block_reason(ip), reason, "{addr}");
            assert!(is_ssrf_blocked_ip(ip), "{addr}");
        }
    }

    #[test]
    fn reason_compatible_v4_public() {
        assert_eq!(
            ssrf_block_reason("::8.8.8.8".parse().unwrap()),
            "deprecated IPv4-compatible address (::/96)"
        );
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
    fn blocked_hostname_case_insensitive() {
        assert_eq!(is_ssrf_blocked_hostname("LocalHost"), Some("localhost"));
    }

    #[test]
    fn blocked_hostname_trailing_dot() {
        assert_eq!(is_ssrf_blocked_hostname("localhost."), Some("localhost"));
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

    // ===================================================================
    // Cidr tests
    // ===================================================================

    #[test]
    fn cidr_parse_ipv4_valid() {
        let cidr = Cidr::parse("10.96.0.0/12").unwrap();
        assert!(cidr.contains("10.96.0.1".parse().unwrap()));
        assert!(cidr.contains("10.111.255.255".parse().unwrap()));
        assert!(!cidr.contains("10.112.0.0".parse().unwrap()));
        assert!(!cidr.contains("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_ipv4_host_32() {
        let cidr = Cidr::parse("10.0.5.20/32").unwrap();
        assert!(cidr.contains("10.0.5.20".parse().unwrap()));
        assert!(!cidr.contains("10.0.5.21".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_ipv4_slash_0() {
        let cidr = Cidr::parse("0.0.0.0/0").unwrap();
        assert!(cidr.contains("1.2.3.4".parse().unwrap()));
        assert!(cidr.contains("255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_ipv6_valid() {
        let cidr = Cidr::parse("fd00::/8").unwrap();
        assert!(cidr.contains("fd12::1".parse().unwrap()));
        assert!(!cidr.contains("fe80::1".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_rejects_missing_slash() {
        assert!(Cidr::parse("10.0.0.0").is_err());
    }

    #[test]
    fn cidr_parse_rejects_bad_address() {
        assert!(Cidr::parse("999.999.999.999/8").is_err());
    }

    #[test]
    fn cidr_parse_rejects_ipv4_prefix_too_large() {
        assert!(Cidr::parse("10.0.0.0/33").is_err());
    }

    #[test]
    fn cidr_parse_rejects_ipv6_prefix_too_large() {
        assert!(Cidr::parse("fd00::/129").is_err());
    }

    // ===================================================================
    // SsrfGuard tests
    // ===================================================================

    use super::SsrfPolicy;

    fn guard(cfg: SsrfPolicy) -> SsrfGuard {
        SsrfGuard::from_config(&cfg).unwrap()
    }

    #[test]
    fn guard_disabled_allows_everything() {
        let e = SsrfGuard::disabled();
        assert!(!e.is_ip_blocked("127.0.0.1".parse().unwrap()));
        assert!(!e.is_ip_blocked("10.0.0.1".parse().unwrap()));
        assert!(e.is_hostname_blocked("localhost").is_none());
    }

    #[test]
    fn guard_default_blocks_builtin_ranges() {
        let e = guard(SsrfPolicy::default());
        assert!(e.is_ip_blocked("127.0.0.1".parse().unwrap()));
        assert!(e.is_ip_blocked("10.0.0.1".parse().unwrap()));
        assert!(e.is_ip_blocked("192.168.1.1".parse().unwrap()));
        assert!(!e.is_ip_blocked("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn guard_extra_deny_cidrs_blocks_additional_range() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_cidrs: vec!["203.0.114.0/24".into()],
            ..Default::default()
        });
        // 203.0.114.x is NOT in the built-in deny-list but IS in extra.
        assert!(e.is_ip_blocked("203.0.114.5".parse().unwrap()));
        assert!(!e.is_ip_blocked("203.0.115.5".parse().unwrap()));
        // Built-in still works too.
        assert!(e.is_ip_blocked("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn guard_allow_cidrs_exempts_from_builtin() {
        let e = guard(SsrfPolicy {
            enabled: true,
            allow_cidrs: vec!["10.0.5.20/32".into()],
            ..Default::default()
        });
        // 10.0.5.20 is in the built-in deny (10.0.0.0/8) but allow-listed.
        assert!(!e.is_ip_blocked("10.0.5.20".parse().unwrap()));
        // Other 10.x IPs still blocked.
        assert!(e.is_ip_blocked("10.0.5.21".parse().unwrap()));
    }

    #[test]
    fn guard_allow_cidrs_exempts_from_extra_deny() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_cidrs: vec!["203.0.114.0/24".into()],
            allow_cidrs: vec!["203.0.114.10/32".into()],
            ..Default::default()
        });
        assert!(!e.is_ip_blocked("203.0.114.10".parse().unwrap()));
        assert!(e.is_ip_blocked("203.0.114.11".parse().unwrap()));
    }

    #[test]
    fn guard_extra_deny_hostnames_exact() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec!["kubernetes.default.svc".into()],
            ..Default::default()
        });
        assert!(e.is_hostname_blocked("kubernetes.default.svc").is_some());
        assert!(e.is_hostname_blocked("other.svc").is_none());
    }

    #[test]
    fn guard_extra_deny_hostnames_suffix() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec![".cluster.local".into()],
            ..Default::default()
        });
        assert!(
            e.is_hostname_blocked("my-svc.default.svc.cluster.local")
                .is_some()
        );
        assert!(e.is_hostname_blocked("cluster.local").is_some());
        assert!(e.is_hostname_blocked("notcluster.local").is_none());
    }

    #[test]
    fn guard_extra_deny_hostnames_case_insensitive() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec![".Cluster.Local".into()],
            ..Default::default()
        });
        assert!(e.is_hostname_blocked("foo.cluster.local").is_some());
    }

    #[test]
    fn guard_extra_deny_hostnames_trailing_dot() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec![".cluster.local.".into()],
            ..Default::default()
        });
        // Pattern has trailing dot stripped during normalization.
        assert!(e.is_hostname_blocked("foo.cluster.local").is_some());
    }

    #[test]
    fn guard_allow_hostnames_exempts_from_builtin() {
        let e = guard(SsrfPolicy {
            enabled: true,
            allow_hostnames: vec!["localhost".into()],
            ..Default::default()
        });
        // localhost is in the built-in deny-list but allow-listed.
        assert!(e.is_hostname_blocked("localhost").is_none());
        // Other built-in blocked hostnames still blocked.
        assert!(e.is_hostname_blocked("localhost.localdomain").is_some());
    }

    #[test]
    fn guard_allow_hostnames_exempts_from_extra_deny() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec![".cluster.local".into()],
            allow_hostnames: vec!["special-svc.cluster.local".into()],
            ..Default::default()
        });
        assert!(e.is_hostname_blocked("special-svc.cluster.local").is_none());
        assert!(e.is_hostname_blocked("other-svc.cluster.local").is_some());
    }

    #[test]
    fn guard_allow_hostnames_suffix() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_hostnames: vec![".internal".into()],
            allow_hostnames: vec![".safe.internal".into()],
            ..Default::default()
        });
        assert!(e.is_hostname_blocked("evil.internal").is_some());
        assert!(e.is_hostname_blocked("svc.safe.internal").is_none());
    }

    #[test]
    fn resolved_ip_exemption_is_scoped_to_configured_hostname() {
        let e = guard(SsrfPolicy {
            allow_hostnames: vec!["demo-echo.demo.svc.cluster.local".into()],
            ..Default::default()
        });
        let private = "10.111.56.200".parse().unwrap();
        assert!(!e.is_resolved_ip_blocked("DEMO-ECHO.demo.svc.cluster.local.", private));
        assert!(e.is_resolved_ip_blocked("other.demo.svc.cluster.local", private));
        assert!(e.is_resolved_ip_blocked("demo-echo.demo.svc.cluster.local.attacker.org", private));
        assert!(e.is_resolved_ip_blocked("public.example", "169.254.169.254".parse().unwrap()));
        assert!(e.is_ip_blocked(private));
    }

    #[test]
    fn an_exempted_hostname_never_reaches_loopback_link_local_or_metadata() {
        let e = guard(SsrfPolicy {
            allow_hostnames: vec!["svc.cluster.local".into()],
            ..Default::default()
        });
        let host = "svc.cluster.local";
        for private in [
            "10.1.2.3",
            "172.16.0.9",
            "192.168.4.4",
            "100.64.0.1",
            "fd12::1",
        ] {
            assert!(
                !e.is_resolved_ip_blocked(host, private.parse().unwrap()),
                "{private}: the exemption lifts the private-range deny"
            );
        }
        for never in [
            "169.254.169.254",
            "169.254.1.1",
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "100.100.100.200",
            "192.0.0.192",
            "::1",
            "::",
            "fe80::1",
            "ff02::1",
            "fd00:ec2::254",
            "fd20:ce::254",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:a9fe:a9fe::",
        ] {
            assert!(
                e.is_resolved_ip_blocked(host, never.parse().unwrap()),
                "{never}: an exempted name must not reach it"
            );
        }
        assert!(
            e.resolved_block_reason(host, "169.254.169.254".parse().unwrap())
                .contains("allow_hostnames"),
            "the reason names the exemption it did not honour"
        );
    }

    #[test]
    fn allow_cidrs_still_admit_a_never_exempt_address_for_an_exempted_name() {
        let e = guard(SsrfPolicy {
            allow_hostnames: vec!["localhost".into()],
            allow_cidrs: vec!["127.0.0.0/8".into()],
            ..Default::default()
        });
        assert!(!e.is_resolved_ip_blocked("localhost", "127.0.0.1".parse().unwrap()));
        assert!(e.is_resolved_ip_blocked("localhost", "169.254.169.254".parse().unwrap()));
    }

    #[test]
    fn resolved_ip_applies_suffix_exemptions_and_hostname_denials() {
        let e = guard(SsrfPolicy {
            allow_hostnames: vec![".safe.internal".into()],
            extra_deny_hostnames: vec!["blocked.example".into()],
            ..Default::default()
        });
        assert!(!e.is_resolved_ip_blocked("svc.safe.internal", "10.0.0.1".parse().unwrap()));
        assert!(e.is_resolved_ip_blocked("svc.unsafe.internal", "10.0.0.1".parse().unwrap()));
        assert!(e.is_resolved_ip_blocked("blocked.example", "8.8.8.8".parse().unwrap()));
        assert!(!e.is_resolved_ip_blocked("public.example", "8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn guard_ip_block_reason_extra_deny() {
        let e = guard(SsrfPolicy {
            enabled: true,
            extra_deny_cidrs: vec!["203.0.114.0/24".into()],
            ..Default::default()
        });
        assert_eq!(
            e.ip_block_reason("203.0.114.5".parse().unwrap()),
            "blocked by extra_deny_cidrs"
        );
        assert_eq!(e.ip_block_reason("8.8.8.8".parse().unwrap()), "allowed");
    }

    #[test]
    fn guard_from_config_rejects_bad_cidr() {
        let result = SsrfGuard::from_config(&SsrfPolicy {
            enabled: true,
            extra_deny_cidrs: vec!["not-a-cidr".into()],
            ..Default::default()
        });
        assert!(result.is_err());
    }

    // ===================================================================
    // hostname_matches tests
    // ===================================================================

    #[test]
    fn a_denied_hostname_is_the_reason_its_public_address_is_refused() {
        let guard = SsrfGuard::from_config(&SsrfPolicy {
            extra_deny_hostnames: vec![".blocked.example".into()],
            ..Default::default()
        })
        .unwrap();
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        assert!(guard.is_resolved_ip_blocked("api.blocked.example", public));
        assert_eq!(
            guard.resolved_block_reason("api.blocked.example", public),
            ".blocked.example"
        );
        assert_eq!(
            guard.resolved_block_reason("api.example", public),
            "allowed"
        );
    }

    #[test]
    fn hostname_matches_exact() {
        assert!(hostname_matches("foo.bar", "foo.bar"));
        assert!(!hostname_matches("baz.bar", "foo.bar"));
    }

    #[test]
    fn hostname_matches_suffix() {
        assert!(hostname_matches("a.cluster.local", ".cluster.local"));
        assert!(hostname_matches("a.b.cluster.local", ".cluster.local"));
        // The suffix itself matches (.cluster.local matches cluster.local).
        assert!(hostname_matches("cluster.local", ".cluster.local"));
    }

    #[test]
    fn hostname_matches_suffix_no_partial() {
        // "notcluster.local" should NOT match ".cluster.local".
        assert!(!hostname_matches("notcluster.local", ".cluster.local"));
    }

    // ===================================================================
    // Config deserialization tests
    // ===================================================================

    #[test]
    fn config_ssrf_policy_deserializes_full_form() {
        let json = r#"{
            "enabled": true,
            "extra_deny_cidrs": ["10.96.0.0/12"],
            "allow_cidrs": ["10.0.5.20/32"],
            "extra_deny_hostnames": [".cluster.local"],
            "allow_hostnames": ["special-svc.cluster.local"]
        }"#;
        let policy: SsrfPolicy = serde_json::from_str(json).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.extra_deny_cidrs.len(), 1);
        assert_eq!(policy.allow_cidrs.len(), 1);
        assert_eq!(policy.extra_deny_hostnames.len(), 1);
        assert_eq!(policy.allow_hostnames.len(), 1);
    }

    #[test]
    fn config_ssrf_policy_defaults_to_enabled() {
        let json = "{}";
        let policy: SsrfPolicy = serde_json::from_str(json).unwrap();
        assert!(policy.enabled);
        assert!(policy.extra_deny_cidrs.is_empty());
    }

    #[test]
    fn config_ssrf_policy_disabled() {
        let json = r#"{"enabled": false}"#;
        let policy: SsrfPolicy = serde_json::from_str(json).unwrap();
        assert!(!policy.enabled);
    }

    // ===================================================================
    // IANA IPv6 special-purpose blocks that are not globally reachable
    // ===================================================================

    const OUTSIDE_2000_3: &str = "address outside global unicast space (2000::/3)";

    /// Refused by the default guard with `reason`, through both the
    /// address check and the resolved-address check.
    #[track_caller]
    fn assert_denied(addr: &str, reason: &str) {
        let g = guard(SsrfPolicy::default());
        let ip: IpAddr = addr.parse().unwrap();
        assert!(g.is_ip_blocked(ip), "{addr} is denied");
        assert!(
            g.is_resolved_ip_blocked("api.example.com", ip),
            "{addr} is denied once resolved"
        );
        assert_eq!(g.ip_block_reason(ip), reason, "{addr}");
    }

    #[track_caller]
    fn assert_allowed(addr: &str) {
        let g = guard(SsrfPolicy::default());
        let ip: IpAddr = addr.parse().unwrap();
        assert!(!g.is_ip_blocked(ip), "{addr} is allowed");
        assert_eq!(g.ip_block_reason(ip), "allowed", "{addr}");
    }

    #[test]
    fn site_local_is_denied() {
        let reason = "site-local address (fec0::/10)";
        assert_denied("fec0::1", reason);
        assert_denied("feff:ffff::1", reason);
        // Both neighbours are denied for their own reasons.
        assert_denied("febf:ffff::1", "link-local address (fe80::/10)");
        assert_denied("ff00::1", "multicast address (ff00::/8)");
    }

    #[test]
    fn local_use_nat64_is_denied_whatever_it_embeds() {
        let reason = "local-use NAT64 address (64:ff9b:1::/48)";
        assert_denied("64:ff9b:1::a00:1", reason);
        assert_denied("64:ff9b:1::808:808", reason);
        assert_denied("64:ff9b:1:ffff:ffff:ffff:ffff:ffff", reason);
        assert_denied("64:ff9b:2::1", OUTSIDE_2000_3);
        assert_allowed("64:ff9b::808:808");
    }

    #[test]
    fn dummy_prefix_is_denied() {
        assert_denied("100:0:0:1::1", "dummy prefix (100:0:0:1::/64)");
        assert_denied(
            "100:0:0:1:ffff:ffff:ffff:ffff",
            "dummy prefix (100:0:0:1::/64)",
        );
        assert_denied("100:0:0:2::1", OUTSIDE_2000_3);
    }

    #[test]
    fn ipv6_benchmarking_is_denied() {
        let reason = "benchmarking address (2001:2::/48)";
        assert_denied("2001:2::1", reason);
        assert_denied("2001:2:0:ffff::1", reason);
        // AMT, globally reachable.
        assert_allowed("2001:3::1");
    }

    #[test]
    fn deprecated_orchid_is_denied() {
        let reason = "deprecated ORCHID address (2001:10::/28)";
        assert_denied("2001:10::1", reason);
        assert_denied("2001:1f:ffff::1", reason);
        // ORCHIDv2, globally reachable.
        assert_allowed("2001:20::1");
    }

    #[test]
    fn ietf_protocol_assignments_are_denied_except_the_globally_reachable_ones() {
        let reason = "IETF protocol assignment (2001::/23)";
        for denied in [
            "2001:1::",
            "2001:1::4",
            "2001:2:1::1",
            "2001:4:113::1",
            "2001:5::1",
            "2001:40::1",
            "2001:1ff:ffff::1",
        ] {
            assert_denied(denied, reason);
        }
        for allowed in [
            "2001:1::1",
            "2001:1::2",
            "2001:1::3",
            "2001:3:ffff::1",
            "2001:4:112::1",
            "2001:2f::1",
            "2001:30::1",
            "2001:3f:ffff::1",
            // Just past 2001::/23.
            "2001:200::1",
        ] {
            assert_allowed(allowed);
        }
    }

    #[test]
    fn ipv6_documentation_3fff_is_denied() {
        let reason = "documentation address (3fff::/20)";
        assert_denied("3fff::1", reason);
        assert_denied("3fff:fff:ffff::1", reason);
        assert_allowed("3fff:1000::1");
        assert_allowed("3ffe:ffff::1");
    }

    #[test]
    fn srv6_sids_are_denied() {
        let reason = "SRv6 segment identifier (5f00::/16)";
        assert_denied("5f00::1", reason);
        assert_denied("5f00:ffff::1", reason);
        assert_denied("5eff:ffff::1", OUTSIDE_2000_3);
        assert_denied("5f01::1", OUTSIDE_2000_3);
    }

    #[test]
    fn ipv6_outside_global_unicast_space_is_denied() {
        for denied in [
            "::1:0:0:1",
            "1::1",
            "1fff:ffff::1",
            "4000::1",
            "8000::1",
            "dead:beef::1",
            "fbff:ffff::1",
        ] {
            assert_denied(denied, OUTSIDE_2000_3);
        }
        assert_allowed("2000::1");
        assert_allowed("3fff:ffff:ffff::1");
        // IPv4 carriers outside 2000::/3 are judged by their IPv4 address,
        // except the deprecated IPv4-compatible form.
        assert_denied("::808:808", "deprecated IPv4-compatible address (::/96)");
        for carrier in ["::ffff:8.8.8.8", "::ffff:0:808:808", "64:ff9b::808:808"] {
            assert_allowed(carrier);
        }
    }

    /// Addresses around the special-purpose IPv6 prefixes: a random number of
    /// a prefix's leading bits, the rest random.
    fn near_special_ipv6() -> impl Strategy<Value = Ipv6Addr> {
        const PREFIXES: [u128; 16] = [
            0,
            0xFFFF << 32,
            0xFFFF << 48,
            0x0064_FF9B << 96,
            0x0064_FF9B_0001 << 80,
            0x0100 << 112,
            0x0100_0000_0000_0001 << 64,
            0x2001 << 112,
            0x2001_0DB8 << 96,
            0x2002 << 112,
            0x3FFF << 112,
            0x5F00 << 112,
            0xFC00 << 112,
            0xFE80 << 112,
            0xFEC0 << 112,
            0xFF00 << 112,
        ];
        (0..PREFIXES.len(), 0u32..=128, any::<u128>()).prop_map(|(i, keep, rest)| {
            let mask = u128::MAX.checked_shl(128 - keep).unwrap_or(0);
            Ipv6Addr::from((PREFIXES[i] & mask) | (rest & !mask))
        })
    }

    proptest! {
        #[test]
        fn the_built_in_verdict_and_reason_agree(
            v6 in prop_oneof![any::<u128>().prop_map(Ipv6Addr::from), near_special_ipv6()],
            v4 in any::<u32>(),
        ) {
            for ip in [IpAddr::V6(v6), IpAddr::V4(Ipv4Addr::from(v4))] {
                prop_assert_eq!(is_ssrf_blocked_ip(ip), ssrf_block_reason(ip) != "allowed", "{}", ip);
            }
        }
    }

    // ===================================================================
    // CIDR address families
    // ===================================================================

    #[test]
    fn an_ipv6_deny_cidr_does_not_refuse_ipv4() {
        let g = guard(SsrfPolicy {
            extra_deny_cidrs: vec!["::/3".into()],
            ..Default::default()
        });
        assert!(!g.is_ip_blocked("8.8.8.8".parse().unwrap()));
        assert!(!g.is_ip_blocked("::ffff:8.8.8.8".parse().unwrap()));
        assert!(!g.is_resolved_ip_blocked("dns.google", "8.8.8.8".parse().unwrap()));
        assert_eq!(g.ip_block_reason("8.8.8.8".parse().unwrap()), "allowed");
        // IPv6 addresses in ::/3 are refused by their bits, including the
        // NAT64 form of 8.8.8.8, which the built-in list alone allows.
        let nat64: IpAddr = "64:ff9b::808:808".parse().unwrap();
        assert!(!guard(SsrfPolicy::default()).is_ip_blocked(nat64));
        assert!(g.is_ip_blocked(nat64));
        assert_eq!(g.ip_block_reason(nat64), "blocked by extra_deny_cidrs");
    }

    #[test]
    fn an_ipv6_allow_cidr_does_not_admit_ipv4() {
        let g = guard(SsrfPolicy {
            allow_cidrs: vec!["::/0".into()],
            ..Default::default()
        });
        assert!(g.is_ip_blocked("10.0.0.1".parse().unwrap()));
        assert!(g.is_ip_blocked("::ffff:10.0.0.1".parse().unwrap()));
        assert_eq!(
            g.ip_block_reason("10.0.0.1".parse().unwrap()),
            "private address (10.0.0.0/8)"
        );
        assert!(!g.is_ip_blocked("fd00::1".parse().unwrap()));
    }

    #[test]
    fn an_ipv4_mapped_cidr_is_its_ipv4_cidr() {
        assert_eq!(
            Cidr::parse("::ffff:10.0.0.0/104").unwrap(),
            Cidr::parse("10.0.0.0/8").unwrap()
        );
        assert_eq!(
            Cidr::parse("::ffff:0:0/96").unwrap(),
            Cidr::parse("0.0.0.0/0").unwrap()
        );
        let cidr = Cidr::parse("::ffff:10.0.0.0/104").unwrap();
        assert!(cidr.contains("10.1.2.3".parse().unwrap()));
        assert!(!cidr.contains("11.1.2.3".parse().unwrap()));

        let g = guard(SsrfPolicy {
            extra_deny_cidrs: vec!["::ffff:10.0.0.0/104".into(), "::ffff:8.8.8.0/120".into()],
            ..Default::default()
        });
        assert!(g.is_ip_blocked("10.1.2.3".parse().unwrap()));
        for ip in ["8.8.8.8", "::ffff:8.8.8.8"] {
            assert_eq!(
                g.ip_block_reason(ip.parse().unwrap()),
                "blocked by extra_deny_cidrs",
                "{ip}"
            );
        }
        assert_eq!(g.ip_block_reason("8.8.4.4".parse().unwrap()), "allowed");
    }

    #[test]
    fn an_ipv4_cidr_matches_other_ipv6_forms_only_when_denying() {
        let cidr = Cidr::parse("10.0.0.0/8").unwrap();
        for carrier in [
            "::ffff:10.1.2.3",
            "::a01:203",
            "::ffff:0:a01:203",
            "64:ff9b::a01:203",
            "64:ff9b::a00:1",
            "2002:a01:203::1",
            "2002:a00:1::1",
        ] {
            let ip = carrier.parse().unwrap();
            assert!(cidr.contains_or_embeds(ip), "{carrier} is denied");
            assert_eq!(
                cidr.contains(ip),
                carrier == "::ffff:10.1.2.3",
                "{carrier}: only the mapped form is allowed"
            );
        }
        for other in ["fd00::a01:203", "64:ff9b:1::a01:203", "2002:b01:203::1"] {
            let ip = other.parse().unwrap();
            assert!(!cidr.contains(ip), "{other}");
            assert!(!cidr.contains_or_embeds(ip), "{other}");
        }
        let ipv6 = Cidr::parse("::ffff:0:0/95").unwrap();
        assert!(!ipv6.contains("10.1.2.3".parse().unwrap()));
        assert!(!ipv6.contains_or_embeds("::ffff:10.1.2.3".parse().unwrap()));
        assert!(ipv6.contains("::fffe:0:1".parse().unwrap()));
    }

    #[test]
    fn an_ipv4_deny_cidr_refuses_the_ipv6_forms_that_carry_the_address() {
        let g = guard(SsrfPolicy {
            extra_deny_cidrs: vec!["8.8.8.0/24".into()],
            ..Default::default()
        });
        for carrier in [
            "8.8.8.8",
            "::ffff:8.8.8.8",
            "::ffff:0:808:808",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            let ip: IpAddr = carrier.parse().unwrap();
            assert_eq!(
                g.ip_block_reason(ip),
                "blocked by extra_deny_cidrs",
                "{carrier}"
            );
            assert!(g.is_ip_blocked(ip), "{carrier}");
            assert!(g.is_resolved_ip_blocked("dns.google", ip), "{carrier}");
        }
        // The built-in list denies every IPv4-compatible address first.
        assert_eq!(
            g.ip_block_reason("::808:808".parse().unwrap()),
            "deprecated IPv4-compatible address (::/96)"
        );
        for other in ["8.8.4.4", "64:ff9b::808:404", "2002:808:404::1"] {
            assert!(!g.is_ip_blocked(other.parse().unwrap()), "{other}");
        }
    }

    #[test]
    fn an_ipv4_allow_cidr_does_not_exempt_the_ipv6_forms_that_carry_the_address() {
        let g = guard(SsrfPolicy {
            allow_cidrs: vec!["10.0.0.0/8".into()],
            ..Default::default()
        });
        assert!(!g.is_ip_blocked("10.0.0.1".parse().unwrap()));
        assert!(!g.is_ip_blocked("::ffff:10.0.0.1".parse().unwrap()));
        for carrier in [
            "::a00:1",
            "::ffff:0:a00:1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
        ] {
            assert!(g.is_ip_blocked(carrier.parse().unwrap()), "{carrier}");
        }
        assert_eq!(
            g.ip_block_reason("64:ff9b::a00:1".parse().unwrap()),
            "NAT64 address with blocked embedded IPv4 (64:ff9b::/96)"
        );

        let exempt = guard(SsrfPolicy {
            allow_hostnames: vec!["localhost".into()],
            allow_cidrs: vec!["127.0.0.0/8".into()],
            ..Default::default()
        });
        assert!(!exempt.is_resolved_ip_blocked("localhost", "127.0.0.1".parse().unwrap()));
        assert!(exempt.is_resolved_ip_blocked("localhost", "64:ff9b::7f00:1".parse().unwrap()));
    }

    #[test]
    fn an_ipv6_host_cidr_matches_one_address() {
        let cidr = Cidr::parse("2001:4860:4860::8888/128").unwrap();
        assert!(cidr.contains("2001:4860:4860::8888".parse().unwrap()));
        for other in ["2001:4860:4860::8887", "2001:4860:4860::8889", "::8888"] {
            assert!(!cidr.contains(other.parse().unwrap()), "{other}");
            assert!(!cidr.contains_or_embeds(other.parse().unwrap()), "{other}");
        }
    }

    #[test]
    fn an_ipv4_mapped_host_cidr_is_its_ipv4_host_cidr() {
        let cidr = Cidr::parse("::ffff:10.0.5.20/128").unwrap();
        assert_eq!(cidr, Cidr::parse("10.0.5.20/32").unwrap());
        assert!(cidr.contains("10.0.5.20".parse().unwrap()));
        assert!(!cidr.contains("10.0.5.21".parse().unwrap()));
    }

    #[test]
    #[traced_test]
    fn an_ipv6_cidr_covering_the_ipv4_mapped_block_is_flagged() {
        SsrfGuard::from_config(&SsrfPolicy {
            extra_deny_cidrs: vec!["::/0".into(), "::/3".into(), "fc00::/7".into()],
            allow_cidrs: vec!["::/80".into(), "::ffff:0:0/96".into()],
            ..Default::default()
        })
        .unwrap();
        assert!(logs_contain(
            "ssrf_policy.extra_deny_cidrs entry '::/0' no longer covers IPv4 addresses"
        ));
        assert!(logs_contain(
            "ssrf_policy.allow_cidrs entry '::/80' no longer covers IPv4 addresses"
        ));
        assert!(logs_contain(
            "ssrf_policy.extra_deny_cidrs entry '::/3' no longer covers IPv4 addresses"
        ));
        assert!(logs_contain("0.0.0.0/0"));
        logs_assert(|lines| {
            let flagged = lines
                .iter()
                .filter(|line| line.contains("no longer covers IPv4"))
                .count();
            if flagged != 3 {
                return Err(format!("{flagged} warnings, expected 3"));
            }
            if lines.iter().any(|line| line.contains("fc00::/7")) {
                return Err("fc00::/7 does not cover ::ffff:0:0/96".into());
            }
            Ok(())
        });
    }

    // ===================================================================
    // CIDR family invariants
    // ===================================================================

    fn ipv6(bits: u128) -> IpAddr {
        IpAddr::V6(Ipv6Addr::from(bits))
    }

    fn mapped(x: Ipv4Addr) -> IpAddr {
        IpAddr::V6(x.to_ipv6_mapped())
    }

    /// The IPv6 forms other than IPv4-mapped that carry `x`, as
    /// `embedded_ipv4` reads them. `suffix` fills the 6to4 bits after the
    /// IPv4 address.
    fn carriers(x: Ipv4Addr, suffix: u128) -> Vec<IpAddr> {
        let x = u128::from(u32::from(x));
        let mut forms = vec![
            // IPv4-translated ::ffff:0:a.b.c.d
            ipv6((0xFFFF << 48) | x),
            // NAT64 64:ff9b::a.b.c.d
            ipv6((0x0064_FF9B << 96) | x),
            // 6to4 2002:aabb:ccdd::/48
            ipv6((0x2002 << 112) | (x << 80) | (suffix & ((1 << 80) - 1))),
        ];
        // IPv4-compatible; `::` and `::1` are judged as IPv6.
        if x > 1 {
            forms.push(ipv6(x));
        }
        forms
    }

    fn any_cidr() -> impl Strategy<Value = Cidr> {
        let v4 = |a: u32, p: u8| format!("{}/{p}", Ipv4Addr::from(a));
        let v6 = |a: u128, p: u8| format!("{}/{p}", Ipv6Addr::from(a));
        prop_oneof![
            (any::<u32>(), 0u8..=32).prop_map(move |(a, p)| v4(a, p)),
            (any::<u128>(), 0u8..=128).prop_map(move |(a, p)| v6(a, p)),
            // Around the mapped block and the IPv6 forms that carry IPv4.
            (any::<u32>(), 64u8..=128)
                .prop_map(move |(a, p)| v6((0xFFFF << 32) | u128::from(a), p)),
            (any::<u32>(), 0u8..=128).prop_map(move |(a, p)| v6(u128::from(a), p)),
            (any::<u32>(), 0u8..=128)
                .prop_map(move |(a, p)| v6((0x0064_FF9B << 96) | u128::from(a), p)),
            (any::<u32>(), 0u8..=128)
                .prop_map(move |(a, p)| v6((0x2002 << 112) | (u128::from(a) << 80), p)),
        ]
        .prop_map(|s| Cidr::parse(&s).unwrap())
    }

    proptest! {
        #[test]
        fn a_mapped_cidr_parses_as_its_ipv4_cidr(a in any::<u32>(), p in 0u8..=32) {
            let a = Ipv4Addr::from(a);
            prop_assert_eq!(
                Cidr::parse(&format!("::ffff:{a}/{}", p + 96)).unwrap(),
                Cidr::parse(&format!("{a}/{p}")).unwrap()
            );
        }

        #[test]
        fn an_ipv4_address_and_its_mapped_form_match_alike(cidr in any_cidr(), x in any::<u32>()) {
            let x = Ipv4Addr::from(x);
            prop_assert_eq!(cidr.contains(IpAddr::V4(x)), cidr.contains(mapped(x)));
            prop_assert_eq!(
                cidr.contains_or_embeds(IpAddr::V4(x)),
                cidr.contains_or_embeds(mapped(x))
            );
        }

        /// An IPv6 CIDR never matches an IPv4 address or its mapped form,
        /// and matches every other IPv6 address, NAT64 and 6to4 included, by
        /// its bits alone.
        #[test]
        fn an_ipv6_cidr_matches_ipv6_bits_only(
            cidr in any_cidr(),
            x in any::<u32>(),
            bits in any::<u128>(),
            suffix in any::<u128>(),
        ) {
            let Cidr::V6 { network, mask } = cidr else {
                return Ok(());
            };
            let x = Ipv4Addr::from(x);
            for ip in [IpAddr::V4(x), mapped(x)] {
                prop_assert!(!cidr.contains(ip));
                prop_assert!(!cidr.contains_or_embeds(ip));
            }
            for ip in carriers(x, suffix).into_iter().chain([ipv6(bits)]) {
                let IpAddr::V6(v6) = ip else { unreachable!() };
                if v6.to_ipv4_mapped().is_some() {
                    continue;
                }
                let raw = u128::from(v6) & mask == network;
                prop_assert_eq!(cidr.contains(ip), raw);
                prop_assert_eq!(cidr.contains_or_embeds(ip), raw);
            }
        }

        /// An IPv4 CIDR denies every IPv6 form that carries an address it
        /// holds, and allows none of them but the mapped one.
        #[test]
        fn an_ipv4_cidr_judges_ipv6_forms_by_their_ipv4_address(
            a in any::<u32>(),
            p in 0u8..=32,
            x in any::<u32>(),
            suffix in any::<u128>(),
        ) {
            let cidr = Cidr::parse(&format!("{}/{p}", Ipv4Addr::from(a))).unwrap();
            let x = Ipv4Addr::from(x);
            let holds = cidr.contains(IpAddr::V4(x));
            for ip in carriers(x, suffix) {
                prop_assert_eq!(cidr.contains_or_embeds(ip), holds);
                prop_assert!(!cidr.contains(ip));
            }
        }
    }
}
