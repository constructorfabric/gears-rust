# flows/

Multi-step end-to-end journeys shown as **full inline HTTP transcripts** — every request and response in sequence, numbered (`## Exchange 1`, `## Exchange 2`, …). A flow stands alone: you read the entire client↔broker dialogue top to bottom without hopping between files. (This intentionally duplicates request/response bodies that also appear in single-endpoint scenarios — the duplication is the feature.)

Every assertion is broker-observable: the calls a client makes and the broker's replies. SDK decision logic (which partitions to re-SEEK after a rebalance, recovery-loop control flow, retry budgets) is **out of scope** — that lives in the `event-broker-sdk-scenarios` change. Cross-links to single-endpoint scenarios are allowed only as non-normative "see also" pointers.

See [../INDEX.md](../INDEX.md#flows--multi-step-end-to-end-journeys) for the scenario list.
