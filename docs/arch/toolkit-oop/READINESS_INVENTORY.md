# OoP Readiness Inventory

Point-in-time sweep of every gear in the repo against out-of-process (OoP)
readiness requirements, including the gears that intentionally remain in
`platform-host`.

**Legend:** 
- 🟢 ready & committed 
- 🔴 not ready / blocking 
- ⚪ N/A (requirement doesn't apply to this gear)

**Columns:**

- **Provides** - transport surface this gear serves: `REST`, `gRPC`,
  `REST + gRPC`, or ⚪ none (in-process only, e.g. `[system]`-only gears). This
  is just *what it exposes*, independent of whether anything consumes it.
- **Contract** - *producer* side: is there a typed, cross-process contract
  (`#[toolkit::contract]` + a `#[toolkit::rest_contract]` / `#[toolkit::grpc_contract]`
  projection) that *other* gears consume? 🟢 a contract exists and
  a gear depends on it; 🔴 other gears depend
  on this one but no consumable contract exists yet (the "add a contract to
  unblock" lever); ⚪ no gear depends on it.
- **Consumes Cleanly** - *consumer* side: does this gear reach *its own*
  dependencies via `#[toolkit::consumes]` (works local or remote), rather
  than a compile-time `deps=[...]` link that forces co-location? (⚪ if the
  gear has no gear-to-gear dependencies at all)
- **OoP Binary** - has an `oop_module`-gated bin (or dedicated OoP crate)
- **DB Isolation** - DB-backed gears get their own database
- **Authn Stack** - embedded tenant-plane authn wired (⚪ if the gear is
  anonymous)
- **k8s-auth** - platform-plane TokenReview wired
- **Helm Chart** - standalone deployable chart exists
- **Notes / Staged changes** - current blocker, or other notes

## Readiness by gear

| Gear | Provides | Contract | Consumes Cleanly | OoP Binary | DB Isolation | Authn Stack | k8s-auth | Helm | Notes / Staged changes |
|---|---|---|---|---|---|---|---|---|---|
| `authz-resolver` | REST | 🟢 REST `AuthZResolverApi` | 🔴 `deps=[types_registry]` | 🔴 none | ⚪ no DB | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | Blocked by `types_registry` hard-dep and synthetic `SecurityContext::anonymous()` in its internal chain. |
| `tenant-resolver` | ⚪ none | 🔴 no contract yet | 🔴 `deps=[types_registry]` | 🔴 none | ⚪ no DB | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | `[system]`-only, exposes no surface yet; `types_registry` blocker. `rg-tr-plugin` reads `resource-group` DB directly. |
| `resource-group` | REST | 🔴 no contract yet | 🔴 `deps=[authz_resolver, types_registry]` | 🔴 none | 🟢 pg | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | Adding a REST contract unblocks `authz-resolver` + `tenant-resolver`. |
| `account-management` | REST | 🔴 no contract yet | 🔴 `deps=[authz_resolver, types_registry, resource_group, tenant_resolver]` | 🔴 none | 🟢 pg | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | No contract; synthetic `am.system` credential needs real S2S migration. |
| `gear-orchestrator` | REST + gRPC | ⚪ raw-tonic `DirectoryService`, not a `#[toolkit::contract]` | ⚪ (no deps) | 🔴 none | ⚪ no DB | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | Discovery mechanism the contract system resolves *through*; consumed via a raw `DirectoryGrpcClient`, not `#[toolkit::consumes]`. Currently in platform-host. |
| `grpc-hub` | gRPC | ⚪ transport, not an app contract | ⚪ (no deps) | 🔴 none | ⚪ no DB | ⚪ | 🟢 | ⚪ (bundled in platform-host chart) | Plumbing; currently in platform-host. |
| `api-gateway` | REST | ⚪ edge; nothing consumes it | 🔴 `deps=[grpc_hub, authn_resolver]` | 🔴 none | ⚪ no DB | ⚪ | 🟢 | ⚪ (bundled in platform-host chart) | Reverse-proxy to OoP gears; currently in platform-host. |
| `types-registry` | REST | 🔴 no contract yet | ⚪ (no deps) | 🔴 none | ⚪ no DB (link-time inventory) | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | Biggest cross-cutting blocker. |
| `credstore` | REST | 🔴 no contract yet | 🔴 `deps=[authz_resolver, tenant_resolver, types_registry]` | 🔴 none | 🟢 pg | ⚪ | 🔴 | ⚪ (bundled in platform-host chart) | No contract; blocks `oagw`. |
| `authn-resolver` | ⚪ none | ⚪ embedded per OoP pod, not consumed via `ClientHub` | 🔴 `deps=[types_registry]` | ⚪ (in every OoP binary) | ⚪ no DB | ⚪ | ⚪ (inherits host pod's) | ⚪ | Embedded by design - intentionally never extracted; every OoP pod embeds its own copy. |
| `hello` | REST | ⚪ no gears depend on it | ⚪ (no deps) | 🟢 | ⚪ | ⚪ | 🟢 | 🟢 | New minimal reference gear. |
| `users-info` | REST | ⚪ no gears depend on it | 🟢 consumes `AuthZResolverApi` via `#[toolkit::consumes]` | 🟢 | 🟢 pg | 🟢 | 🟢 | 🟢 | OoP binary, own database on shared Postgres, routes `.exposed()`, verified e2e through the edge. |
| `api-contracts` | REST | 🟢 REST `PaymentApi`/`PaymentApiV2` | ⚪ (no deps) | 🟢 | ⚪ | 🟢 | 🟢 | 🟢 | OoP<->OoP REST reference pair. |
| `api-contracts-consumer` | REST | ⚪ no gears depend on it | 🟢 consumes `PaymentApi`/`PaymentApiV2` | 🟢 | ⚪ | 🟢 | 🟢 | 🟢 | OoP binary + Helm. |
| `simple-user-settings` | REST | ⚪ no gears depend on it | 🔴 `deps=[authz_resolver]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | In-process gear; hard `deps=[authz_resolver]` PEP link. No OoP binary, Helm chart, or platform-plane wiring. |
| `file-storage` | REST | ⚪ no gears depend on it | 🔴 `deps=[authz_resolver]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | In-process gear; hard `deps=[authz_resolver]` PEP link. No OoP binary, Helm chart, or platform-plane wiring. |
| `chat-engine` | REST | ⚪ no gears depend on it | 🔴 `deps=[authz_resolver]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | In-process gear; hard `deps=[authz_resolver]` PEP link. Has its own separate `k8s` (leader-election) feature unrelated to `k8s-auth`. No OoP binary, Helm chart, or platform-plane wiring. |
| `usage-collector` | REST | ⚪ no gears depend on it | 🔴 `deps=[types_registry, authz_resolver]` | 🔴 none | ⚪ (plugin-owned storage) | 🔴 | 🔴 | 🔴 | Hard `deps=[types_registry, authz_resolver]`; no OoP binary, Helm chart, or platform-plane wiring. |
| `oagw` | REST | 🔴 no contract yet | 🔴 `deps=[types_registry, authz_resolver, credstore, tenant_resolver]` | 🔴 none | ⚪ | 🔴 | 🔴 | 🔴 | No contract; many hard deps. |
| `mini-chat` | REST | ⚪ no gears depend on it | 🔴 `deps=[types_registry, authn_resolver, authz_resolver, oagw]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | Blocked by `oagw`. |
| `bss-ledger` | REST | ⚪ no gears depend on it | 🔴 `deps=[types_registry, authz_resolver, account_management]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | Hard-deps on `account_management` (itself blocked by a real S2S credential migration). |
| `bss-pricing` | REST | ⚪ no gears depend on it | 🔴 `deps=[types_registry, authz_resolver]` | 🔴 none | 🔴 pg-capable | 🔴 | 🔴 | 🔴 | Hard-deps on `types-registry` + `authz-resolver`; no OoP binary, Helm chart, or platform-plane wiring. |
| `bss-rate-provider` (+`ecb`/`http-json` plugins) | ⚪ none (internal only) | ⚪ no gears depend on it | 🔴 `deps=[types_registry]` | 🔴 none | ⚪ | ⚪ anonymous | ⚪ | 🔴 | Simplest remaining blocker - only `types-registry`. |
| `file-parser` | REST | ⚪ no gears depend on it | ⚪ (no deps) | 🔴 none | ⚪ | 🔴 | 🔴 | 🔴 | No hard deps; otherwise a `hello`-shape candidate. |
| `nodes-registry` | REST | ⚪ no gears depend on it | ⚪ (no deps) | 🔴 none | ⚪ | ⚪ anonymous | 🔴 | 🔴 | Plain REST, not yet a `#[toolkit::consumes]`-wireable contract. No blockers to convert. |
| `event-broker` | REST | ⚪ no gears depend on it | 🔴 hard Rust crate dep on `cluster` | 🔴 none | ⚪ | 🔴 | 🔴 | 🔴 | `cluster` already exposes gRPC contracts; the real blocker is event-broker resolving `cluster-sdk` facades from `ClientHub` in-process (with `deps=[cluster]`) and no remote resolver wired yet. |
| `cluster` | REST + gRPC | 🟢 gRPC `ClusterCacheApi`/`DistributedLockApi`/`LeaderElectionApi`/`ClusterProfileApi` | ⚪ (no deps) | 🟢 | ⚪ | ⚪ | 🟢 | 🔴 | Four `#[toolkit::grpc_contract]` surfaces; committed `cluster-oop` binary + `k8s-auth` wiring. Still missing a standalone Helm chart. |

> Other gear-shaped directories exist (`approval-service`,
> `infrastructure-resource-manager`, `llm-gateway`, `model-registry`,
> `serverless-runtime`, `settings-service`, `simple-resource-registry`, plus
> `bss/{products,subscriptions,rating}`), but none declare a `#[toolkit::gear]`
> gear yet, so they are outside the scope of this readiness inventory.

## Plugins - always embedded, inherit host's status

`static-authn-plugin`, `oidc-authn-plugin`, `static-authz-plugin`,
`tr-authz-plugin`, `static-tr-plugin`, `single-tenant-tr-plugin`,
`rg-tr-plugin`, `static-credstore-plugin`, `static-idp-plugin`,
`keycloak-idp-plugin`, `noop-usage-collector-plugin`,
`timescaledb-usage-collector-plugin`, `static-mini-chat-audit-plugin`,
`static-mini-chat-model-policy-plugin` - all hard-dep `types_registry` and
ride inside whatever process hosts their parent gear. Not independently
assessable.

## Key takeaways

1. **Single biggest lever:** give `resource-group` a REST contract →
   unblocks `authz-resolver` and `tenant-resolver` (3 of the 4 authz/tenant
   core gears) at once.
2. **Second biggest lever:** `types-registry` `#[toolkit::consumes]` wiring →
   unblocks `usage-collector`'s residual hard-dep, `bss-rate-provider`, and is
   a prerequisite for `mini-chat`.
3. **Easiest untouched wins:** `nodes-registry` (zero deps, zero auth) and
   `file-parser` (zero deps, just needs the authn-stack pattern already
   proven 6 times) - both far easier than `oagw`/`mini-chat`/`bss-ledger`.
4. **Structurally stuck, not just "not started":** `event-broker` links the
   `cluster` crate directly in Rust (resolving `cluster-sdk` facades from
   `ClientHub`, plus `deps=[cluster]`). `cluster` already exposes gRPC
   contracts and is itself OoP-deployable (`cluster-oop` binary; only missing a
   Helm chart) — the work is wiring event-broker onto those contracts remotely,
   an architecture change rather than a mechanical `deps→consumes` swap.
5. **`account-management`'s credential migration** (`am.system` → real S2S)
   is the only blocker in the whole inventory that isn't solvable by "add a
   REST contract" - it needs an actual identity design decision.
