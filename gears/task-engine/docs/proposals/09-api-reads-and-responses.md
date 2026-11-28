# API reads and responses

**Status:** Draft · [All proposals](README.md)

## Problem

- **Several reads return lists that can grow without limit:** lists embedded in `GET /tasks/{id}` and in search results at detail `full`, a task's event history, and wait conditions per group. One large task can make a single response arbitrarily large.
- **Some reads duplicate others.** `GET /admin/dlq` is a search on `state = dead_lettered`, and a single-task heartbeat duplicates the batch heartbeat.
- **The search window looks like a gap but is not.** Search for finished tasks covers a bounded window of creation time (PRD.md:1143). Searching every stored task (500 million at the capacity profile) is not something the engine should do; reads by ID already cover old tasks.

## Proposal

- [Proposed] **Search window stays as designed.** Search for finished tasks covers at most `search_max_window_days` (31) of creation time, 7 by default (PRD.md:1143); unfinished tasks have no window.
  - **By ID:** `GET /tasks/{id}` and a task's history keep working for every stored task.
  - **Long-range search** goes to another gear fed by lifecycle events, as ADR-0001 already says (ADR-0001:88, :101).
  - **Capacity profile:** the 500 million profile stays in p1 (PRD.md:1294, :1310). The search target applies only within the window, and the profile still validates indexes, pooling and retention.
- [Proposed] **Bounded responses.** No read returns a list that can grow without limit.
  - **`GET /tasks/{id}`:** no unbounded embedded lists.
  - **Search:** every list embedded at detail `full` is capped; new `parent_task_id` and `predecessor_id` filters.
  - **`GET /tasks/{id}/events`:** cursor paging, a page-size limit and filters.
  - **Waits:** a limit on wait conditions per group.
  - **Removed:** `GET /admin/dlq` (use search on `state = dead_lettered`) and the single-task heartbeat (only the batch heartbeat stays).
