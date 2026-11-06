---
title: Use existing gears
description: Add ready-made system and service gears to your application and call them through their SDK traits.
sidebar:
  label: Use existing gears
  order: 3
---

Before writing your own gear, compose the ones that already ship. A Gears application is assembled from gears you select; the runtime discovers them at link time and wires them in dependency order.

## Pick the gears you need

- **Core gears** provide the control plane — API gateway, authn/authz, tenancy, type registry. Most applications include these. See [Core gears](../../capabilities/core-gears/).
- **Reusable service gears** provide business capabilities you can drop in — file parsing, credentials, chat, settings. See [Reusable service gears](../../capabilities/service-gears/).

## Add a gear to your application

A gear becomes part of your app when its crate is a dependency and it is registered with the runtime. With the CLI, this is manifest-driven:

```sh
cargo gears config mod add <gear-name> -c config/app1-dev.yml
```

The runtime resolves each gear's declared `deps`, runs migrations for gears with the `db` capability, and initializes everything in dependency order — there is no central switchboard to edit. See [Runtime and lifecycle](../../concepts/runtime-and-lifecycle/).

## Call a gear through its SDK

Consumers depend only on a gear's **SDK trait**, never its internals. Resolve the trait from the typed `ClientHub` and call it:

```rust
// Resolve a gear's public client by its SDK trait
let users = ctx.client_hub().get::<dyn UsersInfoClientV1>()?;
let user = users.get_user(ctx, id).await?;
```

Whether the registered implementation is a local in-process adapter or a gRPC client (out-of-process) is decided by configuration — the calling code is identical. See [SDK contracts and ClientHub](../../concepts/sdk-and-clienthub/).

## Configure a gear

Each gear reads typed configuration under `gears.<name>` in your runtime config. See [Configure a Gears application](../configure/) for the config model and [Run a gear out-of-process](../out-of-process/) to move a gear to its own process.

## See also

- [Build your first gear](../your-first-gear/) — when you need a capability that doesn't exist yet.
- [Integrate into an existing platform](../integrate-existing-platform/) — replace or extend gears with plugins and adapters.
