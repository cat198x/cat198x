# Cat198x Specification

**Status: DRAFT — the store-and-export direction, 2026-08-28.** Supersedes the pipeline-and-phases specification. Not yet reflected in the code.

## What Cat198x is

A package manager for ROM collections.

You tell it which sets you want to be complete for. It tells you what you have and what you are missing, and it builds you a romset in whatever shape you need — split or merged, one zip per machine or one zip per romset, from the ROMs already on your disk.

## The model

Five nouns.

**Source** — a directory Cat198x reads. Every source has a **disposition**: `consume` (staging; content may be moved out and the source freed) or `preserve` (reference; content is copied out and the source left intact). Disposition follows the directory's role and is a property of the source, never a per-command flag.

**Store** — every ROM Cat198x knows about, addressed by content hash, wherever it physically sits. A ROM inside a zip is in the store; so is a loose CHD. The store is not a layout.

**Collection** — a DAT at a version. It defines what *complete* means for some set. A collection is a lens over the store, not a container in it.

**Tree** — collections are addressable by path, not only by name. TOSEC ships its own
hierarchy as directory structure, and Cat198x adopts it rather than inventing one.

**Target** — a directory Cat198x materialises a romset into and keeps current: your emulator's ROM folder, a share, a drive you hand to someone. A target is a *view* of the store in a chosen shape.

### The store is a superset of every manifest

This is the load-bearing rule.

The manifest says what you want to be **complete for**. It never says what you are permitted to **keep**. Content that no active DAT claims — an unidentified dump, a version you no longer track, a homebrew nobody catalogued — stays in the store and is reported, never removed.

**No desired state can imply a deletion.** Completeness is a report, not a boundary. An archive that deletes what it cannot identify is not an archive.

Today the catalogue holds 58,230 hashes claimed by no DAT. That number is expected to be non-zero forever.

### A target is rebuildable; the store is not

The store is the only copy of what it holds. A target can always be rebuilt from it.

That asymmetry is the safety model. Cat198x may delete freely inside a target — the worst case is rebuilding it. It may never delete freely from the store, because nothing backs the store up.

**Never export into the store.** Doing so collapses the two, and then tidying a view can remove the only copy of something.

## The manifest

```toml
library = "/Volumes/Data/Library/ROMs"

[collections.mame]
dat     = "MAME/MAME ROMs (split)"
version = ["0.283", "0.288"]

[collections.spectrum]
dat     = "TOSEC/Sinclair/ZX Spectrum/Games"
version = "2023-06-14"

[collections.whdload]
dat = "WHDLoad"

[sources]
consume  = ["/Volumes/Data/ToSort/*"]
preserve = ["/Volumes/Data/Magazines", "/Volumes/Data/WOS-Archive"]

[targets.mame]
collection = "mame"
path       = "/Volumes/Emulation/mame/roms"
merge      = "split"
container  = "game"

[targets.snes-play]
collection = "no-intro/snes"
path       = "/Volumes/Emulation/snes"
container  = "game"
select     = "1g1r"
regions    = ["eu", "us", "jp"]
exclude    = ["beta", "proto", "demo"]

[targets.spectrum-archive]
collection = "spectrum"
path       = "/Volumes/Data/Sets"
container  = "dat"          # one zip per collection, Pleasuredome-style
```

A collection is named by its **tree path**, and naming a group node selects everything
beneath it: `dat = "TOSEC/Commodore/C64"` takes every C64 collection.

Collections are one to three lines. Anything the manifest grows beyond a DAT, a version and a source list is complexity being chosen.

**Pinning several versions is cheap.** Measured across MAME 0.283 and 0.288: 154,900 of 154,957 ROMs are shared, and holding both costs **0.19 GB** of content unique to the older one. Version churn is not the expensive axis. Container shape is.

## Verbs

| Verb | Does | Today |
|---|---|---|
| `scan` | Hash a source and record what is there | built |
| `status` | Completeness per collection, merge-mode aware | built |
| `have` / `missing` | The lists, as txt, csv or json | built as `export --have` |
| `import` | Bring content from a source into the store | built as `plan` + `apply` |
| `export` | Materialise a romset into a target, or a one-off shape | **new** |
| `verify` | Re-hash the store and report what changed | partial |
| `remove` | Delete content, explicitly | built as `reclaim`, `clean-superseded` |

Six verbs and one dangerous one. If a seventh is proposed, ask which repair it is performing and whether the model should have made the damage impossible.

## Export

The headline capability, and the reason layouts are never stored.

```
cat198x export mame --merge split --container per-machine --format torrentzip --to /media/out
cat198x export tosec/spectrum-games-tzx --container per-romset --format zip
cat198x export mame --only mslug,neogeo --to ./slice
```

Three independent axes:

- **merge** — `split`, `merged`, `non-merged`. How parent and clone content is distributed.
- **container** — `per-machine`, `per-romset`, `single`, `loose`. Pleasuredome-style one-zip-per-romset is `per-romset`; it is a choice here, not a different tool.
- **format** — `zip`, `torrentzip`, `loose`, and per-content-type exceptions below.

Properties that matter:

