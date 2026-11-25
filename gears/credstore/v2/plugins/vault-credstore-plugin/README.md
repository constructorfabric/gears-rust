Created:  2026-09-23 by Constructor Tech
Updated:  2026-10-06 by Constructor Tech

# Vault `CredStore` Plugin

A `CredStorePluginClientV2` backend that stores secret bytes in a
[HashiCorp Vault](https://www.vaultproject.io/) or
[`OpenBao`](https://openbao.org/) KV v2 secrets engine, over Vault's HTTP API.
It is the reference implementation of the backend contract of
[ADR-0006](../../../docs/ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md):
every `put` creates a new immutable KV v2 version, `get` reads exactly one
version, `delete_key` removes a key with all its versions, and `destroy`
removes older or single versions.

Documentation: [PRD](docs/PRD.md), [DESIGN](docs/DESIGN.md) (the KV v2
mapping, authentication, mount requirements, ACL policy, configuration
reference, failure modes) and [TESTING](docs/TESTING.md).

## Backend key shape

All versions of a record live under one KV v2 path:

```text
{mount}/data/{path_prefix}/{tenant_id}/{record_id}
```

| Operation | Vault call | Notes |
|-----------|------------|-------|
| `put` | `POST /v1/{mount}/metadata/...` `{"max_versions":100}`, then `POST /v1/{mount}/data/...` `{"data":{"value":"<base64>"}}` | The first call sets the key's own `max_versions` (idempotent, allowed before the first write); if it fails, the `put` fails and nothing is written. No `cas`. Returns `data.version`. Neither call is retried. |
| `get(version)` | `GET /v1/{mount}/data/...?version=N` | `404` (absent, soft-deleted, destroyed or evicted) is `None`. |
| `delete_key` | `DELETE /v1/{mount}/metadata/...` | Removes all versions. `404` is success. |
| `destroy(Exactly(N))` | `POST /v1/{mount}/destroy/...` `{"versions":[N]}` | |
| `destroy(Below(N))` | `GET /v1/{mount}/metadata/...`, then `POST /v1/{mount}/destroy/...` | Destroys the versions below `N` that are not destroyed yet; no call when there are none. |

`get`, `delete_key` and `destroy` are retried with bounded exponential backoff
on transient failures; `put` (both of its calls) is sent at most once.

## Authentication

A Vault token, from exactly one of: `token` (inline, `${VAR}` is expanded),
`token_file` (for example the sink of a Vault Agent sidecar) or `token_env`
(the name of an environment variable). When Vault answers `403` the token
source is re-read once and the request is retried once with the new token, so
a rotated `token_file` is picked up without a restart. There is no login
method, no renewal loop and no background task. The token is never logged and
never appears in `Debug` output or error messages.

## Mount requirements

The plugin does not check the mount, at startup or later. The operator
provides a **KV v2** mount with:

- `delete_version_after = 0s` (versions never expire by age);
- `cas_required = false` (the plugin writes without `cas`; the gear's
  `PostgreSQL` compare-and-set decides the winner).

The mount's `max_versions` is not an operator obligation: before every `put`
the plugin sets the key's own `max_versions` to 100. Vault keeps the larger of
the mount's and the key's limit (`OpenBao` lets a non-zero key value win; 10 when both are `0` or unset; `0` is not
unlimited) and counts destroyed versions, so a lower value on the mount is
harmless. Superseded versions are destroyed after each commit, so a key
normally holds one or two. If more than 100 versions pile up above a record's
pointer (orphans of failed or ambiguous writes, versions of CAS losers,
destroyed or not), Vault drops the oldest, possibly the one a record points
at, and that record answers an internal error (500) until it is rewritten.
The 100 is a constant of the plugin and is never lowered: the next write to
every existing key would remove the versions beyond the new window.

The token needs only this policy (replace `secret` and `credstore` with the
configured `mount` and `path_prefix`); `create` and `update` on the metadata
path are for the `max_versions` call of `put`:

```hcl
path "secret/data/credstore/*"     { capabilities = ["create", "update", "read"] }
path "secret/metadata/credstore/*" { capabilities = ["create", "update", "read", "delete"] }
path "secret/destroy/credstore/*"  { capabilities = ["update"] }
```

See [docs/DESIGN.md](docs/DESIGN.md) for the details.

## Configuration

```yaml
gears:
  vault-credstore-plugin-v2:
    config:
      vendor: "openbao"           # default; selects this plugin as the credstore backend
      priority: 100               # default
      address: "https://vault.internal:8200"
      # Exactly one of the three token sources:
      token_file: "/vault/secrets/token"   # re-read after a 403
      # token: "${VAULT_TOKEN}"            # inline; ${VAR} is expanded from the environment
      # token_env: "VAULT_TOKEN"           # name of the variable, read at startup
      mount: "secret"             # default; a KV v2 mount
      path_prefix: "credstore"    # default
      namespace: null             # optional; sent as X-Vault-Namespace when set
      timeout_secs: 5             # default; per request
      retry:
        max_attempts: 3           # default; 1..=10, the first attempt included
        base_delay_ms: 100        # default; 1..=2000, doubles per attempt, capped at 2 s
```

Unknown keys are rejected. The plugin has no TLS settings: HTTPS uses
`reqwest`'s defaults (the platform's trust roots).

`credstore` discovers exactly one backend plugin by `vendor`, so this plugin
and `static-credstore-plugin-v2` cannot back the same `credstore` instance at
once.

## Using the plugin outside the gear

A binary that is not a gear and has no `ClientHub` (a value-migration tool
that writes into this plugin, a diagnostic) builds the client with
`client_from_config`, a small and stable constructor re-exported at the crate
root:

```rust,no_run
use std::sync::Arc;

use credstore_sdk::CredStorePluginClientV2;
use vault_credstore_plugin_v2::client_from_config;
use vault_credstore_plugin_v2::config::VaultCredStorePluginConfig;

fn build_new_store() -> anyhow::Result<Arc<dyn CredStorePluginClientV2>> {
    // The SAME values as the running gear's `config` block (see above).
    let cfg = VaultCredStorePluginConfig {
        address: "https://vault.internal:8200".to_owned(),
        token_file: Some("/vault/secrets/token".to_owned()),
        mount: "secret".to_owned(),
        path_prefix: "credstore".to_owned(),
        ..VaultCredStorePluginConfig::default()
    };
    client_from_config(&cfg)
}
```

It validates the configuration exactly as the gear does at startup
(`VaultCredStorePluginConfig::validate`: one token source, a well-formed
address, mount and prefix, bounded timeout and retry) and returns the very
client the gear registers, with the same transport, token handling and retry
policy. It makes no request to Vault, publishes no GTS instance
and starts no gear; a `token_file` is read on the first call, not at
construction. The configuration is used as given: `${VAR}` placeholders are
not expanded here (the gear expands them while loading its `config` block), so
expand them first or build the struct in code. Use the same `address`, `mount`,
`path_prefix`, `namespace` and token as the gear: a different mount or prefix
writes versions the gear cannot read.

## Trying it locally

The plugin is not wired into the example server yet: v2 is connected to the
server at the switch (step S1 of `gears/credstore/v2/plan.md`). Until then its
behaviour is checked by the integration tests (see [Tests](#tests) and
[docs/TESTING.md](docs/TESTING.md)), which start their own Vault or `OpenBao`
dev server in Docker.

To poke at a server by hand, a dev-mode server is enough (it is **not** a
production setup: in-memory storage, a known root token):

```bash
docker run -d --name credstore-vault -p 8200:8200 \
  -e VAULT_DEV_ROOT_TOKEN_ID=root -e VAULT_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
  --cap-add=IPC_LOCK hashicorp/vault:2.1.1 server -dev
```

(`openbao/openbao` works the same way with `BAO_DEV_ROOT_TOKEN_ID` and
`BAO_DEV_LISTEN_ADDRESS`.) The dev server mounts a KV v2 engine at `secret/`.
Check that it answers:

```bash
curl -s -H "X-Vault-Token: root" "http://127.0.0.1:8200/v1/sys/health"
```

## Tests

Unit tests run with a plain `cargo test -p cf-gears-vault-credstore-plugin-v2`.
The integration tests start a `hashicorp/vault` dev server in Docker (or
`OpenBao`, see below), run the SDK conformance suite and check the Vault-specific
behaviour. They are ignored by default because they need Docker; the CI lane
`credstore-vault` runs them:

```bash
make test-integration LANE=credstore-vault
# or by hand
cargo test -p cf-gears-vault-credstore-plugin-v2 -- --ignored
```

To run them against `OpenBao`, override the image and tag:

```bash
GEARS_TEST_VAULT_IMAGE=openbao/openbao GEARS_TEST_VAULT_TAG=2.7.3 \
  cargo test -p cf-gears-vault-credstore-plugin-v2 -- --ignored
```

See [docs/TESTING.md](docs/TESTING.md).
