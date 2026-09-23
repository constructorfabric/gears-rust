# Vault `CredStore` Plugin

A `CredStorePluginClientV1` backend that stores secret bytes in a
[HashiCorp Vault](https://www.vaultproject.io/) or
[`OpenBao`](https://openbao.org/) KV v2 secrets engine, over Vault's HTTP API.

This is a **prototype**: it is meant for local development and demos
against a single-node `vault server -dev` / `openbao server -dev`
instance, not for production use. See Limitations below.

## Backend key shape

Per `credstore_sdk::plugin_api`, the plugin is a pure per-tenant,
per-version value store keyed by `(tenant_id, value_id)`. Every entry is
addressed at a fixed KV v2 path:

```text
{mount}/data/{path_prefix}/{tenant_id}/{value_id}
```

- `get` issues `GET {address}/v1/{mount}/data/{path_prefix}/{tenant_id}/{value_id}`.
  A `200` yields the base64-decoded `data.data.value` field; a `404` maps to
  `Ok(None)`.
- `put` issues `POST` with body `{"options":{"cas":0},"data":{"value":"<base64>"}}`
  — `cas: 0` means "only create if no version exists yet", which is exactly
  the immutability guarantee `CredStorePluginClientV1::put` must provide. A
  `400` whose error body mentions "check-and-set" maps to
  `CredStoreError::Conflict`.
- `delete` issues `DELETE {address}/v1/{mount}/metadata/{path_prefix}/{tenant_id}/{value_id}`,
  which removes all versions (and the key's metadata) in one call. `204` and
  `404` both map to `Ok(())` (idempotent).

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
the `value_id`, then read it directly:

```bash
curl -s -H "X-Vault-Token: root" \
  "http://127.0.0.1:8200/v1/secret/data/credstore/<tenant_id>/<value_id>" \
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
