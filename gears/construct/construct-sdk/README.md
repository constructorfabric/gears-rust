# Construct SDK

SDK crate for the Construct gear.

## Overview

The `cf-gears-construct-sdk` crate provides:

- `ConstructClientV1` trait — the client other gears resolve from `ClientHub`
- Model types (`RecordOutcome`)
- The connector record types Construct takes at intake (`gts` module): the
  abstract base `gts.cf.connectors.core.record.v1~` and its derived types,
  defined as Rust structs with `#[gts_type_schema]`, which generates their
  schemas and registers them in the types registry at link time. Whether a
  type requires `subject_id` is its GTS trait `subject_required`
  (`gts::RecordTraits`), on unless the type turns it off. The generated record
  schemas are committed under `schemas/`; a test fails when they drift (run it
  with `UPDATE_SCHEMAS=1` after an intended change). The rules a generated schema
  cannot carry are in `gts::check_type_rules`; the fact candidate's
  `provenance` and `version` are derived as `docs/fact-candidate-identity.md`
  specifies.
- The person types (`person_types`) — one GTS graph node type per category of a person's profile

```rust,ignore
use construct_sdk::ConstructClientV1;

let client = hub.get::<dyn ConstructClientV1>()?;
let outcome = client.submit_record(&ctx, tenant_id, record).await?;
```

## Person types

A person's profile has four categories: identity, role, skill and preference. Each is a graph node type that
derives from graph storage's owned node:

`gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.person.<category>.v1~`

One node is one fact. Its `payload` holds one fact property, which names the fact, and `origin`, graph storage's
provenance attribute. It also holds `record`, the connector and the record's identity and version, exactly when the
fact came from a record. The payload is closed: an unknown property is refused, and a new property is a
backward-compatible change. `PROPERTIES` says whether a subject holds one value of a property or many. The type ids
are in `gts`.

The schemas live in `schemas/` and are embedded in `person_types::PERSON_TYPES` for registration with graph storage,
which keeps its own type catalogue and validates every node against it. The design is in [the Profile Writer feature](../docs/features/profile-writer.md).
