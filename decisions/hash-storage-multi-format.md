# Decision: Multi-format hash storage (the `hashes` table)

**Status:** Decided (design), **not yet built** — the planned next piece of work
after the oversized-collection dedup-index. Binding for Cat198x when built.

**Date:** 2026-06-22.

**Scope:** Cat198x catalogue schema — how a content's hashes are stored and
matched. Distinct from, and **decoupled from**, the oversized-collection planner
fix (see [declarative-reconciliation.md](declarative-reconciliation.md) and the
dedup-index work), which needs no schema change.

## Bottom line

DATs identify ROMs by **different hash formats**, and that is already live in the
catalogue — not hypothetical. When we add a persistent hashes layer it will be a
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
