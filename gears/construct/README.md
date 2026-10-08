# Construct

Construct keeps a living profile of a person, or of any other entity with a persistent identity, and serves it to the applications that need it. Connectors send records about the entity from many sources to Construct under GTS contracts. Construct checks each record against its type, turns it into facts, stores the facts and the relationships between them as a graph, and gives applications a structured profile, so they can tailor what they do to that person.

This folder will hold the gear's design documents: the PRD, the DESIGN and the ADRs, in `docs/`. The code follows in later changes.

Construct keeps its graph in [graph-storage](../graph-storage/).
