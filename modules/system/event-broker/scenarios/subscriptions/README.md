# subscriptions/

Scenarios for subscription lifecycle: `POST /v1/subscriptions` (JOIN) and `DELETE /v1/subscriptions/{id}` (LEAVE). Covers topic-anchored typed-filter interests, multi-topic JOINs, parallelism (N subscriptions on one group), filter expressions, and the authz / validation rejections. Pre-stream positioning lives in [../positions/](../positions/); streaming lives in [../transports/](../transports/).

See [../INDEX.md](../INDEX.md#subscriptions--post-v1subscriptions-delete-v1subscriptionsid) for the scenario list.
