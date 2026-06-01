# Decision: Meeting the 198x skeleton lints in the Cat198x rescue

**Status:** Active. Binding for Cat198x.

**Date:** 2026-06-01.

## The decision

Cat198x adopts the [198x standard project skeleton](../../../decisions/project-skeleton-standard.md)
**in full** — including `unsafe_code = "forbid"` and `clippy::unwrap_used =
"deny"`. It is a rescue of ~24k LoC (Romshelf) that predates those gates, so two
specific reconciliations were needed where the existing code met the standard's
lints head-on. Both keep the lints at full strength; neither weakens the gate.

1. **Disk-space query uses the `fs4` crate, not hand-rolled FFI** — so
   `unsafe_code = "forbid"` holds with no per-site escape hatch.
2. **`clippy.toml` exempts `unwrap`/`expect`/`dbg` in tests** — `unwrap_used =
   "deny"` applies to production code, where the standard's "no `.unwrap()` in
   committed code" rule actually bites.

## 1. fs4 for disk space, keeping `unsafe_code = "forbid"`

The pre-apply disk-space check (`plan::executor::get_available_space`) originally
called `statvfs` (unix) and `GetDiskFreeSpaceExW` (Windows) through raw `libc`
and `windows-sys` FFI — three `unsafe` blocks. The skeleton mandates
`unsafe_code = "forbid"`, and `forbid` (unlike `deny`) **cannot** be overridden
with a local `#[allow]`. So the FFI and the lint could not coexist.

We replaced the FFI with [`fs4`](https://crates.io/crates/fs4)'s
`available_space()`, which wraps the same syscalls (`statvfs` /
`GetDiskFreeSpaceExW`) and returns the space available to non-privileged users —
the same quantity the old `f_bavail`-based code computed. No `unsafe` remains in
the crate, so `forbid` stands unmodified, and the direct `libc` / `windows-sys`
dependencies are gone.

**Alternatives considered:**

- **Relax to `unsafe_code = "deny"` + `#[allow(unsafe_code)]` on the FFI.** Keeps
  zero new deps and the FFI, but it is a real deviation from the standard's
  `forbid`, and the standard is the reference. Rejected in favour of holding the
  line.
- **Drop the disk-space check.** No new dep, no unsafe — but it loses a genuine
  safety feature (apply warns before filling a disk). Rejected; the check earns
  its place in a tool that moves large ROM sets.

`fs4` is boring, maintained, and small. Adding it to remove three `unsafe`
blocks and two platform-FFI dependencies is a net simplification.

## 2. `clippy.toml` allows unwrap/expect/dbg in tests

The skeleton denies `clippy::unwrap_used` and `clippy::dbg_macro`, encoding "no
`.unwrap()` / `dbg!` in committed code." Read literally that includes test code,
but in tests `unwrap`/`expect` are the normal, readable idiom — a panic there
*is* the failure signal, with a backtrace pointing at the assertion. The rescue
carries hundreds of test `unwrap`s; rewriting them into `?`-propagating
`Result`-returning test bodies would be churn that makes tests harder to read,
not safer.

So `clippy.toml` sets `allow-unwrap-in-tests`, `allow-expect-in-tests`, and
`allow-dbg-in-tests` to `true`. The deny still applies to all production code.
The two production `unwrap`s the lint surfaced (the scan progress-bar template,
the export output path) were rewritten as `expect` with a documented invariant —
`expect_used` is *not* denied, because an `expect` with a message gives a
diagnosable panic, unlike a bare `unwrap`.

Asm198x — the reference — needs no `clippy.toml`: it is greenfield and has no
test `unwrap`s. This file is a rescue-specific addition, not a divergence from
the standard's intent. The intent (production code never panics on a `None`/`Err`
without context) is fully enforced.

## Why this isn't drift

Both choices **strengthen or hold** the standard rather than weaken it:

- `forbid` is enforced crate-wide with no exceptions (option 1 would have added
  an exception; we chose the dependency instead).
- `unwrap_used` is denied in production with the test exemption scoped narrowly
  and documented in `clippy.toml` itself.

If a future change needs `unsafe` (e.g. a performance-critical inner loop or an
FFI a crate can't wrap), that is a real amendment to this record and to the
`forbid` stance — raise it here, do not silently switch to `deny` + `#[allow]`.

## Drift triggers

- **"Just relax `unsafe_code` to `deny` and `#[allow]` the FFI."** — no; that was
  the rejected alternative. The crate holds `forbid`; wrap unsafe in a vetted
  crate (`fs4` is the precedent) or raise an amendment here.
- **"Tests are failing the unwrap lint, so loosen `unwrap_used` to `warn`
  workspace-wide."** — no; the exemption is `clippy.toml`'s
  `allow-unwrap-in-tests`, scoped to tests. Production stays `deny`.
- **"Use `unwrap` in production, it's obviously fine here."** — no; use `expect`
  with a message documenting the invariant (the precedent is the scan template
  and export path).
- **"Drop `fs4` and inline the syscall, it's only a few lines."** — no; that
  reintroduces `unsafe` and breaks `forbid`. The whole point of the crate is to
  keep the FFI out of our `unsafe`-free codebase.

## Reference

- The standard this applies: [`../../../decisions/project-skeleton-standard.md`](../../../decisions/project-skeleton-standard.md).
- The rescue framing: [`../../../decisions/cat198x-asset-tooling.md`](../../../decisions/cat198x-asset-tooling.md).
- Code: `src/plan/executor.rs` (`get_available_space`), `Cargo.toml`
  (`[workspace.lints]`, `fs4`), `clippy.toml`.
