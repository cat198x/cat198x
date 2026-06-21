# Draining consume staging containers after a repack

**Status:** **Shipped.** Forward drain + rollback coherence both implemented and tested. 2026-06-19.
**Scope:** Cat198x flagship — the planner's repack path, the verify-before-delete safety net, and the rollback journal.
**Related:** [source-disposition.md](source-disposition.md) (consume vs preserve, the delete rule); [concurrent-apply.md](concurrent-apply.md) (repacks run concurrently, deletes serial). Landed on branch `feat/drain-consume-containers` (WIP commit `19ef5a6`, rollback fix on top).

## Bottom line

A `consume` staging area (`ToSort/`) did **not** drain for archive sets that have to be *recompressed* into the library — e.g. TOSEC-ISO CD images. The content got copied into the library, but the original staging container was left behind, so the same bytes sat in two places (a real **~12 GB** for the TOSEC-ISO run on 2026-06-18). The fix now drains the container *and* journals a reverse that rebuilds it on rollback, so no command can lose content in either direction.

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

## How the rollback gap was closed

A copy-mode repack's journal reverse is `Delete dest`. A *plain* `Delete` carries no restoring reverse (deletes are not journaled), so on its own a drained container would be removed on rollback with the destination — losing content.

The drain now journals a **rebuild-container-from-`dest`** reverse. A container-drain is a `Delete` carrying an optional `ContainerRebuild` spec (`OperationKind::Delete { rebuild: Some(..) }`): for each entry the container held, *which* destination archive it was repacked into, its name there, the name it had in the container, and its SHA1. The forward path is unchanged — it still routes through the verify-before-delete net, which decides safety — but on a real removal it journals a `LoggedOperation::RebuildContainer` reverse. On rollback that reverse rebuilds the container by extracting each entry back out of its destination via `execute_repack` (SHA1-verified, partial archive removed on mismatch) and deletes nothing.

The ordering is what makes it coherent: drains are emitted *after* every repack, and the serial drain logs after the repack batch flushes, so the drain's journal entry sits **after** the repack's. Rollback runs in **reverse journal order**, so `RebuildContainer` runs **before** the repack's `Delete dest` — the container is rebuilt from the destination while the destination still exists, then the destination is removed. A container that fed several games spreads its entries across several destinations; the single `ContainerRebuild` names each entry's own `dest`, and because it is reversed first (emitted last) every destination is still present when the rebuild reads them.

This realises the *executor-internal coupling* the first approach had — reverse = rebuild-container-from-`dest` — but reuses the verify-before-delete net as the forward arbiter rather than guessing safety at plan time. Round-trip proven by `rolling_back_a_drained_container_rebuilds_it_before_the_destination_is_deleted` (the container is restored with its original in-container entry names and the destination is gone — which can only happen if the rebuild ran first).

## Reclaiming the interim space

The ~12 GB of redundant TOSEC-ISO staging copies were always safe (verified present in the library), just wasted space. They can now be reclaimed reversibly: re-plan and apply, and the drain removes them with a journaled rebuild reverse — a mistaken run is recoverable with `apply --rollback`.

## Drift triggers

- "Just guard on `!shared_containers`" — no; it flags single-game CD containers as shared and blocks the fix. The net, not a plan-time guess, is the arbiter.
- "Delete the container inside the repack worker" — the worker does file I/O only and has no catalogue/verify access; and an unjournaled in-worker delete reintroduces the rollback gap. Route through the net, journal a rebuild reverse.
- "Drain it as a plain delete" / "drop the `ContainerRebuild` spec" — that reintroduces the broken-rollback state this work closed: a drained container with no reverse is removed on rollback alongside its destination, losing content. The rebuild reverse is load-bearing; the `rolling_back_a_drained_container…` test guards it.
