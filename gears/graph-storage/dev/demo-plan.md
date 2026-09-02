# Graph Storage Gear — Demo Speaker Notes

---

## 1. WHY — the problem

**One sentence:** We store relationships between entities — and search across them.

- Data we deal with every day is naturally a **graph**: what matters is not only the data itself, but **how it is connected**.
  - Our examples: findings → commits, PRs, files, each other; artifacts → their sources.
- We build a **universal graph storage** — one platform gear, reused by all pipelines.
  - Instead of each pipeline making its own links, indexes, traversal queries.
- One search is never enough. Real scenarios need **three at once**:
  - **Full-text** — find by words
  - **Vector** — find by meaning (words can differ)
  - **Graph** — find by relationships ("connected within 3 hops")
- And their **combo**: narrow with search → expand through relationships.

**Punchline:** Graph Storage does this **once, for everyone**. Producers push nodes/edges. Consumers search, traverse, project. Types via GTS → independent gears share one graph safely.

---

## 2. ARCHITECTURE — one database, no mirrors

**Key line:** One PostgreSQL 19 + pgvector + SQL/PGQ. No second DB. No dual writes. Only extension: pgvector.
- PG19 made it possible to work with graphs directly within PostgreSQL: **SQL/PGQ is in the database core** (SQL:2023 standard).
- Result: **one engine** answers text + vector + attribute + graph queries.
  - Same consistent rows. One access-control layer.

---

## 3. SQL/PGQ — graph as a "view"

**Key analogy (say it slowly):** A property graph is like a **VIEW**.
- A view stores no data — it describes how to look at tables.
- Same here: "this table = vertices, this table = edges, join on these columns."
- In catalogs: a relation with **zero bytes of data** (`relkind = 'g'`, zero columns).

**Show:** Cypher-style MATCH pattern inside plain SQL.

```sql
MATCH (a IS node)-[e IS edge]->(b IS node)
WHERE a.id = $seed
```

- Base tables: ordinary `graph_node` and `graph_edge`.
- No migration of data. No new storage. Just a new way to query.

---

## 4. HYBRID SEARCH — the money slide

**Task to state out loud:**
"Find nodes similar in meaning → expand their neighbours → keep only rows with a given word."

Three searches. **One SQL statement.** Walk the query top-down:

1. **Vector** — `ORDER BY embedding <=> $query LIMIT 5` → HNSW index → seeds
2. **Graph** — `GRAPH_TABLE ... MATCH` → edge indexes → neighbours
3. **Full-text** — `search_text @@ websearch_to_tsquery` → FTS index → filter

**Key idea (emphasize):**
- Planner sees the **whole query at once**.
- Right index at every stage.
- Intermediate results **never leave the database** — no glue code, no network hops.

---

## 5. THE BET ON THE FUTURE

- SQL/PGQ = a **standard in the Postgres core** → grows with every major version.
- PG20 expected: variable-length paths, shortest-path search.
- When they land → **we get new features for free**, over data already in place.

**Closing line:** One engine, one copy of the data, three kinds of search — and it only gets better from here.