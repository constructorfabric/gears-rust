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

```rust,ignore
use construct_sdk::ConstructClientV1;

let client = hub.get::<dyn ConstructClientV1>()?;
let outcome = client.submit_record(&ctx, tenant_id, record).await?;
```
