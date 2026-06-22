# Decision: Multi-format hash storage (the `hashes` table)

**Status:** **Deferred — not being built.** Investigated against the live
catalogue 2026-06-22 and found all-cost / no-benefit today (see *Why deferred*).
The design below is kept as the shape to use **if** a real format need ever
lands; until then the wide `files`/`dat_roms` hash columns stay.

**Date:** 2026-06-22.

**Scope:** Cat198x catalogue schema — how a content's hashes are stored and
matched. Distinct from, and **decoupled from**, the oversized-collection planner
fix (see [declarative-reconciliation.md](declarative-reconciliation.md) and the
dedup-index work), which needs no schema change.

## Why deferred (the evidence that killed it, 2026-06-22)

Two motivations were on the table — MD5 *matching* and *speed*. The live
catalogue refutes both:

- **No match benefit.** **Zero** `dat_roms` are MD5-only: every ROM carrying an
  MD5 also carries SHA1/CRC32, so the planner already matches all of them. The
  998 ROMs with no SHA1 *and* no CRC32 have no MD5 either — unmatchable by any
  hash. So an MD5 match path (tall table or a one-line branch) gains **nothing**.
  TOSEC — the apparent reason MD5 mattered — carries all three hashes and is
  already fully matched on SHA1.
- **No speed benefit — the opposite.** Matching already runs on **covering
  indexes** (`EXPLAIN QUERY PLAN`: SHA1 and CRC32+size are pure index probes, no
  table access). A tall `(algo, digest)` table would be **~3× the rows**
  (5,321,313 vs the wide `files` table's 1,773,771), a bigger index with worse
  cache locality, plus an extra join hop and an `algo=?` discriminator on every
  lookup. It would be **slower**, not faster. The real costs in the workflow are
  I/O (scanning/applying over a network mount), which no schema change touches.

A migration + re-scan of a multi-GB catalogue, rewiring every match site, for
zero current benefit and a likely slowdown, is exactly the trade the project's
values ("solve the problem in front of you; no abstractions for hypothetical
futures; rescue beats replace") exist to refuse. **Revisit only when a concrete
need arrives** — a SHA256-only or MD5-only DAT actually appears — at which point
adding it is one `ALTER TABLE` plus a match branch, done against real data.

## Bottom line (the design, if it is ever revived)

DATs identify ROMs by **different hash formats**, already live in the catalogue.
If a persistent hashes layer is ever needed it should be a
**single tall table** keyed on `(algo, digest, size)` with the format as a
**column**, not separate per-format tables and not a string-prefixed key. Until a
real need lands (MD5 matching for TOSEC; SHA256 for future DATs), nothing is
built — the wide `files`/`dat_roms` columns stay as they are.

## The facts that drive it (measured against the live catalogue, 2026-06-22)

Across 2,325,464 `dat_roms`: ~99.9% carry CRC32, ~94% SHA1, ~54% (1,254,222) MD5.
Hash format is **per-DAT, and non-uniform**:

| Source | Hashes carried | Notes |
|--------|----------------|-------|
| **MAME** ROMs (`all_non-zipped_content`, `MAME ROMs split/merged`) | **CRC32 + SHA1** | No MD5. 100% populated. |
| **MAME** CHDs (`MAME CHDs (merged)`) | **SHA1 only** | No CRC32. (Old CHD v3/v4 headers carried MD5; v5 dropped it for SHA1 — hence the lingering "MAME is MD5" memory, which is *not* true of modern MAME.) |
| **TOSEC** (IBM, C64, Amiga, ZX Spectrum, TRS-80, …) | **CRC32 + MD5 + SHA1** | This — not MAME — is what makes MD5 first-class: ~1.25M ROMs. |

**MD5 is stored but not a *matching* path today.** The planner matches on SHA1
(direct + headerless), then CRC32+size for the SHA1-less remainder; `dat_roms.md5`
is populated for TOSEC but never consulted. Adding an MD5 match path is the
concrete reason to build this table.

## The design

A single tall lookup table, format as a column:

```
hashes(algo TEXT, digest TEXT, size INTEGER, content_id …)   -- index on (algo, digest)
```

- **One table, not per-format.** SQLite stores variable-width digests fine, so
  "all SHA1s are 40 chars" is no reason to isolate them. Separate tables make
  every new format a new table + migration + code path, and turn any
  "everything about this content" query into an N-way UNION. Adding a format
  should be **data, not DDL**.
- **A format *column*, not a `"sha1:…"` prefix.** A prefix bakes the algorithm
  into the value, forcing substring parsing to filter by it. A real `algo` column
  is typed, indexed, and queryable for the same flexibility.
- **Same row count** as the per-format option — storing N digests per file is N
  rows either way; the tall table just keeps them in one queryable place.

## Domain constraints baked into the design

- **CRC32 is not a content identity.** It is a 32-bit checksum with real
  collisions — which is why the planner already matches CRC32 **+ size**, never
  CRC32 alone. The strong hash (SHA1, later SHA256) is the canonical content key;
  CRC32 is a weaker lookup path that **requires `size`** (kept in the row).
- **Not every content has every hash.** CHDs are SHA1-only; a CRC-only DAT entry
  has no SHA1. The table is sparse by nature — another reason the tall shape fits
  (absent format = absent row, no NULL columns).
- Strong hash is the dedup key; the other formats are **aliases** into it (a
  DAT giving only CRC32+size, or only MD5, resolves to the same canonical
  content).

## Why it is decoupled from the icons fix

The oversized-collection planner fix (the dedup-index that lets
`all_non-zipped_content` plan instead of being skipped) dedups on the **existing**
SHA1 over the **existing** tables — no schema change, no migration, no re-scan.
This table, by contrast, is a **core schema change**: a migration plus a re-scan
to populate, touching scanning and matching. They ship separately; the icons fix
must not wait on, or drag in, this work.

## Drift triggers

- "MAME uses MD5" — no: MAME ROMs are **CRC32 + SHA1**, CHDs **SHA1-only**. MD5 is
  **TOSEC**. (Old MAME CHDs did carry MD5; modern ones do not.)
- "A table per hash format" / "prefix the digest with the format" — no: one tall
  table, format as a column.
- "`(algo, digest)` uniquely identifies content" — no for CRC32; it needs `size`.
  Strong hash is the identity; CRC32+size and MD5 are aliases.
- "Fold the hashes table into the icons fix" — no: the icons fix needs no schema
  change; this is separate, larger, migration-bearing work.
- "A hashes table would make matching faster" — no: matching already runs on
  covering indexes; a tall table is ~3× the rows plus an extra join hop, i.e.
  slower. Workflow slowness is I/O (scan/apply over the mount), not the schema.
- "Add MD5 matching" — gains nothing today: zero ROMs are MD5-only. Only worth it
  if an MD5-only (or SHA256-only) DAT actually appears.
