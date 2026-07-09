---
title: Core concepts
description: The mental model behind Gears — composition, runtime, security, and the type system.
sidebar:
  label: Overview
  order: 1
---

These pages are the mental model you need before building. They are grouped into four
areas; read them in order, or jump to what you need.

- **[Gears & composition](./gears-and-composition/)** — what a gear is, the
  three-tier hierarchy, the SDK facade+backend pattern, and how gears find each other
  through `ClientHub`.
- **[Runtime & lifecycle](./runtime-and-lifecycle/)** — capabilities, the ordered
  lifecycle the runtime drives every gear through, async boundaries, and the (planned)
  cluster plane.
- **[Security & multi-tenancy](./security-and-tenancy/)** — the secure-by-default
  data path: `SecurityContext`, PDP/PEP authorization, `AccessScope`, `SecureConn`, and the
  tenant tree.
- **[Errors & the type system](./errors-and-types/)** — the canonical error model
  (RFC-9457) and the Global Type System (GTS).

When you're ready to apply them, the [first-gear walkthrough](../get-started/your-first-gear/)
ties everything together against real code, and the [Guides](../guides/) go deep on individual
features.
