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
- The profile schema (`linked_schema`) — the node kinds and link kinds of a person's profile, one GTS graph type each

```rust,ignore
use construct_sdk::ConstructClientV1;

let client = hub.get::<dyn ConstructClientV1>()?;
let outcome = client.submit_record(&ctx, tenant_id, record).await?;
```

## The profile schema

A person's profile is a graph of nodes and links, defined by a schema of node kinds and link kinds
(`linked_schema::Schema`). The default schema, `default_schema()`, has the things a person works with (`person`,
`project`, `product`, `activity`, `team`, `customer`, `partner`, `competitor`), the entries that hang on them
(`commitment`, `decision`, `question`, `metric`), the reference nodes (`ticket`, `source`), the person's own direct
properties (`me`) and the links between them (`works_on`, `member_of`, `owner`, `came_from` and the rest). The REST API
and the SDK name a kind by its registered name, such as `project`: the `FactCategory`.

Each kind and each link kind is one graph type, which derives from graph storage's owned node or static edge. The type
ids carry a per-person prefix, `u` and the subject id as 32 hex digits, so two people's kinds never collide in one tenant:

`gts.cf.core.graph.node.v1~cf.core.graph.owned_node.v1~cf.construct.u<subject>.<kind>.v1~`

One node is one value of its kind. Its `payload` holds one property, named like the kind, whose value is the object of the
node's fields, and `origin`, graph storage's provenance attribute. It also holds `learned`, the tool and the message of
each write that touched it, `owner`, the key of the node that owns it, for an entry kind, and `record`, the connector and
the record's identity and version, exactly when the node came from a record. A link is an edge whose payload holds its
`fields`, `origin` and `record`. The payloads are closed: an unknown property is refused.

The kinds are built in Rust and registered with graph storage per person: `Schema::graph_types(&SchemaPrefix)`. The root
and the has-fact edge, which every profile shares, are in `profile_types`. The design is in [the Profile Writer feature](../docs/features/profile-writer.md).
