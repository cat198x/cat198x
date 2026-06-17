# Cleaning superseded content under the library

**Status:** Accepted (downscoped after adversarial review, 2026-06-17).
**Scope:** Cat198x flagship — the `src/plan` reorganiser and its cleanup paths.
**Supersedes:** the roadmap's "same-source canonical-keep reclamation" follow-up.
**Related:** [agent-native-surface-and-ui.md](agent-native-surface-and-ui.md) — the UI does not depend on the deferred model below.

## Bottom line

A re-layout (e.g. MAME loose→split-zip) leaves the previous layout's files stranded under the library, and the planner cannot remove them: it is additive (it computes what to ADD to satisfy the active DAT), and the `is_in_library()` guard blocks deleting anything under a destination root. We will **fix the case in front of us with a small, bounded capability — same-source canonical-keep cleanup** — and **defer** the larger "declarative desired-state reconciliation" model until a concrete need the small fix cannot express actually arrives.

An earlier draft of this record proposed adopting the full reconcile model now. Adversarial review (appendix) showed three problems: the model was sized to a headline that included version-gap residue it does not solve; a simpler fix covers the cases we have; and the draft's removal predicate was **unsound** — a data-loss path. This record keeps the insight and right-sizes the decision.

## The case in front of us

The MAME 0.288 split reorg placed 41,712 per-machine zips and left the pre-switch loose layer under `Library/ROMs/MAME/<machine>/`: 55,580 files. 42,061 of them (38.79 GB) are content-identical to entries in the new zips; 13,519 (42.88 GB) are 0.283 version-gap content the split layout does not contain. The redundant 38.79 GB should go; the 42.88 GB must stay — it exists nowhere else. The same shape appeared at FBN (flat→machine) and the TOSEC staging merge.

Root cause, grounded in the code: placement is **derived at plan time, never stored** — the catalogue is content-addressed, not placement-addressed (`src/db/files.rs`; `find_matched_roms` generator.rs:913). Convergence is additive: "No operations needed" means "everything the DAT wants is placed," not "the library equals the desired layout" (generator.rs:550-577, 681-712). And `is_in_library()` (generator.rs:1102-1108) forbids deleting under a destination root because the planner cannot tell a canonical placement from incidental content — so superseded layers are never collected.

## Decision 1 — same-source canonical-keep cleanup (do now)

Add a bounded capability: re-scan the library, then remove a loose file under the library **only when all of these hold**:

1. its content sits in the canonical archive for its machine at the expected destination path — a content match against that specific archive, not a bare same-SHA1-exists-somewhere check;
2. that canonical archive is itself a current desired-state member of the collection;
3. the file being removed is **not** a current desired-state member of any other in-scope collection; and
4. the surviving copy is re-verified on disk at delete time (existing verify-before-delete + fsync, `delete_has_surviving_copy` apply.rs:28-66).

Conditions 2 and 3 are the correction the review forced. The earlier predicate ("absent from this collection's desired state AND content preserved canonically") would delete a file that is canonical under *another* collection, because `delete_has_surviving_copy` proves a SHA1 survives but not that the survivor is wanted. The blunt `is_in_library()` guard is what currently prevents that cross-placement deletion; this capability replaces it only where conditions 1-4 prove the removal safe, and **reports** content of unknown provenance instead of deleting it.

The 42.88 GB version-gap orphans fail condition 1 (their content is in no canonical archive) and are left alone — the discrimination we want, for free.

Ship it reported-by-default, `--execute`-gated, and journaled, consistent with `reclaim` and `prune-empty`.

## Decision 2 — defer declarative reconciliation (record, do not build)

The broader model — store desired state, give placements provenance, make `apply` a full add/move/**remove** reconcile — is recorded as a future option, not adopted. The review surfaced why:

- **Oversized.** It was motivated by 81.7 GB of residue, but only the 38.79 GB redundant half is the architectural gap; the 42.88 GB is DAT-version churn the model quarantines instead of resolving.
- **Costly new state.** The one genuinely new piece — per-placement provenance — cannot be backfilled cleanly from the named mechanism (`catalogue-placements` has no collection dimension and skips `Relocate`), and a stored assertion drifts from disk in a way content-addressing never does.
- **The UI does not force it.** A Tauri UI renders the existing plan as a diff (the plan already *is* the diff); it needs the plan/apply surface made consumable, not a reconcile model. See [agent-native-surface-and-ui.md](agent-native-surface-and-ui.md).

**Trigger to revisit:** a concrete, recurring case that neither Decision 1 nor cross-source `reclaim` can express — for example reconciling moves/renames across a whole set, or enforcing "the library contains exactly this and only this." Until then, this stays deferred.

## Consequences

- The cleanup in front of us ships as a small, safe, tested command — one change, one commit.
- The unsound removal predicate is written down so it is not reintroduced.
- The reconcile insight is preserved with a clear trigger, honouring "solve the problem in front of you" without losing it.

## Appendix — adversarial review findings (2026-06-17)

Condensed; full review in session history.

1. **Critical / data-loss:** the original removal predicate lacked a cross-collection conjunct and retired the one guard preventing cross-placement deletion. → fixed in Decision 1, conditions 2-3.
2. **Oversized diagnosis:** bundled a real architectural gap (redundant residue) with version-gap churn the model does not solve. → Decision 2.
3. **Simpler fix unexamined:** existing primitives (`reclaim`, move-mode dedup, quarantine `set_removed`) plus a same-source-canonical step cover the real cases; cross-source `reclaim` correctly declines the version-gap orphans. → Decision 1.
4. **Provenance not cleanly backfillable** from `catalogue-placements` (no collection dimension, skips `Relocate`); stored state drifts. → Decision 2 (deferred).
5. **Phase independence overstated:** a provenance phase would ship unverifiable, and the staleness hash (`compute_state_hash`) may not invalidate on a layout-only change. → folded into Decision 2's risks.
