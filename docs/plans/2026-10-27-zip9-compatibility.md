# ZIP 9 compatibility

The ZIP 9 name accessor returns a Result and tries UTF-8 before CP437 even
when the language-encoding flag is clear. That changes names already stored
in the catalogue. Preserve the ZIP 8 name interpretation at every production
reader while using ZIP 9 for archive I/O.

1. Add one private archive-name decoder in `src/archive/zip_name.rs` using
   raw bytes and the language-encoding flag (including validated Unicode Path
   overrides applied by zip). Use an ASCII fast path and the published CP437
   mapping without a new dependency.
2. Use it in `src/archive/writer.rs`, `src/scanner/archive.rs` and
   `src/cli/fetch.rs`. Propagate archive-reading errors through
   `src/plan/executor/archive_ops.rs` rather than returning a missing entry.
3. Test CP437/UTF-8 identity, scan-to-extract agreement, Unicode Path overrides,
   malformed input and existing repack/rollback behaviour. Show the regression
   fails under ZIP 9's default name accessor, then passes with the adapter.
4. Run format, Clippy, coverage and UI checks as applicable; push to PR #105,
   wait for required CI and merge by rebase. Finish PR #104 independently once
   its checks pass. Remove task worktrees and caches after commits are retained.
