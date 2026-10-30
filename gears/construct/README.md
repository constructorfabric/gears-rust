# Construct

Construct keeps a living profile of a person, or of any other entity with a persistent identity, and serves it to the applications that need it. Connectors send records about the entity from many sources to Construct under GTS contracts. Construct checks each record against its type, turns it into facts, stores the facts and the relationships between them as a graph, and gives applications a structured profile, so they can tailor what they do to that person.

This folder holds the gear's design documents (the PRD, the DESIGN, the ADRs and the FEATURE documents) in `docs/`, and its code: the SDK crate in `construct-sdk/` and the gear crate in `construct/`. The gear takes connector records (record intake, with the record identity table and the connector record types) and keeps each subject's settings, with the subject's own settings routes.

Construct keeps its graph in [graph-storage](../graph-storage/).
