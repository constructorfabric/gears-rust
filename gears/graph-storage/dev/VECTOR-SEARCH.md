# Vector search in the graph-storage prototype

What was built, what it cost to build, and what a live stand proved about it.
Companion to [`DEVIATIONS.md`](./DEVIATIONS.md), which records the divergences
from PR #4523; this file records the *experiment* — the difficulties, how each
was resolved, and the implementation that came out of it.

<!-- toc -->

- [Where it started](#where-it-started)
- [The implementation that was chosen](#the-implementation-that-was-chosen)
  - [1. One provider per deployment, behind a published contract](#1-one-provider-per-deployment-behind-a-published-contract)
  - [2. The Embedding Coordinator, before the transaction](#2-the-embedding-coordinator-before-the-transaction)
  - [3. The four vector states, on two columns](#3-the-four-vector-states-on-two-columns)
  - [4. The embedding space is an identity, not a width](#4-the-embedding-space-is-an-identity-not-a-width)
  - [5. The ONNX provider](#5-the-onnx-provider)
- [Difficulties, and how they were resolved](#difficulties-and-how-they-were-resolved)
- [The live run](#the-live-run)
- [What the stand did not prove](#what-the-stand-did-not-prove)
- [Reproducing it](#reproducing-it)

<!-- /toc -->

## Where it started

The gear stored vectors and had an HNSW cosine index, and **computed nothing**.
`EmbeddingProviderV1` shipped as a trait with no implementation anywhere in the
repository; the word `onnx` did not appear in the gear's code; the gear had no
background task of any kind. Vectors arrived ready-made from the producer
(`NodeSpec.embedding`), and query vectors arrived ready-made too
(`SearchRequest.query_vector`).

That is option **C** of ADR-0005 — the option it rejects by name, because
*"nothing enforces that all producers and the query side use the same model —
mixed-model vector spaces silently break similarity ranking, and the gear
cannot embed query text at all without a model."*

Three things were inert in a way that only showed up when someone asked how
vectorization actually worked:

- The **`vector_search` trait** was resolved across the derivation chain,
  stored with the type's effective traits, and **published through
  `GET /types`** with the description *"Paths composed into the embedding
  input"* — while nothing composed anything from it. A producer reading the
  catalogue was told the paths it declared did something.
- **`node.embedding_epoch`** and **`node.embedding_input_hash`** existed in the
  initial schema and were written `NULL` on every path, so the vector states
  `fr-embedding-pipeline` requires had nowhere to live.
- There was **no embedding-space identity anywhere**, so a same-dimension model
  swap would corrupt ranking invisibly — the failure ADR-0005 spends most of
  its length preventing.

## The implementation that was chosen

Five decisions, in the order they constrain each other.

### 1. One provider per deployment, behind a published contract

Both producer-facing vector fields were **removed**: `NodeSpec.embedding` and
`SearchRequest.query_vector` are gone, replaced by `options.embed`. The gear
computes both sides of every comparison itself, so the single-embedding-space
constraint is structural rather than advisory — sending a foreign vector is not
refused, it is unrepresentable.

The contract itself became **executable** and lives in the SDK
(`graph_storage_sdk::contract`, behind `test-support`): ADR-0005 asks for
"contract tests [that] run all three plugins against the provider contract",
and a suite living in one implementation's `tests/` directory cannot be reached
by another crate. Every provider proves itself against the same assertions.

The identity hash moved into the SDK too, for the same reason: it is the *name*
readiness compares against, so the ONNX default, a remote plugin and the CI
fake must derive the same string for one space and different strings for two.

### 2. The Embedding Coordinator, before the transaction

`domain/embedding.rs` composes each node's input from its name plus the payload
paths its type declares in `vector_search`, hashes it canonically, and calls
the provider **once per batch**.

It runs *before* the write transaction — which is where DESIGN's ingest
sequence puts it (step 5, ahead of step 6) — and that placement cost nothing:
`validate_batch` has already resolved every `TypeRecord`, so each node's trait
is in hand with no extra round trip.

### 3. The four vector states, on two columns

`fr-embedding-pipeline` names four states and does not say how a store
distinguishes them. The encoding chosen (recorded as D-017):

| state | `embedding` | `embedding_epoch` | `embedding_input_hash` |
|---|---|---|---|
| embedded and current | the new vector | active epoch | the new input's hash |
| absent | NULL | NULL | the current input's hash |
| preserved | kept | kept | kept |
| stale | kept | **NULL** | kept |

`embedding_input_hash` describes the **stored vector's** input, never the
node's current text — that is what lets a later ingest tell *preserved* from
*stale*. The vector arm then reads `embedding_epoch = <active>`, so "similarity
search must consider only current vectors" is one equality rather than a rule
every query has to remember, and the HNSW index is partial on
`embedding_epoch IS NOT NULL` so a stale row does not occupy a slot in the
candidate set.

The decision itself lives in `domain::embedding::decide_vector`, **shared by
both store implementations**. That is deliberate: the endpoint-constraint
episode earlier in this prototype established that a check living in one store
is a check the conformance suite cannot see.

### 4. The embedding space is an identity, not a width

`embedding_space` (migration `m0003`) is the canonical durable home of the
identity: model artifact, tokenizer artifact, preprocessing, pooling,
normalization, dimension — hashed together. A partial unique index enforces at
most one `active` row, so "exactly one embedding space per deployment" is a
database fact rather than something the boot path remembers.

Boot compares the active provider against the recorded identity. On a mismatch
it **does not open a new epoch** — that would strand every stored vector in a
space nothing searches — it reports the mismatch and blocks the vector arm.
Every other path is untouched, because only vectors are incomparable.

### 5. The ONNX provider

`gears/graph-storage/onnx-embedding-plugin`: a MiniLM-class sentence model
through ONNX Runtime, in the gear's own process, behind the gear's off-by-
default `onnx` feature.

**Artifacts are read, never fetched.** ADR-0005 names the fastembed/ort
ecosystem, and fastembed would download weights at boot — which makes the
embedding-space identity implicit, since the only identity a downloader can
offer is the name it asked for. Reading operator-supplied files lets the
identity be the SHA-256 of the bytes actually loaded, so two deployments
claiming one space either agree on that hash or are visibly different.

Mean pooling over the attention mask, L2-normalized, one session behind a fair
mutex (`ort`'s `Session::run` takes `&mut self`, so inference serializes
whatever the sharing).

## Difficulties, and how they were resolved

**1. `ort` 2.0.0-rc.12 hangs instead of erroring on a bad `ORT_DYLIB_PATH`.**
Measured in this repository against a nonexistent path: neither
`Session::builder` nor `ort::init_from` returns in 45 seconds, there is no
pre-flight validation, and the hang cannot be interrupted from inside.
*Resolved* by reusing file-parser's mitigation: the init runs on a raw
`std::thread` (not `spawn_blocking`, which Tokio joins at shutdown) and the
thread is **abandoned** on timeout. A caller receiving `RuntimeHung` has leaked
one thread and must end the process rather than retry.

**2. Two ONNX tests measured nothing, and looked green.** `MiniLM`'s tokenizer
pads to a fixed 128 positions, so a same-batch comparison is padded identically
either way — the test held with mean pooling **ignoring the attention mask
entirely**. So did `related > unrelated`: unmasked it scores 0.85 against 0.52,
because 122 padding vectors make everything resemble everything, and the
ordering survives by a hair. Masked, the same pair scores 0.72 against 0.02.
*Found* by deliberately breaking the mask and watching the tests pass.
*Resolved* by comparing across **padding lengths** (two providers over one
model, differing only in their token ceiling) instead of across batch mates,
and by asserting a *margin* rather than an ordering.

**3. The conformance suite invented its own vector width.** It picked 8, the
in-memory fake accepted it, and every case failed on the first real server with
`expected 384 dimensions, not 8`. *Resolved* by taking the width the schema was
migrated with. This is the second time in this prototype that having two store
implementations caught something inspection had not.

**4. The domain service's own wiring was covered by nothing.** The service and
the conformance suite each resolved a type's declared vector paths, separately
— so a service that passed *no* paths at all would have left every test green.
*Resolved* by collapsing both onto one shared `declared_paths`, then breaking
it to confirm the suite now fails.

**5. Vendoring forks the contract.** studio-web vendors the gear, which copies
the SDK into `crate::graph_storage::sdk`. The ONNX plugin, being a normal
crate, implements the *published* trait:

```text
`OnnxEmbeddingProvider` implements similarly named trait
`graph_storage_sdk::plugin_api::EmbeddingProviderV1`, but not
`EmbeddingProviderV1`
```

Two identical traits, structurally distinct. *Resolved* for the experiment with
a shim (`studio-backend/src/graph_storage_onnx.rs`) that restates the request,
response, identity and error types field for field, plus one more mechanical
rewrite in the vendoring script. It is ~120 lines and it disappears the moment
the gear is depended on rather than copied — which is the real lesson: **an
external plugin cannot be used with a vendored gear without a shim per
plugin.** Worth weighing when deciding how long vendoring continues.

**6. A dependency that had to be added, and one that did not.** `tokenizers`
is new to the workspace; `default-features = false` with `fancy-regex` avoids
the C `onig` build that the default feature set pulls in. `ort` was already
there — file-parser's optional `magika` feature had established the pin, the
`load-dynamic` strategy and the CI recipe for fetching the runtime, all of
which this crate reuses.

## The live run

Against **PostgreSQL 19 + pgvector** in the studio-web stand, with the ONNX
provider and the real `all-MiniLM-L6-v2` artifacts, through the gear's REST
surface.

**Boot.** Three migrations applied on an empty database; the provider named
itself by content hash and the space opened:

```text
loaded the in-process ONNX embedding provider
  model=model.onnx@sha256:6fd5d72f…
embedding space active epoch=1 identity=f6903c4d… model=model.onnx@sha256:6fd5d72f…
```

**The trait is live.** A producer type declaring
`x-gts-traits: { vector_search: ["/payload/summary"] }` came back from
`GET /types` with that path in its resolved effective traits, and the columns
that were previously always `NULL` were populated on ingest:

```text
 node_key  | embedding_epoch | input_hash | has_vector
-----------+-----------------+------------+------------
 finding:1 |               1 | bf45f8a427 | t
```

**Semantic retrieval.** Four documents; three queries, each a paraphrase
sharing **no vocabulary** with its target. Each returned the right document
first:

| query | top hit | the text it matched |
|---|---|---|
| "a hardcoded secret was checked into source control" | `finding:1` Credential leak | "a plaintext password was committed into the deployment script" |
| "the page takes far too long to load" | `finding:2` Slow query | "the dashboard endpoint takes twelve seconds because the join is unindexed" |
| "we are refurbishing the break room" | `finding:3` Kitchen remodel | "the office kitchen renovation is scheduled for next spring" |

**Stale.** Re-ingesting `finding:1` with `embed: false` and a changed summary
dropped it out of the vector arm while leaving it fully present everywhere
else — `embedding_epoch` cleared, the vector kept for re-embedding,
`has_embedding: true` on the node read, and still first on the lexical arm.
Re-ingesting with `embed: true` brought it back, now retrievable by its new
meaning.

**Preserved.** Re-ingesting `finding:2` byte-identically with `embed: false`
reported `nodes_unchanged: 1`, kept its epoch, and it still ranked.

**The guard.** Restarting the same stand with a different provider against the
recorded space blocked the vector arm and nothing else:

```text
ERROR stored vectors belong to a different embedding space than the configured
      provider; vector search is blocked until the graph is re-embedded
      recorded_identity=f6903c4d… active_identity=6c6d0bdc…
```

and through the API, `POST /search` with `mode: vector`:

```json
{
  "title": "Failed Precondition", "status": 400,
  "context": { "violations": [ {
    "subject": "embedding_space", "type": "EMBEDDING_SPACE_MISMATCH",
    "description": "stored vectors belong to a different embedding space than the active provider; re-embedding is required before similarity search can rank them"
  } ] }
}
```

while `mode: lexical` answered normally in the same moment, and ingest kept
writing — recording no vector rather than failing the batch.

That last observation is the most valuable one of the exercise: it is exactly
the failure ADR-0005 exists to prevent, caught on a real database, with a real
model swap, and confined to the one arm it affects.

## What the stand did not prove

- **That a re-embedded node stops matching its old meaning.** The corpus was
  four documents and the node's *name* did not change — and the name is part
  of the composed input by design — so the old query still matched it. A
  negative needs a larger corpus than this run had.
- **Latency against PRD § 6.1.** Nothing was timed; four documents is not the
  seeded reference graph the criterion names.
- **The ONNX provider inside the container image.** The stand runs the binary
  natively, because a `[patch]` on a sibling checkout and an image build are
  mutually exclusive (D-015) — and the plugin is a path dependency for the same
  reason.
- **The model-change lifecycle.** Blocking is the safe half; `requested →
  scanning → embedding → validating → cutover → complete`, its administrative
  API and the resumable backfill are all deferred. The experiment reset the
  space by hand, which is precisely the operation that lifecycle would own.

## Reproducing it

```sh
# Artifacts, pinned by the same checksums the CI lane uses.
M=~/.cache/cf-graph-storage
mkdir -p $M/minilm && cd $M/minilm
curl -sL --fail -O https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json
curl -sL --fail -o model.onnx https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx
cd $M
curl -sL --fail -o ort.tgz https://github.com/microsoft/onnxruntime/releases/download/v1.29.0/onnxruntime-linux-x64-1.29.0.tgz
tar xzf ort.tgz

# The gear's own lanes.
cd gears-rust
GEARS_TEST_PG_GRAPH_IMAGE=studio-graph-postgres:latest GEARS_TEST_PG_GRAPH_REQUIRED=1 \
  cargo test -p cf-gears-graph-storage
ORT_DYLIB_PATH=$M/onnxruntime-linux-x64-1.29.0/lib/libonnxruntime.so \
GRAPH_STORAGE_ONNX_MODEL=$M/minilm/model.onnx \
GRAPH_STORAGE_ONNX_TOKENIZER=$M/minilm/tokenizer.json \
GRAPH_STORAGE_ONNX_REQUIRED=1 \
  cargo test -p cf-gears-graph-storage-onnx-embedding-plugin

# The stand (studio-web), natively against the compose PostgreSQL.
cd ../studio-web && docker compose up -d graph-postgres keycloak
cd studio-backend && cargo build --features onnx
STUDIO_EMBEDDING_PROVIDER=onnx \
STUDIO_EMBEDDING_MODEL=$M/minilm/model.onnx \
STUDIO_EMBEDDING_TOKENIZER=$M/minilm/tokenizer.json \
ORT_DYLIB_PATH=$M/onnxruntime-linux-x64-1.29.0/lib/libonnxruntime.so \
  ./target/debug/studio-backend --config config/local-stand.yaml
# REST: http://127.0.0.1:8090/cf/graph-storage/v1
#       Authorization: Bearer studio-admin-token
```
