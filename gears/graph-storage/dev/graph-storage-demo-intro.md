# Graph Storage Gear

## Why we are building this

We are solving one problem: **storing relationships between different kinds of entities — and being able to search across them**.

Graphs are everywhere in our platform: Findings reference commits, pull requests, files, and each other; artifacts reference what they were built from. The data is already a graph — we want to store it as one and use it effectively, instead of every pipeline rebuilding its own links, indexes, and traversal queries from scratch.

And one kind of search is never enough. Real scenarios need all three — over the same data:

- **full-text** — find by words;
- **vector** — find by meaning, even when the words differ;
- **graph traversal** — find by relationships: "what is connected to this object within three hops".

Plus their combination: first narrow the candidates with search, then expand their surroundings through relationships.

Graph Storage is a platform gear that does this once, for everyone. Producer gears push nodes and edges into it; consumer gears search, traverse, and build projections. Every node and edge is typed, and the types are described through GTS — that is what lets independently developed gears share one graph without breaking each other.

## The architectural decision: one PostgreSQL, no mirrors

**One PostgreSQL 19 + pgvector + SQL/PGQ. No second database, no dual writes, no graph extensions. The only extension is pgvector.**

The usual approach to this problem is to put a dedicated graph database next to the main one. It sounds simple, but it means duplicating everything: every write happens twice, and keeping the two copies in sync becomes a permanent engineering effort.

PostgreSQL 19 let us drop all of it: graph queries (SQL/PGQ, the SQL:2023 standard) are now **in the database core itself**.

So the stack is: **PG19 + pgvector + SQL/PGQ**. One engine answers text, vector, attribute, and graph queries — over the same consistent rows, under one layer of access control.

## SQL/PGQ: a graph is not a separate store — it's a way of looking at tables

On top of the ordinary `graph_node` and `graph_edge` tables we declare a property graph — and then write Cypher-style patterns directly in SQL:

```sql
SELECT * FROM GRAPH_TABLE (kb_pgq
  MATCH (a IS node)-[e IS edge]->(b IS node)
  WHERE a.id = $seed
  COLUMNS (b.id AS neighbour, e.edge_type AS via)
);
```

The best analogy is a **view**. Just as a view stores no data and  describes how to look at tables, a property graph is just a description: "this table is the vertices, this one is the edges, they join on these columns". In the PostgreSQL catalogs it exists as a relation **without a single byte of data** (`relkind = 'g'`, zero columns).

## Hybrid search: three kinds of indexes in one query

This is where everything comes together. The task: "find nodes similar in meaning to the query, expand their neighbours, and keep only those where a given word appears". Three different kinds of search — and on one engine it is **a single SQL statement**:

```sql
FROM (SELECT id FROM graph_node                    -- 1. by meaning:
      ORDER BY embedding <=> $query LIMIT 5)        --    vector index (HNSW)
     AS seeds,
     GRAPH_TABLE (kb_pgq                            -- 2. by relationships:
       MATCH (a IS node)-[e IS edge]->(b IS node)   --    graph traversal
       WHERE a.id = seeds.id
       COLUMNS (b.id AS neighbour)) AS g
WHERE search_text @@ websearch_to_tsquery($word)    -- 3. by words:
                                                    --    full-text index
```

The key idea: the PostgreSQL planner sees the whole query at once and uses the right index at every stage — the HNSW vector index for similarity, the edge indexes for traversal, the full-text index for the filter. Intermediate results never leave the database.


## The bet on SQL/PGQ  is a bet on the future.
 A standard in the PostgreSQL core will grow with every major version: PG20 is expected to bring variable-length paths and shortest-path search. When they arrive, we get new language capabilities over what is already in the database.