**Deterministic.** The same request twice produces byte-identical output. `torrentzip` is the default for that reason: an export can be verified rather than re-transferred, and two people can compare results.

**Partial by default is supported, not assumed.** `--only` takes machines, systems or a romset. A full export of a large set is a second copy of it, so exporting a slice must be a first-class request rather than a workaround.

**Resumable.** An interrupted export continues; already-written members are verified rather than rebuilt. This falls out of determinism.

**Reads from wherever content lives.** The store indexes ROMs inside archives, so an export re-containers from existing archives without a loose intermediate copy.

**Targets are maintained, not regenerated by hand.** A declared target is brought up to date by `export` with no arguments — new content added, content that left the collection removed from the view. Removing from a view is safe by definition: the store still holds it.

An existing romset can be adopted as a target where it stands. A library that is already 41,712 split zips does not need moving; it is declared, indexed, and kept current in place.

### Materialising a target

Updating a target is per-file, not per-directory. Determinism does most of the work: a
deterministic export produces byte-identical output for anything that has not changed, so
a rebuild writes only what differs. Across five MAME releases that is 57 ROMs of 155,000.

Each file that does change is written to a temporary name and `rename()`d into place, so a
target is never half-updated and never needs double its own size to refresh.

For content that must be written, prefer in this order:

1. **Reflink** — `clonefile()` on APFS and HFS+, `FICLONE` on btrfs and XFS. No extra space,
   and copy-on-write means a modified target file cannot write back into the store. This is
   the only linking mode that preserves the store/target asymmetry, so it is the default
   where the filesystem supports it.
2. **Hardlink** — same space saving, but two paths to one inode. Deleting a target file is
   safe; modifying one in place is not. Used only where reflink is unavailable and the
   target is understood to be read-only.
3. **Copy** — always correct, and what a different filesystem gets: an external drive, a
   network share, a set being handed to someone.

Linking only helps where the bytes already exist in the shape the target wants — a whole
archive the store already holds in that exact form. A target in a different merge mode or
container granularity is constructing new archives, and no linking scheme avoids that.

### The collection tree

A flat list of 3,982 TOSEC collection names is not addressable. The distribution already
carries a hierarchy in its own directory layout, and the tree is derived from it:

```
TOSEC / Commodore / C64 / Games / Arcade / [D64] / <dat>
TOSEC / Acorn / Archimedes / Compilations / Games / [ADF] / <dat>
TOSEC / ACT / Apricot PC-Xi / Demos / <dat>
```

Three distributions sit at the root — `TOSEC` (2,608 dats), `TOSEC-ISO` (271) and
`TOSEC-PIX` (1,103, scans rather than ROMs). Between the root and the dat run zero to
three grouping segments. A segment in square brackets is a **format**; every other
segment is a **group**. That rule is syntactic and holds across all 3,982.

**The tree is positional.** Nodes are `group` or `dat`, and nothing more is claimed. It is
tempting to label the levels manufacturer, system and category, and for most paths that
would be right — but `Acorn/Magazines/Acorn User` and `Multi-format/DVD/Games` put a
category where a machine is expected, and 357 of 3,982 paths disagree with that reading.
Positional addressing works for all of them. Semantic labels can be added later where they
verify; a wrong label cannot be withdrawn from a manifest someone has already written.

A tree may be flat. MAME's three collections sit directly under their root, and the same
addressing applies to a depth of one.

The tree derives from `collection_versions.dat_path`, which is already stored, so
populating it parses strings rather than rescanning content.

Addressing any node selects everything beneath it:

```toml
[collections]
"TOSEC/Commodore/C64"                    = "2023-06-14"   # every C64 dat
"TOSEC/Sinclair/ZX Spectrum/Games"       = "2023-06-14"   # games only
"TOSEC/Acorn/Archimedes/Compilations/Games/[ADF]" = "2021-12-11"
```

### Containers

Container granularity is a **level in the tree**, not a pair of modes:

- `container = "game"` — one archive per game. The layout an emulator expects.
- `container = "dat"` — one archive per collection. The Pleasuredome convention, and the
  reason `Sinclair ZX Spectrum - Games - [TZX]` is a single zip of 19,182 entries.
- `container = "group"` — one archive per named group node, for coarser sets.

A romset needs no user-supplied name: its identity is its tree path, and the archive name
is the collection's own name, mapped to a filename by a documented and frozen rule.
Collection names contain apostrophes and brackets that are legal in the name and awkward in
a filename, and a target's filenames are its identity across updates — changing the
sanitiser later renames everything it has already written.

### Adopting an existing target

An existing romset is declared, indexed and kept current in place. Whether that needs a full
re-hash is unresolved, and deliberately unbuilt.

`file_locations` stores `sha1`, `path`, `archive_path` and `last_seen` — no size, no mtime.
There is no stat data to compare against, so nothing cheaper than hashing exists today.

Before adding columns, measure. A target is local and far smaller than the store; if hashing
one of 41,712 archives takes minutes, a staleness heuristic is complexity buying nothing.

