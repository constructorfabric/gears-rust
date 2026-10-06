# Construct SDK

SDK crate for the Construct gear.

## Overview

The `cf-gears-construct-sdk` crate provides:

- `ConstructClientV1` trait — the client other gears resolve from `ClientHub`
- Model types (`RecordOutcome`)
- The connector record types Construct takes at intake (`gts` module): the
  abstract base `gts.cf.connectors.core.record.v1~` and its derived types,
  registered in the types registry at link time. The schema files under
  `schemas/` are copies of the connector boundary schemas in rolos-cyber
  (`docs/construct/GTS/schemas/`), which stay the source of truth.
- The person types (`person_types`) — one GTS graph node type per category of a person's profile

```rust,ignore
use construct_sdk::ConstructClientV1;

let client = hub.get::<dyn ConstructClientV1>()?;
let outcome = client.submit_record(&ctx, tenant_id, record).await?;
```

## Person types

A person's profile has four categories: identity, roles, skills and preferences. Each is a graph node type that
derives from graph storage's owned node:

`gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.<category>.v1~`

A node's `payload` holds exactly one property of its type, so one node is one value of one property. The payload is
closed: an unknown property is refused, and a new property is a backward-compatible change.

The schemas live in `schemas/` and are embedded in `person_types::PERSON_TYPES` for registration with the types
registry and graph storage.
