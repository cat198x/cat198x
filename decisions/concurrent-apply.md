# Concurrent apply: parallelise placement, keep the safety decision serial

**Status:** Proposed (2026-06-19).
**Scope:** Cat198x flagship — how `apply` executes a plan's file operations, and how progress is reported.
**Related:** [source-disposition.md](source-disposition.md) (the delete rule the safety decision enforces); `../CLAUDE.md` safety model (verify-before-delete, plan→apply→rollback); issue #47 (transient failures aren't resumable — a dependency, below).

## Bottom line

`apply` runs copies, moves, and relocates one at a time, which is the bottleneck over a network mount: each is a round-trip to the Time Capsule, and the engine waits for each before starting the next. Parallelise the **placement** operations (copy / move / relocate) across a worker pool — overlapping the waiting, the win on a latency-bound mount — while keeping every **stateful and safety-bearing** step (the rollback journal, the catalogue update, the verify-before-delete decision) on the single calling thread. This is the pattern repacks already use; extend it to the rest. Deletes and quarantines stay serial. The UI shows one row per worker.

## Context

The ops are **latency-bound, not bandwidth-bound**: a same-volume relocate over AFP is a server round-trip, an archive extraction is read-decompress-write-verify. Serially, the engine spends almost all its wall-clock waiting on one mount round-trip at a time. Overlapping N of them is close to an N× speed-up until bandwidth or the server's own concurrency limit bites.

The machinery is half-built. `ApplyOptions.jobs` exists and **repacks already run concurrently** (`execute_repacks_concurrent`: workers do the file I/O, the calling thread does journal + catalogue + status as each outcome streams back in completion order — because the `rusqlite` connection is not `Sync` and must never cross a thread). But the non-repack loop is serial and `ops.rs` hardcodes `jobs: 1`, so today nothing but repacks overlaps.

## Decision

### Which operations parallelise

- **Parallelise: Copy, Move, Relocate.** Independent file I/O (read source → write dest → verify → for move/relocate remove/rename source). The slow, latency-bound part. The planner guarantees independence within a batch: shared content is *copied* to each destination (never moved), and distinct content has distinct destination paths, so no two placement ops in one batch read or write the same file.
- **Serial: Delete, Quarantine.** Each carries the verify-before-delete safety decision (reads the catalogue *and* re-checks the disk for a same-tree surviving copy). They are cheap, and keeping them serial keeps that decision — the one that can destroy data — simple and ordered. Negligible wall-clock cost.
- **Repack: unchanged** — already concurrent via its own batch.

### Ordering: batch placement, flush before anything else

Process the plan in order. Accumulate consecutive placement ops into a batch; **flush the batch** (run it to completion) before any delete, quarantine, or repack. This preserves the one ordering that matters for safety: a plan places a surviving copy *before* it deletes the now-redundant source, and the delete's verify-before-delete check must see that copy already on disk. Flushing the placement batch before a delete guarantees it. (This mirrors how repacks already flush before non-repack ops.)

### Threading model (the load-bearing constraint)

Workers perform **file I/O only** and return an outcome. The **calling thread** owns everything stateful, draining outcomes in completion order: the rollback-journal append, the catalogue (`sync_catalogue_after`), the plan's per-op status, and the progress event. The database connection never leaves the calling thread. This is exactly `execute_repacks_concurrent`'s contract, generalised to a placement op.

### Worker count

Default **6** — tuned for a latency-bound network mount, not CPU. Configurable via the existing `jobs` (CLI `--jobs`; the UI passes the default). One worker reproduces today's serial behaviour exactly, which is the fallback if concurrency ever misbehaves.

### Resumability is a dependency (issue #47)

More in-flight ops means more to lose if the mount drops mid-batch. Today a dropped op is marked `Failed`, and a re-apply skips it (only `Pending` is retried) — so "Apply again to resume" silently does nothing. **Fix #47 alongside this:** a re-apply must retry `Failed` placement ops (reset to pending on a fresh apply, or an explicit retry-failed pass), while a permanent refusal (a `DeleteRefused`) stays sticky. Concurrency without this makes a mid-run drop materially worse.

### Progress + UI

Each progress event carries a **worker slot** (`0..jobs`). The UI renders up to `jobs` rows, one per slot, each showing that worker's current `from → to · remaining`; the aggregate `done/total` and `processed of total` sum across slots. `bytes_done` still counts only *completed* ops (a slot's bytes join the total when its op finishes), so the processed figure never runs ahead of the disk — concurrency doesn't change that guarantee, it just means several ops are "remaining" at once.

## Boundaries (deliberate)

- The executor's **primitives are unchanged** — `execute_copy/move/relocate` already verify-before-delete and fsync. This changes only *how many run at once* and *who drains their outcomes*, not what a single op does.
- **Deletes/quarantines stay serial** — revisit only if profiling shows they dominate (they won't; placement is the cost).
- **No new safety surface.** Verify-before-delete, last-copy refusal, journal append in completion order, and the single-threaded connection all hold exactly as today.
- **Within-batch independence** rests on the planner's guarantees (shared→copy, distinct paths). If that ever changes, the batch boundary must change with it.

## Drift triggers

Stop and re-consult this record if you find yourself:

- sharing the `rusqlite::Connection` across threads, or doing a catalogue/journal write off the calling thread — the connection is single-threaded by contract;
- parallelising deletes or quarantines, or moving the verify-before-delete decision onto a worker;
- dispatching a placement batch *across* an intervening delete/quarantine/repack without flushing first — that reorders place-before-delete and can defeat the surviving-copy check;
- adding concurrency without the issue-#47 retry path, so a mid-run mount drop strands `Failed` ops that resume won't retry;
- letting `bytes_done` count an in-flight op, so the processed total runs ahead of the disk (now multiplied by `jobs`).