If measurement says otherwise: adopt on `(path, size, mtime)`, hash whatever fails to match,
and **always hash in full before the first destructive operation on a target**. Cat198x may
delete inside a target, and that freedom must not rest on mtime, which lies after a backup
restore, a `cp -p`, or an rsync carrying times across.

### Target state

A target records what it contains, in a file inside the target, so a drive handed to
someone carries its own provenance.

Re-deriving instead would be safe only if an export were a pure function of collection
version, policy and store. It is not: `1g1r` resolves against *what the store holds*, so
acquiring a better regional dump silently changes which ROM the same policy picks. Without
a record, the file in the target changes and nothing says a decision changed.

The file holds two things of different standing:

- **Resolved selection** — which hash satisfied which entry, under which policy. Not
  re-derivable once the store moves on. This is the reason state exists, and what lets
  `plan` report *a different dump now wins* rather than silently swapping a file.
- **Placement map** — what sits at which path, by link or copy. A cache.

Unlike Terraform, whose state is authoritative because cloud resource identifiers are
opaque, a Cat198x target is self-describing: every file in it is content-addressed, so the
placement map can always be rebuilt by hashing the directory. Losing state costs a rescan
and a warning that prior resolutions were lost, never the target.

### Selection

Curated sets are **named policies, never a filter language**:

- `select = "all"` — everything the collection names. The default.
- `select = "1g1r"` — one release per game, resolved by `regions` in preference order.
- `exclude = [...]` — release tags: `beta`, `proto`, `demo`, `alpha`, `sample`.

When a request cannot be expressed that way — and eventually one will not be — the answer is not a bigger vocabulary:

```
cat198x export snes --from-list my-picks.txt
```

A list of game or ROM names, produced however the user likes. This escape hatch exists so
the selection vocabulary can stay small: every unusual request has somewhere to go that is
not the manifest.

**A selection you want to reproduce next year is a collection, not a list.** A flat list of
names fails silently: when the DAT version bumps and entries are renamed or split, the
unmatched ones simply do not export, and the set quietly shrinks. A curated set with a name,
a version and a membership is what a collection already is — `source_type = 'custom'`, of
which 743 exist holding 114,352 games.

Generate a custom collection as a **filtered subset of its parent DAT**, keeping the parent's
entries rather than only the names. A list can only describe content you already hold; a
subset keeps the hash entries for content you do not, so HAVE and MISSING keep working, and a
parent version bump shows up as a diff instead of a shrink.

`--from-list` is therefore how a custom collection is *created*, not a way of living in one.
The curated CHD subset is a custom collection.

### Dependencies

A split export of a machine is incomplete without its BIOS and device sets. The DATs carry 178 BIOS and 2,314 device entries; `dat_game_devices` exists in the schema and holds **0 rows**, so this is unimplemented rather than designed away.

Export resolves dependencies and includes them by default. `--no-deps` is available and warns. An export that silently produces a set which cannot run is the worst failure this tool has.

### Content that does not containerise

CHDs are stored and exported loose, matched on internal-header SHA1. This is a property of the content type, not a flag: `--format zip` on a CHD-bearing machine produces a zip of its ROMs beside the loose CHD, and needs no flag.

Any future type with the same shape is added here, not as another option.

## Sources without a DAT

WHDLoad, magazine archives, reference PDFs and similar are `preserve` sources with no DAT and no completeness question. They are stored, hashed, deduplicated and searchable. They are not collections and no `status` is reported for them.

Forcing them into the collection shape is how a fourth noun gets in.

## Removal

The only operation that can lose data, and the only one that is never a consequence of anything else.

- Never implied by desired state, a version change, or an export.
- Always explicit, always dry-run unless `--execute`, always journalled with a reverse operation.
- A file is removable only when **something else still references its content and that something is wanted** — not that the hash exists somewhere. Proving a SHA1 survives does not prove the survivor is wanted; that distinction is the difference between a safe delete and a data-loss path.
- Content in the store claimed by no collection is reported, never removed.

## What this replaces

Four current commands exist to repair damage the storage model causes:

- `catalogue-placements` — placements are derived, never stored
- `clean-superseded` — a layout change duplicates content
- `prune-empty` — moves leave empty directories
- `reclaim` — staging duplicates the library

Under store-and-export the first two have nothing to repair: layouts are not stored, so they cannot be superseded, and the store is content-addressed, so there are no placements to record. `prune-empty` shrinks with the number of moves. `reclaim` survives, as draining a `consume` source.

## Not specified here

**Database schema.** The code is the schema; duplicating DDL into prose guarantees drift.

**Header-aware matching.** `sha1_no_header` exists in the schema and is populated on 0 files. Needed when NES-family content arrives; not designed here.

**Concurrency.** Export is latency-bound and will want parallel placement with serial safety steps. See `decisions/concurrent-apply.md`, which specifies it and is unbuilt.

## Open questions

- Does the format segment (`[D64]`, `[ADF]`) belong in the tree as its own level, or as an
  attribute of the dat node? 2,136 of 3,982 collections have none.
- Do the semantic level labels get added later as verified annotations, and what verifies them?
- Is hashing a target fast enough to make the adoption heuristic unnecessary?
