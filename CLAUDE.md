# Cat198x

The binary-asset cataloguing tool for the 198x family — inventory, DAT
verification, deduplication, and safe reorganisation of ROM/disk/test-suite
collections. One of five sibling projects under the `198x/` umbrella; see
[`../../CLAUDE.md`](../../CLAUDE.md) for umbrella context and cross-project rules,
[`../CLAUDE.md`](../CLAUDE.md) for the `cat198x` org-container layout, and
[`../../decisions/sibling-project-coordination.md`](../../decisions/sibling-project-coordination.md)
for the sibling relationship (Cat198x is the fifth sibling, peer to Code198x,
Emu198x, Asm198x, and Forge198x — not a child of any).

## What this is

A single-binary CLI that answers "what ROMs do I have, what am I missing, and
how do I tidy this safely?" It scans directories, identifies every file by
content hash, matches against [DAT](https://www.logiqx.com) reference databases,
and reorganises collections through a reviewable **plan → apply → rollback**
cycle. It manages the *binary* side of the umbrella asset library
([`../../assets/`](../../assets/)); the prose reference layer
([`../../reference/`](../../reference/)) is a separate concern.

User-facing overview is in [`README.md`](README.md); the data model and on-disk
layout are in [`SPECIFICATION.md`](SPECIFICATION.md).

## Rescue, not rewrite

Cat198x is a **rescue and rebrand of Romshelf** (~24k LoC Rust), not a
green-field cataloguer. Read
[`../../decisions/cat198x-asset-tooling.md`](../../decisions/cat198x-asset-tooling.md)
before proposing structural change: the binding stance is *evolve the existing
code*, fixing correctness and trust as small tested commits, not reimplement it.
The audit that preceded adoption already fixed the data-integrity blockers
(transactional writes, verify-before-delete, full-hash quarantine, headerless
matching); don't reintroduce the patterns it removed.

## Safety model (load-bearing)

The whole tool is built so a wrong command costs time, never data. Preserve
these invariants when touching `plan/` or `cli/apply`:

- **Plan before apply.** `plan` writes an explicit operation list; `apply` is the
  only command that mutates ROM files. `apply --dry-run` performs nothing.
- **The execution engine lives in the library.** `plan::executor` holds the
  verified file operations (copy/move/repack/extract/rollback + disk-space
  check) with no CLI or progress concerns, so other 198x tools (Forge198x) can
  drive the same audited primitives. The `apply` CLI calls into it.
- **Verify-before-delete + fsync.** A move copies, hash-verifies the destination,
  flushes it to disk, and only then removes the source. Cross-device rollback
  re-verifies too. Never reorder these.
- **Quarantine, not delete.** Files that don't belong move to a content-hash-named
  quarantine store, reversible via the operation log.

## Code layout

Single crate today (a workspace root, so it can split later — see
[`decisions/skeleton-lint-adaptations.md`](decisions/skeleton-lint-adaptations.md)).

- [`src/cli/`](src/cli) — one module per subcommand (`init`, `dat`, `source`,
  `scan`, `status`, `plan`, `apply`, `quarantine`, `torrent`, `export`,
  `doctor`, `update`, …). Orchestration + output only.
- [`src/db/`](src/db) — rusqlite layer (collections, dats, files, config,
  quarantine, schema). All state is in `db.sqlite`; the SQL is authoritative.
- [`src/dat/`](src/dat) — Logiqx / clrmamepro DAT parsing (streaming, for
  50 MB+ MAME DATs).
- [`src/scanner/`](src/scanner) — content hashing (SHA-1/MD5/CRC32), header
  detection (iNES/SMC/A78/LNX), archive entry hashing (ZIP + 7z).
- [`src/plan/`](src/plan) — the reorganisation model: `generator` (build a plan),
  `types`, `log` (rollback journal), and `executor` (the file-op engine).
- [`src/archive/`](src/archive) — ZIP / TorrentZIP writers.
- [`src/filter/`](src/filter) — 1G1R-style preference filtering.
- `src/util.rs` — shared helpers (`hex_upper`/`hex_lower`, `verify_sha1`,
  path/byte formatting).

## Quality discipline

This crate is on the [198x standard skeleton](../../decisions/project-skeleton-standard.md):
toolchain pinned in `rust-toolchain.toml`, `[workspace.lints]` with
`unsafe_code = "forbid"` and `unwrap_used`/`dbg_macro` denied, the build-time
`[profile.dev]` levers, and the fmt/clippy/coverage/build CI gates. Before
pushing, the local equivalent is `cargo fmt --all --check`, `cargo clippy
--workspace --all-targets -- -D warnings`, and `./scripts/coverage.sh`.

Two rescue-specific reconciliations are recorded in
[`decisions/skeleton-lint-adaptations.md`](decisions/skeleton-lint-adaptations.md)
and are binding:

- **No `unsafe` in our code.** `unsafe_code = "forbid"` is enforced crate-wide;
  disk-space queries go through the `fs4` crate, not raw FFI. Wrap any future
  syscall need in a vetted crate — don't relax to `deny` + `#[allow]`.
- **`unwrap`/`expect`/`dbg` are allowed in tests only** (`clippy.toml`).
  Production code uses `expect` with a documented invariant, never bare
  `unwrap`.

## Where things live

- [`decisions/`](decisions) — Cat198x-only decisions (rescue scope nuances,
  skeleton adaptations). Cross-project decisions live in
  [`../../decisions/`](../../decisions/); read those in scope before changes that
  touch more than this crate.
- `scripts/` — coverage runner + gate. `.github/workflows/` — CI + release.
- It catalogues the umbrella [`../../assets/`](../../assets/) binary library;
  hardware-fact prose is in [`../../reference/`](../../reference/) and
  [`../../syntheses/`](../../syntheses/), per
  [`../../decisions/shared-hardware-reference-canon.md`](../../decisions/shared-hardware-reference-canon.md).
