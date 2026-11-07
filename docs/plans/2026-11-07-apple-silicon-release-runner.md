# Keep Apple Silicon release builds on an available runner

The checked 0.5.7 release PR's actual cargo-dist plan (run 42382206349,
job 128432008184) selects `macos-14` for `aarch64-apple-darwin`.
Build198x's release already demonstrated that this retired runner cannot
start: `The configuration 'macos-14-arm64-us-default' is not supported`.

1. Record the existing plan's runner assignments as the red case.
2. In `Cargo.toml`, give `aarch64-apple-darwin` an explicit `macos-15`
   runner and matching host, following the family's existing Asm198x setup.
3. Use the project's pinned cargo-dist 0.32.0 to regenerate/check the
   workflow and produce a fresh plan. Require all five targets to remain,
   only Apple Silicon's runner to change, and all archives to retain their
   targets. No dependency or emulator behaviour changes.
4. Run formatting/diff checks, push the small fix through checked CI and
   rebase-merge. Then author the regenerated 0.5.7 release notes, wait for
   final checks, and publish normally.
5. Verify the real release's producer jobs, public archive checksum and
   executable behaviour using disposable files, then remove this task's
   worktree and downloaded tool/archive caches after remote retention.

## Plan verification

Pinned cargo-dist 0.32.0 reproduces `macos-14` before the metadata change
and selects `macos-15` afterward. A strict comparison confirms that all five
native target entries and every archive/installer definition are unchanged
apart from this one runner value. No target is removed. `dist generate
--check` passes without rewriting the workflow: it already consumes the
computed runner matrix. Formatting and diff checks pass.

The disposable registry overlay reuses existing read-only cache entries;
missing sources unpack only under the task's temporary tool directory.
No new project dependency or persistent tool install is needed.
