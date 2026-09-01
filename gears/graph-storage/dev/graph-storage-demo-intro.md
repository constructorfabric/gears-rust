# Graph Storage Gear — Community Intro

## Why we are building this

We are solving one problem: **storing relationships between different kinds of entities — and being able to search across them**.

Analysis of code, documentation and artifacts produces Findings — and those reference commits, pull requests, files, and each other. Artifacts and their traceability. Git objects. All of this is a graph by nature. Yet today every pipeline that needs such relationships has to build its own: its own relationship storage, its own search index, its own traversal queries.

And one kind of search is never enough. Real scenarios need all three — over the same data:

- **full-text** — find by words;
- **vector** — find by meaning, even when the words differ;
- **graph traversal** — find by relationships: "what is connected to this object within three hops".

Plus their combination: first narrow the candidates with search, then expand their surroundings through relationships.

Graph Storage is a platform gear that does this once, for everyone. Producer gears push typed nodes and edges into it (types are described through GTS, so independently developed gears can share one graph without breaking each other); consumers search, traverse, and build projections.

## The architectural decision: one PostgreSQL, no mirrors

**One PostgreSQL 19. Plain relational tables are the single source of truth. No second database, no dual writes, no graph extensions. The only extension is pgvector.**

The usual approach to this problem is a dedicated graph database next to the main one. Then every write happens twice, the two databases have to be kept in agreement, and access rights and tenant isolation have to be verified and audited twice — in two query languages. We walked that path in the prototype, and we know what it costs.

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

It almost reads like a drawing: "node `a`, an edge from it to node `b`, give me the neighbours".

The best analogy is a **view**. Just as a view stores no data and merely describes how to look at tables, a property graph is just a description: "this table is the vertices, this one is the edges, they join on these columns". In the PostgreSQL catalogs it exists as a relation **without a single byte of data** (`relkind = 'g'`, zero columns).

When a query arrives, `GRAPH_TABLE` is expanded at parse time into an ordinary join: `graph_node ⋈ graph_edge ⋈ graph_node`. From there everything familiar works: regular indexes, regular `EXPLAIN`, regular privileges and our secure-ORM scoping. The abstraction **costs nothing at runtime** — and there is no second database to keep in sync. We got a graph query language without paying for it in storage or consistency.

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

The key idea: the PostgreSQL planner sees the whole query at once and uses the right index at every stage — the HNSW vector index for similarity, the edge indexes for traversal, the full-text index for the filter. Intermediate results never leave the database: no pulling candidates into the application, no second round trip for neighbours and a third for the filter. On our test stand such a query answers in tens of milliseconds.

This freedom — combining any kinds of search in one query — is exactly what a "main database + graph database on the side" setup fundamentally cannot offer: there, the graph and the vectors live on opposite sides of an engine boundary.

## Three backends, byte-identical answers

Graph traversal in the gear is hidden behind an interface (`GraphQueryPort`), and it has three implementations:

1. **two simple queries** — fetch the edges, then fetch the nodes (works on any PostgreSQL);
2. **one query with a CTE** — the same thing in a single trip to the database;
3. **SQL/PGQ** — a graph pattern, as above.

Why three? Because SQL/PGQ requires PG19, and the gear must also run on a regular PG16. The interface hides which implementation answers — from the outside it is invisible.

But "invisible" has to be proven, not promised. We ran all three implementations end to end over HTTP on a graph of 200 thousand nodes and 600 thousand edges: **120 requests — and all three backends returned byte-for-byte identical answers**. Speed at depth 3: p95 = 52.8 / 33.1 / 35.3 ms respectively — all several times faster than the budget.

Beyond the benchmark, there is a dedicated test suite that compares the implementations against each other directly and **fails if they diverge**. The principle is simple: a backend that is faster because it answers differently is not faster — it is wrong. That suite has already paid for itself: it caught a bug that ordinary tests through the API structurally could not see (one backend returned extra nodes, but the upper layer happened to mask it).

And one more reason the bet on SQL/PGQ is a bet on the future. A standard in the PostgreSQL core will grow with every major version: PG20 is expected to bring variable-length paths (quantifiers — "1 to N hops" in a single pattern instead of a chain) and shortest-path search. When they arrive, we get them **for free**: same tables, same data, no migration — just new language capabilities over what is already in the database. With a separate graph database, every such feature would mean "wait for the vendor to implement it and hope it fits our schema".
