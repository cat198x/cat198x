# Next session — build kickoff

Handoff after the MAME 0.288 split reorg (2026-06-17). Open this repo
(`cat198x/cat198x`) and start here. Update or delete this file once Task 1 lands.

**Read first:** [`declarative-reconciliation.md`](declarative-reconciliation.md)
(the chosen cleanup model) and
[`agent-native-surface-and-ui.md`](agent-native-surface-and-ui.md)
(the surface/UI direction).

## Task 1 — same-source canonical-keep cleanup command

Remove loose files stranded under the library beside their canonical per-machine
zips. Honour the four-condition removal predicate **exactly**:

1. the file's content sits in the canonical archive for its machine at the
   expected destination path (a match against that specific archive, not a bare
   same-SHA1-exists-somewhere check);
2. that canonical archive is a current desired-state member of the collection;
3. the file is **not** a current desired-state member of any other in-scope
   collection; and
4. the surviving copy is re-verified on disk at delete time.

Report-by-default, `--execute`-gated, journaled — match the `reclaim` /
`prune-empty` conventions. Do **not** reintroduce the unsound predicate the
decision doc documents (deleting content canonical under another collection).

**Acceptance:** a dry-run on the live catalogue reports the MAME loose layer
correctly — ~38.79 GB / 42,061 files removable, ~42.88 GB / 13,519 version-gap
orphans left untouched (they sit in no canonical archive). Then `--execute`
frees the 38.79 GB.

## Then

The MCP surface (`cat198x mcp`), then the first Tauri slice (status +
plan-diff) — per `agent-native-surface-and-ui.md`. Build the MCP surface before
any pixels.

## Cadence

One change per commit; tests at the right level; conventional commits; land via
PR.

## Gotchas (from the reorg session)

- Catalogue at `~/.cat198x/db.sqlite`; paths are stored **relative** to the
  source root (absolute-path SQL queries silently match nothing).
- Binary at `target/release/cat198x`.
- The Time Capsule is a flaky AFP mount — drive long ops through a
  re-plan→apply resume loop so drops do not lose progress.
- In the multi-repo container, use `git -C <repo>` so commits land in the
  sub-repo, not the umbrella.
