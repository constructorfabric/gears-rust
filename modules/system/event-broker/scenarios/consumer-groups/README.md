# consumer-groups/

Scenarios for the consumer-group registry: `POST/GET/DELETE /v1/consumer_groups`. Covers the anonymous create path (broker-minted GTS id), read/list, and deletion (allowed only when no active members reference the group). Named groups are registered via `types_registry` at broker startup and are out of scope for the create scenarios here.

See [../INDEX.md](../INDEX.md#consumer-groups--postgetdelete-v1consumer_groups) for the scenario list.
