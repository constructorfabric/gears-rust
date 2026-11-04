# Heartbeats and leases

**Status:** Draft · [All proposals](README.md)

## Problem

The worker-session model is right for the scale (one heartbeat write per process instead of per task, ADR-0017), and the lease fencing is strong. The gaps are availability, liveness and a few rules that disagree:

- **A PDP outage expires every lease.** Every lease-fenced call, heartbeats included, checks `claim`. A PDP outage of one lease length (30 s) expires every session and requeues every running task on the platform (PRD.md:925, :961; DESIGN.md:1022; ADR-0016:81).
- **A dropped task holds its slot for hours.** A handler that panics or returns without finishing leaves its task leased until the execution timeout: 3,600 s by default (DESIGN.md:2086), 86,400 s on the shipped default queue.
- **The two expiry scans disagree.** The deadline scan ends a running task past its deadline `timed_out` (DESIGN.md:2770-2771), but the session scan requeues it as a lost worker (DESIGN.md:2755-2759, :2777-2804). The scans often run in different shards (`session_id` vs `task_id`, DESIGN.md:1975), so whichever lags decides the outcome.
- **Revocation only works one way.** Only an explicit PDP denial releases the task (PRD.md:961, DESIGN.md:1022, ADR-0016:81). If the PDP allows the call but its constraints exclude the task, the fenced `UPDATE` matches 0 rows, the worker gets `LEASE_LOST` or `404` and stops, and the task stays `running` until its execution timeout.
- **Overload sheds heartbeats.** DESIGN says heartbeats and completions are shed last (DESIGN.md:2032, :2053), but admission is one 512-permit semaphore per node with a 100 ms acquire timeout (DESIGN.md:2021) that treats every request alike. Claims hold a permit for a whole transaction and can push heartbeats out; shedding that lasts longer than about 20 s (two missed heartbeats) expires sessions.

## Proposal

A heartbeat renews the session without the PDP. The SDK gives back tasks its handlers drop. Both scans apply one timeout rule, a worker that loses its rights to a task releases it, and the execution path gets admission that claims cannot use up.

- [Proposed] **No PDP call on heartbeat.** A heartbeat renews the session without checking `claim`.
  - **Still checked:** authentication (local token validation), the session's subject guard, and token, holder and state per lease (ADR-0017). A heartbeat changes no task data and only extends leases the subject already holds.
  - **Other worker calls** (start, progress, checkpoint, complete, fail, release, waits) still check `claim`, so a revoked worker cannot save results or progress.
- [Proposed] **The SDK releases dropped leases.** A handler that ends without finishing its task gives the lease back automatically (PRD.md:676).
  - **How:** when a handler panics, is aborted, or returns without `complete`, `fail`, `release` or a wait, its `Lease` is dropped onto a release queue. The heartbeat loop sends those releases and retries until the engine confirms or answers `LEASE_LOST`.
  - **No engine change.** A process that dies still loses its session as today (ADR-0017).
  - **Not covered:** a REST-only worker that forgets to release waits for the execution timeout. A lost claim response stays with the assignment timeout. A handler that is alive but stuck is an open question below.
- [Proposed] **Same timeout precedence in both scans.** A running task past its execution deadline ends `timed_out`, whichever scan finds it first.
  - **Change:** both scans call one shared expiry step with the same order of causes. No new scan, query or write; the two scans stay separate (ADR-0017, DESIGN.md:788).
- [Proposed] **Revocation releases the task however it arrives.** A worker call whose scope no longer covers its task releases the task and returns `403`.
  - **Change:** when a single-task worker call's fenced `UPDATE` matches 0 rows, the engine re-reads the task without scope. If token, holder and state still match, it releases the task as for a denial and returns `403`; otherwise `LEASE_LOST` as today.
  - **Cost and safety:** one extra read, only on the failure path. The `403` reveals nothing new, because the worker holds a valid lease token (PRD.md:918). A periodic recheck, if adopted (open question below), releases only the leases outside its constraints.
- [Proposed] **Reserved admission for the execution path.** Heartbeat, `complete`, `fail`, `release` and `cancel` get permits that claims cannot use up.
  - **Change:** keep the shared 512 permits for everything and add a reserved pool on top for the execution path (for example 64, so 576 in total). The execution path takes a reserved permit first, then falls back to the shared pool. Claims keep today's capacity and sit with reads, below heartbeats and completions.

## What we give up

- Revocation takes effect at the worker's next lease-fenced write instead of its next heartbeat. A revoked worker that never writes keeps its slot until its execution timeout.
- Under overload, claims wait longer or are shed before heartbeats and completions.

## Open questions

- [Open] **PDP outage: which operations must keep working, and is `503` the code?**
  - **Context:** with heartbeats off the PDP, leases survive an outage, but creates, claims and every worker write (start, progress, checkpoint, complete, fail, release, waits) still fail closed. No new work starts; running tasks keep running but cannot save progress or finish.
  - **Side effect:** background loops do not call the PDP (DESIGN.md:221), so a task whose execution timeout passes during the outage ends `timed_out` even if its worker has finished and is retrying `complete`.
  - **Status code:** an unreachable PDP gives `403` in PRD.md:918, and `503` in PRD.md:961 and DESIGN.md:1022.
  - **Option A:** fail closed for everything, as today.
  - **Option B:** work in hand finishes from a cached `claim` decision at most 60 s old; new claims and creates still fail closed.
- [Open] **Periodic recheck: should the engine recheck `claim` per session from a cached decision at most every 60 s?**
  - **Context:** without the heartbeat check, a revoked worker that never writes keeps its slot until its execution timeout.
  - **Platform:** cached decisions bounded by token expiry are allowed (docs/arch/authorization/DESIGN.md:751, :759), but the caching protocol is not designed yet (:753).
- [Open] **Stuck handlers: which option?** A handler that is alive but stuck (deadlock, endless wait, blocked call) keeps its lease, because the heartbeat loop proves the process is alive, not that the handler is working. Only a signal from the handler can show progress.
  - **(a) Opt-in touch interval per type.** The handler calls `lease.touch()` or `progress` at least that often; the SDK treats an untouched lease as dropped, fires `lease.lost` and releases the task (like Temporal's optional `HeartbeatTimeout`). Types that do not set it rely on the execution timeout as today.
  - **(b) Short default execution timeout** that the handler extends with a refresh call (Hatchet: 60 s default and `refresh_timeout`).
  - **(c) Keep relying on the execution timeout.**


## Doc changes

- PRD.md:676, :696, :711, :925, :961.
- DESIGN.md:530, :1009, :1022, :1921, :2032, :2750-2759, :2777-2804.
- ADR-0016:55, :81; ADR-0017:56.
- DESIGN.md:703 (SDK worker runtime): a dropped `Lease` is released by the heartbeat loop.
- PRD.md:961, DESIGN.md:1009, :1022, ADR-0016:81: a worker call whose scope excludes a task it holds releases it and returns `403`.
- DESIGN.md:2021, :2024, :2032, :2035, :2053 (admission pools and classes) and :703 (SDK heartbeat retries): reserved admission for the execution path.
