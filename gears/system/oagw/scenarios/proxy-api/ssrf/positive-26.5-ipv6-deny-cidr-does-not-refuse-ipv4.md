# IPv6 deny CIDR does not refuse an IPv4 destination

`ssrf_policy.extra_deny_cidrs` and `ssrf_policy.allow_cidrs` match an IPv6 CIDR only against IPv6 addresses other than IPv4-mapped ones. An IPv4-mapped address (`::ffff:a.b.c.d`) is matched as its IPv4 address, and an IPv6 CIDR inside `::ffff:0:0/96` (prefix 96 or longer) as the IPv4 CIDR it maps. An IPv4 CIDR in `extra_deny_cidrs` also denies the NAT64, 6to4, IPv4-compatible and IPv4-translated forms of the addresses it covers; in `allow_cidrs` it exempts none of them.

## Setup

Gear configuration:

```yaml
oagw:
  config:
    ssrf_policy:
      extra_deny_cidrs: ["::/3"]
```

## Request

Create an upstream at a public IPv4 address, then proxy a request through it:

```http
POST /api/oagw/v1/upstreams HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": { "endpoints": [{ "scheme": "https", "host": "8.8.8.8", "port": 443 }] },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "alias": "public-v4"
}
```

## Expected

- `201 Created`: `::/3` covers no IPv4 address, `::ffff:8.8.8.8` included.
- The proxy connects to `8.8.8.8`.
- An IPv6 address in `::/3` is still refused, `blocked by extra_deny_cidrs`, including one the built-in list allows: `64:ff9b::808:808`, the NAT64 form of `8.8.8.8`.

Likewise `allow_cidrs: ["::/0"]` exempts every IPv6 address but no IPv4 one: `10.0.0.1` is still refused as `private address (10.0.0.0/8)`. `extra_deny_cidrs: ["::ffff:10.0.0.0/104"]` is the same as `["10.0.0.0/8"]`. At startup the gear logs a warning for an IPv6 entry that contains `::ffff:0:0/96` (`::/0`, `::/3`, `::/80`), which earlier releases matched against every IPv4 address; `fc00::/7` is not flagged.

The E2E environment has no public IPv4 destination to connect to, so this scenario is covered by Rust tests only (`an_ipv6_deny_cidr_does_not_refuse_ipv4`, `an_ipv6_allow_cidr_does_not_admit_ipv4`, `an_ipv4_mapped_cidr_is_its_ipv4_cidr`, `an_ipv4_deny_cidr_refuses_the_ipv6_forms_that_carry_the_address`, `an_ipv4_allow_cidr_does_not_exempt_the_ipv6_forms_that_carry_the_address`, `an_ipv6_cidr_covering_the_ipv4_mapped_block_is_flagged` and the property tests under "CIDR family invariants" in `domain/ssrf.rs`).
