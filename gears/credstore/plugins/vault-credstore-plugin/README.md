# Vault `CredStore` Plugin

A `CredStorePluginClientV2` backend that stores secret bytes in a
[HashiCorp Vault](https://www.vaultproject.io/) or
[`OpenBao`](https://openbao.org/) KV v2 secrets engine, over Vault's HTTP API.

This is a **prototype**: it is meant for local development and demos
against a single-node `vault server -dev` / `openbao server -dev`
instance, not for production use. See Limitations below.

## Backend key shape

Per `credstore_sdk::plugin_api`, the plugin is a versioned value store keyed by
`StoreKey { tenant_id, record_id }` (ADR-0006). All versions of a record live
under one KV v2 path:

```text
{mount}/data/{path_prefix}/{tenant_id}/{record_id}
```

- `put` issues `POST {address}/v1/{mount}/data/{path_prefix}/{tenant_id}/{record_id}`
  with body `{"data":{"value":"<base64>"}}` - **no `cas`** - and returns the
  KV v2 `data.version` as the value version (a decimal string; KV v2 versions
  are ordered integers).
- `get` issues `GET .../data/...?version=N`. A `200` yields the base64-decoded
  `data.data.value`; a `404` (missing, deleted or destroyed version) maps to
  `Ok(None)`.
- `delete_key` issues `DELETE {address}/v1/{mount}/metadata/{path_prefix}/{tenant_id}/{record_id}`,
  which removes the key with all its versions. `204` and `404` both map to
  `Ok(())` (idempotent).
- `supports_destroy` is `true`. `destroy(Below(N))` reads
  `GET .../metadata/...`, takes the versions that are not yet destroyed and
  are older than `N`, and issues `POST {address}/v1/{mount}/destroy/{path_prefix}/{tenant_id}/{record_id}`
  with `{"versions":[...]}`; `destroy(Exactly(N))` posts `[N]` directly. An
  empty selection makes no destroy call; a `404` is success (idempotent).

## Mount requirements

The plugin does not rely on KV v2 retention. The operator **MUST** configure
the mount with `max_versions = 0` and `delete_version_after = 0s` (the store
must never evict a version the gear references on its own) and
`cas_required = false` (the plugin writes without `cas`; the gear's PG
compare-and-set decides the winner). These are operator obligations: the
plugin does not read `{mount}/config` and does not verify them at startup or
later. A per-path metadata override of `max_versions` is likewise the
operator's responsibility.

## Configuration

```yaml
gears:
  vault-credstore-plugin:
    config:
      vendor: "openbao"          # default; selects this plugin as the credstore backend
      priority: 100               # default
      address: "http://127.0.0.1:8200"
      token: "${VAULT_TOKEN}"     # ${VAR} is expanded from the process environment
      mount: "secret"             # default
      path_prefix: "credstore"    # default
      namespace: null              # optional; sent as X-Vault-Namespace when set
      timeout_secs: 5              # default
```

The token is never logged and never appears in `Debug` output or error
messages.

## Running locally against `OpenBao`

`token` accepts `"${VAULT_TOKEN}"` (expanded from the environment) for
real deployments; the demo config below sets it to `OpenBao`'s dev-mode
root token, `"root"`, which is only suitable for this local setup.

Start a dev `OpenBao` server:

```bash
docker run -d --name credstore-openbao -p 8200:8200 \
  -e BAO_DEV_ROOT_TOKEN_ID=root -e BAO_DEV_LISTEN_ADDRESS=0.0.0.0:8200 \
  --cap-add=IPC_LOCK openbao/openbao:latest server -dev
```

Run the example server against it, using
`config/credstore-vault-demo.yaml` (its `credstore.config.vendor` and
`vault-credstore-plugin.config` select this plugin):

```bash
cargo run -p cf-gears-example-server \
  --features vault-credstore,static-tenants,static-authn,static-authz \
  -- --config config/credstore-vault-demo.yaml run
```

The demo config sets `auth_disabled: true`, so calls need no
`Authorization` header. Create a credential and read its secret back:

```bash
curl -s -X PUT "http://127.0.0.1:8087/cf/credstore/v1/credentials/hello-world" \
  -H 'Content-Type: application/json' \
  -H 'If-None-Match: *' \
  -d '{"type": "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~", "sharing": "tenant", "secret": "hello-from-openbao"}'

curl -si "http://127.0.0.1:8087/cf/credstore/v1/credentials/hello-world?\$select=reference,secret"
```

To see the value stored directly in `OpenBao` (base64-encoded, at the KV v2
path this plugin writes to):

```bash
curl -s -X LIST -H "X-Vault-Token: root" "http://127.0.0.1:8200/v1/secret/metadata/credstore/"
```

List a level deeper, with the tenant id from the LIST output above, to get
the `record_id`, then read the current version directly:

```bash
curl -s -H "X-Vault-Token: root" \
  "http://127.0.0.1:8200/v1/secret/data/credstore/<tenant_id>/<record_id>" \
  | jq -r '.data.data.value' | base64 -d
```

## Limitations

This plugin is a prototype:

- No TLS configuration knobs (CA bundle, client certificate) — TLS is
  whatever `reqwest`'s defaults give you.
- Only static-token auth: no Kubernetes or `AppRole` auth methods, and no
  token renewal/lease management.
- No retries beyond `reqwest`'s defaults.
- One active plugin per gear: `credstore` discovers exactly one backend
  plugin by `vendor`, so this plugin and `static-credstore-plugin` cannot
  both back the same `credstore` instance at once.
