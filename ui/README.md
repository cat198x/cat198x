# Cat198x UI

A desktop UI for Cat198x, built with [Tauri](https://tauri.app). It is a **thin
client over the shared operation surface** (`cat198x::ops`) — the same operations
the CLI formats and the `cat198x mcp` server exposes. Any action here is, by
construction, an operation an agent can invoke too. See
[`../../decisions/agent-native-surface-and-ui.md`](../../decisions/agent-native-surface-and-ui.md).

## This slice

The first vertical slice is **read-only**:

- **Status** — collection completeness against the active DATs (games / have /
  missing / % complete).
- **Plan** — the most recent saved plan rendered as a **diff** of operations
  (copy / move / relocate / repack / delete / quarantine) with a summary. The
  plan already *is* the diff; no reconcile model is involved.

Mutating actions (apply, reclaim, clean-superseded) land once the operation
surface grows structured progress events for long runs.

## Layout

- `src/main.rs` — the Tauri app and its commands (`status`, `plan_diff`). Each
  command opens the catalogue at `~/.cat198x` and calls `cat198x::ops`; it holds
  no logic of its own.
- `frontend/` — a dependency-free static frontend (HTML/CSS/JS). It talks to the
  commands through the injected `window.__TAURI__` global, so there is no Node
  build step.
- `tauri.conf.json`, `capabilities/`, `icons/` — Tauri configuration.

It is a **standalone crate** (excluded from the cat198x workspace) that
path-depends on the `cat198x` library, so the lean CLI crate keeps its own lints,
profiles, and release config.

## Running

```sh
cargo run            # from this directory — opens the window
```

`cargo build` produces the binary; bundling an installer (`.app`/`.dmg`) needs
the platform icon set and the Tauri CLI, which this slice does not wire up.

The UI reads the same catalogue as the CLI (`~/.cat198x/db.sqlite`); run
`cat198x init` and import a DAT first if it is empty.
