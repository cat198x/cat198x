//! Plan execution engine: the file operations that carry out a plan.
//!
//! These functions perform the actual copies, moves, repacks, extractions, and
//! rollback moves. Each writes to its destination, verifies the result against
//! the expected SHA-1, and only then removes any source — so an interrupted or
//! corrupt operation can never lose the original ROM.
//!
//! The engine holds no CLI or progress-reporting concerns: it takes paths and
//! plan types and returns `Result`s. That keeps it reusable — the `apply`
//! command drives it here, and other 198x tools (e.g. Forge198x) can call the
//! same primitives directly.

mod archive_ops;
mod fs_ops;
mod placement_ops;
mod safety_ops;
mod space;

pub use archive_ops::{
    RepackEvent, RepackJob, RepackOutcome, execute_repack, execute_repacks_concurrent,
    extract_from_archive,
};
pub use placement_ops::{
    PlacementEvent, PlacementJob, PlacementKind, PlacementOutcome, execute_copy, execute_move,
    execute_placements_concurrent, execute_relocate,
};
pub use safety_ops::{delete_has_surviving_copy, execute_quarantine, execute_rollback_move};
pub use space::check_disk_space;

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
