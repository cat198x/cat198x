# Source disposition replaces the `--move` flag

**Status:** Accepted (2026-06-18).
**Scope:** Cat198x flagship — how the planner decides move vs copy, and the source schema.
**Related:** [agent-native-surface-and-ui.md](agent-native-surface-and-ui.md) (the plan engine the surfaces share); `../CLAUDE.md` safety model (verify-before-delete, plan→apply→rollback).

## Bottom line

Move-vs-copy stops being a per-invocation `plan --move` flag and becomes a **property of each source**: `consume` (staging — move content out and free it once placed) or `preserve` (reference — copy content out, leave the source intact). Disposition follows the directory's **role**: a *destination* directory (the library — where placed content lives, registered as a source only so the catalogue tracks it) is always `preserve`; a *source/staging* directory is `consume`. The planner derives move or copy per operation from the disposition of the source the file is read from. The `--move` flag is removed.

## Context

Cat198x is a *tidying* tool: its core job is to take content out of a staging area (`ToSort/…`) and place it canonically in the library. That is intrinsically a **move**. Yet `plan` defaulted to **copy**, with `--move` as the opt-in — so the everyday action required a flag, and forgetting it silently asked for a full duplication.

This bit hard. A plain `cat198x plan` over the live catalogue produced 92,098 copy-mode repacks needing ~290 GB on a volume with 263 GB free — the disk gate refused it. The same catalogue planned with `--move` became 50,064 free same-volume relocates + 42,034 repacks + 5,917 deletes, and **passed** the disk gate outright. The 290 GB "blocker" was an artifact of the wrong default, not a real constraint.

A global flag is the wrong home for this choice. Whether a source should be emptied is a stable fact about *that source* — `ToSort` always wants emptying; a reference TOSEC archive never does — not a decision to re-make on every `plan`. Putting it on the source removes the footgun, records the intent where it belongs, and makes the disk-space check honest by construction (a consumed same-volume source nets ~0 space; a preserved one genuinely costs space).

Move is already safe here: verify-before-delete, the rollback journal, and `delete_has_surviving_copy` refusing to destroy a last copy all hold. The risk a destructive default carries is contained by the plan→review→apply gate — a human (or agent) sees the moves and deletes before any file changes.

## Decision

### `disposition` on every source

Each registered source carries a `disposition`:

- **`consume`** — staging. Content may **leave the tree**: it is moved to its canonical destination and the source freed (loose files relocated/moved; loose repack sources deleted after the archive verifies). A consume tree can be emptied.
- **`preserve`** — its **content is never lost from the tree**. `preserve` is *not* "frozen" — internal reorganisation is allowed:
  - relocate a file to its canonical path **within the same tree**;
  - consolidate loose files into an archive **within the same tree**, deleting the now-redundant loose originals (the content lives on in the archive);
  - drop an intra-tree duplicate where another copy of the content **remains in the tree**.

  What `preserve` forbids is removing content the tree alone holds: a move/relocate that takes content **out** to a different tree (the planner **copies** instead, leaving the original), and a delete justified only by a copy in **another** tree.

The planner picks the operation per source from this: reading from a `consume` source it moves; reading from a `preserve` source it moves only when the destination is **within the same tree**, and otherwise copies. A delete against a `preserve`-tree file is allowed only when a surviving copy remains **in that same tree** — a stricter form of the existing `delete_has_surviving_copy` check, which today accepts a copy anywhere.

### The `--move` flag is removed

`plan` no longer takes `--move`; `PlanOptions.move_files` goes away, replaced by the per-source disposition the planner reads. `--prune-empty` on `apply` stays (it cleans the directories a consume tidy empties).

### Disposition follows role; destinations are always `preserve`

A directory's role determines its disposition:

- A **destination** — a path at or under a configured destination root (`default_dest`, or any collection's `dest_path`) — is **`preserve`**. The library holds placed content; consuming it would mean moving content *out* of the library, which is never what a tidy does. This is an **invariant**: setting a destination source to `consume` is refused.
- A **source/staging** directory — anything not under a destination — defaults to **`consume`**. That is the directory you registered to tidy.

A new source classifies by this rule at `source add` time. `--preserve` overrides it for the one case the rule can't infer: a non-destination directory you nonetheless want kept intact (a reference master you tidy *from* but don't want emptied). Setting an existing source: `source set-disposition <source> consume|preserve` (exact command name TBD in implementation), subject to the destination invariant.

### Migration of existing sources: `preserve` all, then mark staging `consume`

"Not under a destination" does **not** mean "staging." The live catalogue proves it: alongside `ToSort/…` (genuine staging) sit `WOS-Archive`, `Reference-PDFs`, `Magazines`, and `LadyEklipse` — non-destination **reference masters you tidy from but keep intact**. Auto-consuming every non-destination source would risk emptying those (WOS-Archive is Spectrum software that *could* match a DAT and be planned).

So the migration is safe-by-default and path-inference stays out of the schema step (per the drift trigger below): the schema migration sets **every existing source to `preserve`** — nothing becomes deletable silently. Then, as an explicit, visible step, the genuine staging sources are flipped to `consume`. For the live catalogue that is exactly the **nine `ToSort/*`** sources; the four reference masters above stay `preserve`. The operator sees precisely which sources become deletable.

## Boundaries (deliberate)

- This does **not** change the executor's primitives — only *which* primitive the planner chooses per op, plus one tightening: a delete against a `preserve`-tree file must verify a surviving copy **in the same tree**, not just anywhere.
- Every delete-bearing command, not only `plan`/`apply` — `clean-superseded`, `reclaim`, dedup — must honour the same `preserve` rule: it may drop an intra-tree duplicate, but never the last copy a `preserve` tree holds.
- `check_disk_space` already credits same-volume moves/relocates as 0. A separate refinement — crediting the space a consume repack frees when it deletes its loose sources — is **out of scope here**; the move-mode plan already passes the gate, so it is not blocking. Noted for later.
- Cross-volume consume is still a move (copy + verify + delete the source), so it costs transient space on the destination volume; the disk check accounts for that correctly.

## Drift triggers

Stop and re-consult this record if you find yourself:

- adding a `--move` / `--copy` flag, or any per-invocation move/copy toggle, to `plan` or `apply`;
- letting a **destination** directory be `consume` — a destination is always `preserve`; consuming the library moves content out of it;
- treating `preserve` as **frozen** — forbidding relocation or loose→archive consolidation *within* a preserve tree. `preserve` forbids losing content, not reorganising it;
- deleting a `preserve`-tree file when the only surviving copy is in **another** tree (allowed only when a copy remains in the same tree);
- auto-`consume`-ing non-destination sources in a **migration** because "they're not destinations" — not-a-destination ≠ staging (reference masters exist); migration is `preserve`-all then explicit `consume`;
- making the planner decide move-vs-copy from anything other than the source's stored disposition (e.g. re-inferring from path at plan time — role drives the disposition *value* at add/migration time, not the per-op choice);
- treating a library-internal relocate as a copy.
