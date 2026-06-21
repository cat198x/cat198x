//! Plan generation and management

pub mod apply;
pub mod executor;
pub mod generator;
pub mod log;
pub mod types;

pub use apply::{ApplyEvent, ApplyOptions, ApplyOutcome, OpView, apply_plan};
pub use generator::{PlanOptions, compute_state_hash, generate_plan, generate_plan_filtered};
pub use log::{LogEntry, LogStatus, LoggedOperation, OperationLog};
pub use types::{
    CollectionPlanStat, ContainerRebuild, Operation, OperationKind, OperationStatus, Plan,
    PlanSummary, RebuildEntry, SourceRef,
};
