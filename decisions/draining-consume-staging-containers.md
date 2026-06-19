# Draining consume staging containers after a repack

**Status:** Investigated, implemented forward-only — **not shipped** (rollback gap). 2026-06-19.
**Scope:** Cat198x flagship — the planner's repack path and the verify-before-delete safety net.
**Related:** [source-disposition.md](source-disposition.md) (consume vs preserve, the delete rule); [concurrent-apply.md](concurrent-apply.md) (repacks run concurrently, deletes serial). WIP on branch `feat/drain-consume-containers`, commit `19ef5a6`.

## Bottom line

A `consume` staging area (`ToSort/`) does **not** drain for archive sets that have to be *recompressed* into the library — e.g. TOSEC-ISO CD images. The content gets copied into the library, but the original staging container is left behind, so the same bytes sit in two places (a real **~12 GB** for the TOSEC-ISO run on 2026-06-18). The forward fix is built and well-tested; it is **blocked on a rollback-coherence gap** and must not merge until that is closed.

## The symptom

After applying a TOSEC-ISO plan, free space did not rise. Investigation showed:

- The 5,917 deletes were tiny duplicate `.cue` files — they reclaim ~0 GB.
- The 574 repacks rebuilt CD containers (`.zip` holding `.ccd`/`.img`/`.sub`) from `ToSort/` into `Library/ROMs/`, **byte-content-identical** (verified), but **kept the ToSort originals**.
- A re-plan produced 0 further removals — nothing cleans the staging copies up.

So `consume` staging silently fails to drain for this shape of data, even though disposition is configured correctly.

## Root cause

For each game the planner picks a `build_from` container. A complete `consume` container is normally **relocated whole** (an instant rename that drains staging). But the relocate fast-path requires `!game_shared`, and a CD container's shared `.cue`/`.sub` content (the same bytes appear in another game's DAT entry) makes the game `game_shared`. That forces a **copy-repack** instead — and the build-from container is explicitly excluded from the archive-dedup pass (`build_from == path → continue`). So the container we rebuild from is never a deletion candidate, regardless of disposition.

Note the trap: `compute_shared_containers` flags a container as "shared" if **any entry it holds matches another game's DAT**. A single-game CD container holding shared `.sub` content is therefore flagged "shared" even though it belongs to one game — so a naive `!shared_containers` guard *blocks the exact case we need to fix*. (A unit test caught this.)

## The safe condition

A build-from container is safe to drain once the destination archive holds **every entry** the container held — which a successful repack guarantees, since it built the destination from those entries and verified each against its SHA1. The right arbiter of "every entry survives" is the existing **verify-before-delete net** (`delete_has_surviving_copy`), which already works at the *entry* level: it pulls a container's inner-entry SHA1s and refuses unless each survives elsewhere on disk. We do not need to reimplement that judgment in a plan-time guard — we route the removal through the net.

## What was built (forward direction — safe, tested)

The planner records each build-from archive container that a repack rebuilt from and whose source permits the loss (`consume`, or `preserve` rebuilding within its own tree), then emits it as a normal `Delete` **after every repack** (so the apply runs the rebuilds first and the catalogue records the destinations' entries before the verify). No plan-time shared-container guess — the verify-before-delete net is the safety check, and it refuses (sticky) any container another game still needs.

Tests (all green): the single-game shared-entry case drains (the real CD case); the fully-consolidated shared container drains exactly once; loose build-froms are left to the existing `move_sources`; and an apply-level test proves the net permits the drain only when every entry survives and refuses the instant one does not.

## Why it is not shipped — the rollback gap

A copy-mode repack's journal reverse is `Delete dest`. A plain `Delete` carries **no restoring reverse** (deletes are not journaled). So rolling back a plan that drained containers would remove the destination **and** never rebuild the container → **content lost**. The forward path refuses to lose data during apply; the rollback path would.

This is the coupling the *executor-internal* approach (drain inside `execute_repack`, reverse = rebuild-container-from-dest) had correct, and that the separate-`Delete` route decouples and breaks.

## The defined next step

Couple the drain to the repack's reverse. The container-drain must journal a **rebuild-container-from-`dest`** reverse (extract the entries back out of the destination archive, verified by SHA1, then it is the repack's reverse that deletes `dest`). Because rollback runs in **reverse plan order** and the drain is emitted *after* the repack, the drain's rebuild reverse runs **before** the repack's `Delete dest` — so the container is restored from the destination before the destination is removed. Content preserved.

Concretely this grafts the WIP branch's first-approach machinery (a `RebuildContainer` logged reverse + executor support, which was prototyped and round-trip-tested) onto this branch's verify-before-delete guard. Keep the forward guard (the net decides safety); add the reverse (rollback restores).

## Interim reality

The TOSEC-ISO content is **safe** — every drained-candidate container's bytes are verified present in the library. The ~12 GB of redundant staging copies are harmless (wasted space), not at risk. Do not hand-delete `ToSort/TOSEC-ISO/` to reclaim space until the tool can do it reversibly, or accept that manual removal is one-way.

## Drift triggers

- "Just guard on `!shared_containers`" — no; it flags single-game CD containers as shared and blocks the fix. The net, not a plan-time guess, is the arbiter.
- "Delete the container inside the repack worker" — the worker does file I/O only and has no catalogue/verify access; and an unjournaled in-worker delete reintroduces the rollback gap. Route through the net, journal a rebuild reverse.
- "Drain it as a separate delete and call it done" — that is exactly the shipped-forward / broken-rollback state recorded here. The reverse is mandatory before merge.
