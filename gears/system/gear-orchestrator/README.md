# Gear Orchestrator Gear

System gear for service discovery.

## Overview

The `cf-gears-ochestrator` crate implements the `gear_orchestrator` gear.

It:

- Registers `DirectoryClient` in `ClientHub` for in-process gears
- Exposes the `DirectoryService` gRPC service (via `grpc-hub`)
- Uses the runtime `GearManager` for instance tracking and service resolution

## Registration authorization

`RegisterInstance` / `DeregisterInstance` / `Heartbeat` authorize the
authenticated platform-plane peer against the `gear_name` it claims. Per-gear
binding applies only to authenticated per-gear identities (SA-per-gear /
SPIFFE), which may act only on their own gear (or a `trusted_registrars` entry).
A shared-secret identity (`PlatformIdentity::Shared`) resolves *every* caller to
one configured label, so its label is routed through the same check: it may act
only on the gear whose name equals that label, or on any gear when the label is
listed in `trusted_registrars`. This keeps the cross-gear grant explicit in
config rather than authorizing any token holder for any gear. See the
`DirectoryServiceImpl` docs and `cpt-cf-adr-platform-plane-auth` for the
mechanism.

A request with no `PlatformSecurityContext` is handled per the listener's auth
posture, which rides the request itself: the platform-plane enforcement layer
(`grpc-hub`'s `InternalAuthGrpcLayer`) stamps a `PlatformAuthEnforced` marker on
every non-exempt request whenever enforcement is active. There is no orchestrator
knob to keep in sync, and it never reads another gear's config:

- **Marker present** (the hub enforces platform auth and lets an anonymous caller
  through under `Permissive`) — the token-less request is rejected
  (`unauthenticated`). This prevents a `Permissive` listener from *inverting* the
  incentive, where an anonymous caller (no token, so no stamped identity) could
  act on any gear while an honest per-gear token holder is bound to its own.
- **Marker absent** (Profile 1 / in-process, enforcement disabled) —
  authorization is skipped (fails open); the process boundary is the trust root.

### `trusted_registrars`

Peers allowed to register on behalf of *other* gears (e.g. a central registrar
whose `ServiceAccount` name differs from the gears it manages). Empty by
default — with SA-per-gear / SPIFFE a gear registers under its own name.

A shared-secret deployment (`internal_auth.provider: shared_secret`) resolves
all callers to the configured `peer_name`; list that name here so those peers can
register the gears they front (otherwise, with an empty list, a shared-secret
peer can only register a gear whose name equals its label).

### `platform_namespaces` / `trust_domains`

The peer name alone is not sufficient to authorize a per-gear identity: the
token authenticator has no namespace/trust-domain allowlist, so a `billing`
`ServiceAccount` in *any* namespace (or a `billing` workload from *any* SPIFFE
trust domain) would otherwise be authorized for gear `billing`. Configure the
platform-controlled Kubernetes namespaces (`platform_namespaces`) and/or SPIFFE
trust domains (`trust_domains`) that per-gear identities must belong to; a peer
whose qualifier is not in the allowlist is rejected even if its name matches the
gear. Gears may be spread across several listed namespaces.

Both are empty by default, which **disables** the respective qualifier check and
silently falls back to unqualified-name authorization. Configure them in any
deployment where untrusted workloads can mint tokens.

### `grpc_service_owners`

Pins which gear may advertise a given gRPC service name, so ownership is decided
by config rather than by whichever gear self-registers first. This is only needed
for gears the host binary does **not** compile in (remote / out-of-process):
compiled-in gears are pinned automatically from the compiled registry (and that
authoritative mapping overrides any conflicting entry here). Keys are validated at
startup against the same rule that advertised names must satisfy, so a malformed pin
fails loudly rather than silently matching nothing. Empty by default; names absent
everywhere keep first-registration ownership.

```yaml
gears:
  gear-orchestrator:
    config:
      trusted_registrars:
        - flight-control
      platform_namespaces:
        - platform-system
        - platform-gears
      trust_domains:
        - platform.example.org
      grpc_service_owners:
        # only for gears not compiled into this binary
        remote.pkg.v1.RemoteService: remote-gear
```

## License

Licensed under Apache-2.0.
