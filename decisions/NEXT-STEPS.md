# Next session — handoff

Open this repo (`cat198x/cat198x`) and start here. Handoff after the
confirm-gated real apply + the `preserve` delete-rule landed (2026-06-19).

**Read first:** [`source-disposition.md`](source-disposition.md) (the move/copy +
delete model the planner now uses — especially *the delete rule* and *Drift
triggers*) and [`agent-native-surface-and-ui.md`](agent-native-surface-and-ui.md)
(one library surface, three adapters — CLI / MCP / Tauri).

## Where things stand

The apply pipeline is now **complete and mutating** — the dry-run preview became
a real, confirm-gated apply, and every delete-bearing path honours source
disposition. All merged to `main`:

- **Source disposition** replaces the `--move` flag. Every source is `consume`
  (staging — may be moved out and freed) or `preserve` (content never lost from
  the tree). The planner derives move/copy per source. The live catalogue is set:
  the 9 `ToSort/*` are `consume`, the 16 library/reference sources `preserve`.
  A plain `cat198x plan` **moves** staging content into place (≈50k relocates +
  42k repacks + 6k deletes, 263 GB, fits the disk). DB backup at
  `~/.cat198x/db.sqlite.bak-predisposition-2026-06-18`.
- **Apply engine** lives in the library: `plan::apply_plan(...)` runs the loop and
  reports `ApplyEvent`s; the engine prints nothing. CLI / UI / MCP are thin
  callers.
- **Gates + real quarantine store** (PR #42). `ops::apply_streaming` enforces the
  staleness and disk gates on a real apply (refuses, touches nothing — returns
  `ApplyReport.refused`), and resolves the configured quarantine store via
  `config::resolve_quarantine_dir` (the CLI resolver delegates to it). New
  `ops::ApplyRunOptions { dry_run, skip_space_check }`; a *started* plan resumes
  through the staleness gate, only a fresh stale plan is rejected.
- **Confirm-gated real apply in the UI** (PR #43). `apply_execute` (`dry_run =
  false`) is a separate Tauri command from `apply_stream` so a preview can never
  mutate. The Preview tab offers an **Apply…** button only when work would run
  (fits on disk; fresh non-stale plan *or* a started one to resume); clicking it
  reveals an explicit confirm ("Apply N operation(s), moving ~X?") that mutates
  only on Confirm. Resume is first-class: a partial run shows "N done, M
  remaining — Apply again to resume" and re-runs `apply_execute`.
  `ApplyReport.total_bytes` feeds the confirm.
- **The `preserve` delete-rule** (PR #44). `delete_has_surviving_copy` now
  requires a **same-tree** (same source) survivor for a `preserve` file — a copy
  in another tree no longer authorises the delete (consume unchanged). The
  planner consolidates and dedups **within** a preserve tree (`may_delete(root,
  survivor_dest)`: consume always; preserve iff `dest_under`), so the library's
  loose→archive repacks now consume their loose originals; cross-tree still
  copies and keeps. `reclaim` refuses `preserve` sources (its cross-tree model is
  forbidden there). `clean-superseded` inherits the tightened net unchanged.

## Not yet done — the live run

Everything above is verified **headless** (429 lib tests, clippy, fmt, UI build)
but has **not** been run against the live 263 GB catalogue, which is the one
remaining proof. The mutating click is deliberate, not automated.

When running it for real, prefer a **per-set apply first** (smaller blast
radius): `cat198x plan --set <SET>` then apply that scoped plan, before a whole
-library run. The executor net refuses any cross-tree `preserve` delete
regardless, and apply is resumable over the flaky AFP mount (run again to resume
the pending ops).

## Candidate next tasks (pick by need, not order)

- **Per-set apply from the UI.** The UI applies the whole latest plan; there is
  no UI affordance to generate/apply a scoped `plan --set X`. Adding it gives the
  smaller-blast-radius first run a home in the UI (the handoff for #43 deferred
  this). Needs a plan-generation command exposed to the UI.
- **Disk-credit refinement.** `check_disk_space` does not yet credit the space a
  consume repack frees when it deletes its loose sources (noted out-of-scope in
  `source-disposition.md` § Boundaries). Not blocking — the move-mode plan passes
  the gate — but it would make the estimate honest for tighter volumes.
- **MCP adapter for apply.** The library surface (`ops::apply_streaming`) is
  adapter-ready; an MCP tool would give an agent the same gated apply the CLI and
  UI have (agent-native parity — see `agent-native-surface-and-ui.md`).

## Cadence

One change per commit; tests at the right level; conventional commits; land via
PR. Build the tested library/ops change first (headless), then the UI on top.

## Gotchas

- Catalogue at `~/.cat198x/db.sqlite`; paths stored **relative** to the source
  root (absolute-path SQL silently matches nothing). Binary at
  `target/release/cat198x`; the UI binary at `ui/target/debug/cat198x-ui`.
- The Time Capsule is a flaky AFP mount — long ops (a real apply!) drop; rely on
  apply's resume (run again) rather than expecting one clean pass.
- Multi-repo container: `git -C <repo>` (or `cd` in the same call) so commits
  land in the sub-repo, not the umbrella. Commits are 1Password-signed; the agent
  re-locks between sessions, so a fetch/push may need an unlock.
- The UI crate is outside the workspace — build/clippy/test it separately in
  `ui/`. It loads `frontend/` assets at compile time, so a frontend-only change
  still needs a `cargo run`/rebuild to show up. Verify CLI output is unchanged
  when refactoring `apply` by diffing `apply --dry-run --skip-space-check`
  old-vs-new.
- "Same tree" in the delete rule means **same registered source**. The library is
  one `preserve` source, so consolidation and `clean-superseded` survivors
  resolve in-tree; nested sources under the library would read as separate trees.
