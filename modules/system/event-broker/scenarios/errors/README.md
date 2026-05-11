# errors/

Scenarios asserting the RFC-9457 Problem Details envelope (`application/problem+json`) and its per-status-code shape. Centralizes the envelope contract (`type`/`title`/`status`/`detail`/`instance`, plus headers like `Retry-After`) so per-endpoint negative scenarios can reference these instead of restating the envelope.

See [../INDEX.md](../INDEX.md#errors--rfc-9457-envelope-per-common-code) for the scenario list.
