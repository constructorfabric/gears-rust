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

`external_docs` is also accepted as `externalDocs`, which is how the `OpenAPI`
specification spells it and therefore how it arrives when copied from a document
that already exists. `external_docs.url` must be an `http` or `https` url. A documentation browser
renders it as a link on the `/docs` page, which is served from the gateway's own
origin, so a `javascript:` or `data:` url there would be a script one click
away. Extension values are checked the same way every other string here is — no
control or direction-override characters anywhere inside them — and bounded: at
most 32 members on one group, 4096 bytes per value once serialised, eight levels
of nesting, and 100 characters for an `x-*` member name. The whole `tags` list
is bounded too, at 256 KiB serialised: the per-field caps multiply, and 200
groups each at their individual limits is tens of megabytes re-serialised on
every anonymous request.

`name` is matched against an operation's tag by **exact string equality** — no
trimming, no case folding. `Orders` and `orders` are two different groups, and
one of them will be empty; a declared group that matches no operation is logged
at startup for exactly this reason. Names must also be unique, and a blank or
duplicated name fails `init` rather than reaching the served document.

### What `init` now refuses

`title`, `version` and `description` are checked beside the groups, and **this
is new**: all three reach `/openapi.json` on an anonymous route and the `/docs`
page in a browser, and `title` and `version` are read by every generated client.
They must be non-blank, at most 200 characters — 4000 for `description` — and
free of control and direction-override characters, which a folded YAML `title:`
spanning two lines is not.

So an assembly that boots today with `openapi.title: ""`, or with a title folded
across lines, **stops booting after this change** — provided it serves
documentation at all. The check is gated on `enable_docs`: with docs off no
document is built and none is served, so there is nothing for it to protect and
`init` does not refuse the config. With docs on, `init` fails naming the field,
rather than serving an `info` block no generated client can use. Setting a title
and a version is the fix.

What is *not* refused is worth stating, because the rule differs by field. A
name — a tag group's, an `x-*` member's — and a url are matched or resolved
character for character, so a character that renders as nothing is a forgery
there and is refused. A `title`, a `version` and any `description` are text for
a reader: emoji, and the zero-width joiner that holds a single emoji together,
and the zero-width non-joiner that Persian, Hindi and Bengali are spelled with,
are all ordinary content and are accepted. Characters that reorder what a reader
sees, or end a line for whatever consumes the document, are refused in every
field regardless.

A declared group is published whether or not anything fills it yet, and
`/openapi.json` and `/docs` are served without authentication. So staging a
group ahead of the feature it will describe announces that feature: its name,
its description and its `external_docs` are readable by anyone who can reach
the gateway, before a single endpoint exists. That is deliberate — a group that
vanished from the document because nothing filled it yet would be exactly the
silent surprise this key exists to remove — but it is a choice worth making on
purpose rather than discovering. Declare the group when you are ready to talk
about it.

Omit the key and the document carries no `tags` list, which is what it did
before the key existed: documentation browsers then fall back to the order the
tags first appear in, which in an assembly is whatever the path alphabet
produced. A tag an operation uses but the list omits is not hidden — it is
emitted after the ones named here, so the list only has to name the groups whose
placement matters.

## License

Licensed under Apache-2.0.
