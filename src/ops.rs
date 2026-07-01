//! The shared operation surface.
//!
//! Each Cat198x operation is defined once here as a typed request → response, so
//! the CLI, the `cat198x mcp` server, and the Tauri UI are thin adapters over one
//! audited core rather than parallel implementations. See
//! `decisions/agent-native-surface-and-ui.md`: any action an adapter offers is,
//! by construction, an operation every other adapter can invoke.
//!
//! Functions here are **silent** — they return data and never print — so the
//! adapter owns all output. That is load-bearing for the MCP stdio server, whose
//! stdout is the JSON-RPC transport: a stray `println!` in an operation would
//! corrupt the protocol stream.
//!
//! This is the read-only foundation — collection status, the saved plan-as-diff,
//! and collection/source listings, the operations the first UI slice needs.
//! Mutating operations (apply, reclaim, clean-superseded) join it behind a
//! structured-progress-event design in a follow-up.

mod apply;
mod catalogue;
mod plans;

pub use apply::{ApplyProgress, ApplyReport, ApplyRunOptions, apply, apply_streaming};
pub use catalogue::{
    CollectionInfo, CollectionStatus, SourceInfo, collection_status, list_collections, list_sources,
};
pub use plans::{PendingItem, PendingWork, latest_plan, pending_work};

#[cfg(test)]
mod tests;
