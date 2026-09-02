# gRPC Hub Gear

This gear builds and hosts the single `tonic::Server` instance for the process.

## Overview

The `cf-gears-grpc-hub` crate implements the `grpc_hub` gear and is responsible for:

- Hosting the gRPC server
- Installing gRPC services collected from other gears

## Configuration

```yaml
gears:
  grpc_hub:
    config:
      # TCP example: "0.0.0.0:50051"
      # Unix example (unix only): "uds:///tmp/cf-gears.sock"
      # Windows named pipe example (windows only): "pipe://\\\\.\\pipe\\cf-gears"
      listen_addr: "0.0.0.0:50051"

      # Platform-plane (internal) authentication for ALL inbound gRPC RPCs
      # served by this hub, enforced by a transport-level Tower layer
      # (cpt-cf-adr-platform-plane-auth). When omitted, enforcement is
      # disabled (Profile 1 / in-process only) and every inbound RPC is
      # unauthenticated — a warning is logged at startup in that case.
      internal_auth:
        # "shared_secret": a single pre-shared token (dev / single-node).
        provider: shared_secret
        secret: "dev-internal-token"
        peer_name: "toolkit-internal" # optional, defaults shown

        # "kube": a projected ServiceAccount token, validated via the
        # Kubernetes TokenReview API. Requires this gear's `k8s-auth`
        # Cargo feature to be enabled at build time; without it,
        # `provider: kube` is a hard `init` error, not a silent downgrade.
        # provider: kube
        # audiences: ["toolkit-internal"]
        # token_path: /var/run/secrets/tokens/toolkit-internal

      # How an ABSENT credential is treated when `internal_auth` is set.
      # "required" (default) rejects every non-exempt RPC with no token;
      # "permissive" lets an absent token through (a present-but-invalid
      # token is always rejected, in either mode). Has no effect when
      # `internal_auth` is unset.
      internal_auth_enforcement: required

      # Optional override of the exempt gRPC method-path prefixes (default:
      # gRPC health-check + both reflection services). Each entry must be
      # non-empty and start with "/"; entries are matched on a method-path
      # segment boundary, so "/pkg.Svc/List" does not also exempt
      # "/pkg.Svc/ListSecrets". An empty list enforces on every method.
      # internal_auth_exempt_methods: ["/my.pkg.Svc/"]

      # Seconds a SUCCESSFUL platform-plane validation is cached, collapsing
      # a burst of calls carrying the same token into a single validation
      # round-trip (only benefits a remote backend, i.e. `kube` — the
      # shared-secret provider is a local comparison and is never cached).
      # `0` disables caching. Bounded at `init` by a maximum of 300s (5
      # minutes) to keep the token-revocation window tight; exceeding it is
      # a hard `init` error.
      internal_auth_cache_ttl_secs: 30
```

## License

Licensed under Apache-2.0.
