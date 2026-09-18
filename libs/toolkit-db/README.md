# ToolKit DB

Database abstractions for Gears / ToolKit with optional SeaORM integration.

## Overview

The `cf-gears-toolkit-db` crate provides:

- Typed database configuration / connection options
- SQLx backend support (SQLite / Postgres / MySQL via features)
- SeaORM integration
- Secure-by-default ORM wrapper (see `secure` gear)
- Per-gear migration runner (see `migration_runner` gear)

## Features

- `pg`, `mysql`, `sqlite`: enable SQLx backends

## Security Model

Gears cannot access raw database connections. All database operations go through
the `SecureConn` API which enforces tenant isolation at compile time. Migrations
are provided as definitions and executed by the runtime with a privileged connection.

## License

Licensed under Apache-2.0.

### Classifying delivery failures

With a database backend enabled, `toolkit_db::retry::{scope, database}` classify
recognized temporary storage failures for **idempotent** callers such as leased outbox
handlers. Pass the actual `Db::backend()`. Contention, temporary transport failures and
pool-acquisition timeout can be retried; scope/access/configuration errors, invalid SQL,
closed pools, corrupt data and unknown errors cannot. This does not change the narrower
`transaction_with_retry` contention policy and does not itself schedule or bound retries.
