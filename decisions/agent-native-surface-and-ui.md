# Agent-native operation surface and the Cat198x UI

**Status:** Accepted (2026-06-17).
**Scope:** Cat198x flagship — how the tool exposes its operations, and the desktop UI.
**Related:** the family agent-native-parity pattern (Emu198x MCP, Forge198x); [declarative-reconciliation.md](declarative-reconciliation.md) — the UI does not depend on the deferred reconcile model.

## Bottom line

Cat198x grows a desktop UI. To keep it consistent with how the tool is actually used — agents driving it — the UI is a **thin client over one shared operation surface in the library**, not a parallel codebase. Define each operation once in the library; expose it through three sibling adapters: the **CLI** (today), an **MCP server** (agents), and **Tauri commands** (the desktop frontend). Any action the UI offers is, by construction, an operation an agent can invoke. Build the MCP surface first; then the UI as a thin Tauri client, one vertical slice at a time.

## Context

The whole MAME reorg was an agent driving the CLI, with the human approving plans — not pushing buttons. That is the tool's real usage pattern, and it matches the family: Emu198x exposes MCP, and Forge198x's charter is a thin front-end over the siblings' MCP/CLI surfaces with agent-native parity. The executor already lives in the library, not the CLI (`cat198x/CLAUDE.md` safety model), so the shared surface is half-built.

## Decision

### One surface, three adapters

```
        cat198x library — the audited core (plan · executor · db)
        /          |            \
      CLI       MCP server   Tauri commands
    (today)     (agents)     (desktop frontend)
```

Each operation (`status`, `plan`, `apply`, `reclaim`, the Decision-1 cleanup, …) is defined once in the library as a typed request→response, with structured progress events for long runs. The three adapters surface the same operations and carry no logic of their own. Parity is structural, not bolted on: a UI action an agent could not invoke would need a library operation the MCP adapter does not expose, which the shared-surface rule forbids.

### MCP first

Build `cat198x mcp` before the UI. It is useful on its own — agents drive Cat198x headlessly, as Emu198x already allows — and it proves the operation surface before any pixels. The CLI is refactored to route through the same shared operations API (low risk; the executor is already decoupled).

### UI: Tauri, thin, slice-first

The desktop UI is **Tauri** (decided 2026-06-17 after reviewing Aptakube, Spacedrive, and GitButler — polished data/management apps in Cat198x's exact shape). The alternatives weighed — egui, Slint, SwiftUI+FFI — are in session history; the deciding factor was rich data/diff/browse layout over a 40k-entry library, where web tech excels and the polish ceiling is high.

The Tauri frontend calls the library through Tauri commands — the same operations the MCP and CLI adapters expose. The central view is the **plan-as-diff** (the plan already is the diff; no reconcile model needed), plus current state and live progress for long runs.

Build one vertical slice end-to-end first — read-only **status + plan-diff** — then **apply with live progress**, then cleanup/reclaim. Validate the shared-surface refactor on a thin slice before investing in UI breadth.

## Boundaries (deliberate)

- **Tauri is a new dependency/framework for Cat198x** — an accepted "Ask First" addition. The first build is a thin slice, not a workbench.
- **On-demand, not a daemon.** The UI invokes operations when the user (or an agent) acts; no background control loop.
- **A Cat198x-specific UI**, designed to later live inside Forge198x (the family workbench), not compete with it.
- **The choice does not transfer to Emu198x.** Cat198x is a data/management UI — web-tech territory. Emu198x renders a live framebuffer at 50/60 fps with audio and input-latency constraints — egui's domain, and where a Tauri Rust↔webview IPC boundary fights you. Any Emu198x UI-stack question is a separate, deliberate call in the Emu198x session. The apps that settled this choice (Aptakube/Spacedrive/GitButler) are all data tools; none drives a real-time render loop.

## Consequences

- One audited path for human, agent, and CLI; no UI-only capability.
- The MCP surface delivers value independently of the UI.
- The platform stays swappable: every adapter is thin, so a future TUI or a Forge198x panel is additive, not a rewrite.
