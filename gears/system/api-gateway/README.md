# API Gateway Gear

HTTP gateway gear that owns the Axum router and collects typed operation specs to emit a single OpenAPI document.

## Overview

The `cf-gears-api-gateway` crate provides:

- HTTP server host for REST APIs
- Operation registration via `toolkit::api::OperationBuilder`
- OpenAPI document aggregation

## Configuration

```yaml
gears:
  api_gateway:
    config:
      bind_addr: "127.0.0.1:8086"
      enable_docs: true
      cors_enabled: false
      auth_disabled: false
```

### Ordering the documentation groups

An operation declares the tag it belongs to; `openapi.tags` declares the groups
those tags name — the order a reader meets them in, and what each one is for.
This is the assembly's call rather than any single gear's: an assembly links
gears it does not own, so the tags in its document come from several authors and
none of them can decide the order.

```yaml
gears:
  api_gateway:
    config:
      enable_docs: true
      openapi:
        title: "Example Assembly"
        version: "0.1.0"
        tags:
          - name: Orders
            description: "Placing an order, and everything that happens to it after."
            external_docs:
              url: "https://example.test/docs/orders"
              description: "The long version."
            extensions:
              x-displayName: "Orders"
          - name: Tenants
```

`external_docs` and `extensions` are optional. `extensions` is a nested map
rather than `x-*` members written beside `name`, because the config rejects
unknown keys — that is what turns a mistyped `descrption:` into a startup error
instead of a group that quietly lost its description — and serde cannot combine
that with a flattened catch-all. Extension names must start with `x-`, since
every other member of an `OpenAPI` object belongs to the specification.

`name` is matched against an operation's tag by **exact string equality** — no
trimming, no case folding. `Orders` and `orders` are two different groups, and
one of them will be empty; a declared group that matches no operation is logged
at startup for exactly this reason. Names must also be unique, and a blank or
duplicated name fails `init` rather than reaching the served document.

Omit the key and the document carries no `tags` list, which is what it did
before the key existed: documentation browsers then fall back to the order the
tags first appear in, which in an assembly is whatever the path alphabet
produced. A tag an operation uses but the list omits is not hidden — it is
emitted after the ones named here, so the list only has to name the groups whose
placement matters.

## License

Licensed under Apache-2.0.
