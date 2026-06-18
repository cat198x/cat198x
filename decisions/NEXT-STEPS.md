# Next session — handoff

Open this repo (`cat198x/cat198x`) and start here. Handoff after the
source-disposition redesign + apply-preview pipeline (2026-06-18).

**Read first:** [`source-disposition.md`](source-disposition.md) (the move/copy
model the planner now uses) and
[`agent-native-surface-and-ui.md`](agent-native-surface-and-ui.md) (one library
surface, three adapters — CLI / MCP / Tauri).

## Where things stand

Landed this session (all merged to `main`, catalogue reconfigured):

- **Source disposition** replaces the `--move` flag. Every source is `consume`
  (staging — may be moved out and freed) or `preserve` (content never lost from
  the tree). The planner derives move/copy per source. The live catalogue is set:
  the 9 `ToSort/*` are `consume`, the 16 library/reference sources `preserve`.
  A plain `cat198x plan` now **moves** staging content into place (≈50k
  relocates + 42k repacks + 6k deletes, 263 GB, fits the disk) instead of asking
  for a ~290 GB copy. DB backed up at `~/.cat198x/db.sqlite.bak-predisposition-2026-06-18`.
- **Apply engine** lifted into the library: `plan::apply_plan(conn, &mut plan,
  plan_path, sources, opts, on_event)` runs the loop and reports `ApplyEvent`s;
  the engine prints nothing. `cli/apply` is a thin caller.
- **Apply preview** in the UI: `ops::apply` / `ops::apply_streaming` drive a
  dry-run and report readiness (stale, disk) + an op tally; the "Preview apply"
  tab animates a **live progress bar** off the event stream. **All dry-run —
  nothing mutates yet.**
- `doctor` detects + nests collections colliding on a destination root; CI's UI
  job hardened against the apt-mirror stall.

## Task — confirm-gated real apply (the mutating slice)

Flip the dry-run preview into a real apply behind an explicit confirm. The code
is small (it reuses the verified engine + the existing progress bar), but it is
**the one operation that mutates data** — treat it with care.

### Prerequisites — make `ops::apply_streaming` correct & safe for `dry_run = false`

Today it is only ever called with `dry_run = true`, so two shortcuts must be
fixed before a real apply:

1. **Resolve the real quarantine store.** It currently hardcodes
   `quarantine_dir: data_dir.join("quarantine")` (fine for dry-run, where
   quarantine never executes). A real apply must resolve the configured store —
   `config.quarantine_dir` or `data_dir/quarantine` (mirror
   `cli::config::resolve_quarantine_dir`, but in the library, since `ops` can't
   call `cli`). 
2. **Enforce the gates.** The preview only *reports* `stale` / `disk_ok`; a real
   apply must **refuse to mutate** when the plan is stale (catalogue moved since
   it was generated — exactly the CLI's check) or when it won't fit (unless an
   explicit `skip_space_check`). Return a refusal the UI can show, don't apply.
   The CLI's `cli/apply::run` is the reference for both checks (staleness allows
   a *started* plan to resume; only a fresh all-pending plan is rejected).

Add tests: a real apply on a tiny tempdir plan actually moves the file + writes
the journal; a stale plan refuses without touching anything.

### UI

- A `apply_execute` Tauri command (`dry_run = false`), **separate** from
  `apply_stream` so a dry-run can never accidentally mutate. Streams the same
  `apply-progress` events to the same bar; returns the final report.
- A **confirm gate**: from the preview (op count + GB), show an explicit confirm
  ("Apply N operations, move/free X GB?") that **defaults to doing nothing** —
  the click is the authorisation. Only then invoke `apply_execute`.
- Apply is **resumable** (per-op status, journaled) and the mount is flaky AFP,
  so a drop mid-apply leaves a consistent partial state; running apply again
  resumes the pending ops. Surface that ("N done, M remaining — Apply again to
  resume") rather than treating a partial run as failure. Consider offering a
  **per-set** apply first (smaller blast radius) — `plan --set X` then apply that
  scoped plan — before a whole-library 263 GB run.

### Safety invariants (do not weaken)

Verify-before-delete + fsync, the rollback journal, `delete_has_surviving_copy`
refusing to destroy a last copy, and quarantine-not-delete all live in the
executor and stay exactly as they are. The real apply adds a *gate*, not a new
mutation path.

## Then — PR 3: the `preserve` delete-rule

Currently a `preserve` source is never deleted from (PR 2 was conservative), so
intra-tree duplicates linger — e.g. 6 repacks that build a canonical archive
from loose library sources leave those loose originals behind. Implement the
decision's full rule: allow **intra-tree dedup** and **loose→archive
consolidation** (drop a duplicate when a copy survives **in the same tree**)
while never removing the last copy a `preserve` tree holds. Touches the planner's
delete/`move_sources` decisions, and tightens `delete_has_surviving_copy` +
`clean-superseded` / `reclaim` to require *same-tree* survival for a preserve
file. The riskiest area (delete safety) — verify hard. See
[`source-disposition.md`](source-disposition.md) § the delete rule + Drift
triggers.

## Cadence

One change per commit; tests at the right level; conventional commits; land via
PR. Build the library/ops change first (tested headless), then the UI on top.

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
  `ui/`. Verify CLI output is unchanged when refactoring `apply` by diffing
  `apply --dry-run --skip-space-check` old-vs-new (how PR #35 was checked).
