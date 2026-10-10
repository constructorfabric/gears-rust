# Construct SDK

SDK crate for the Construct gear.

## Overview

The `cf-gears-construct-sdk` crate provides:

- `ConstructClientV1` trait — the client other gears resolve from `ClientHub`
- Model types (`FoundationNote`, `NewFoundationNote`)
- The person types (`person_types`) — one GTS graph node type per category of a person's profile

```rust,ignore
use construct_sdk::ConstructClientV1;

let client = hub.get::<dyn ConstructClientV1>()?;
let note = client.get_note(&ctx, id).await?;
```

## Person types

A person's profile has four categories: identity, roles, skills and preferences. Each is a graph node type that
derives from graph storage's owned node:

`gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.<category>.v1~`

One node is one fact. Its `payload` holds one fact property, which names the fact, and `origin`, graph storage's
provenance attribute. The payload is closed: an unknown property is refused, and a new property is a
backward-compatible change. `PROPERTIES` and `cardinality` say whether a subject holds one value of a property or
many.

The schemas live in `schemas/` and are embedded in `person_types::PERSON_TYPES` for registration with the types
registry and graph storage. The design is in [the Profile Writer feature](../docs/features/profile-writer.md).
