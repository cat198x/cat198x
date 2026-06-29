//! Plan generation and management

pub mod apply;
pub(crate) mod archive_planning;
pub(crate) mod collisions;
pub(crate) mod desired_state;
pub(crate) mod destinations;
pub mod executor;
pub mod generator;
pub mod log;
pub(crate) mod matching;
pub(crate) mod placement_planning;
pub(crate) mod rules;
pub(crate) mod source_policy;
pub(crate) mod state_hash;
pub mod types;

pub use apply::{ApplyEvent, ApplyOptions, ApplyOutcome, OpView, apply_plan};
pub use collisions::{CollidingCollection, DestinationCollision, find_destination_collisions};
pub use desired_state::{DesiredState, compute_desired_state};
pub use generator::{PlanOptions, compute_state_hash, generate_plan, generate_plan_filtered};
pub use log::{LogEntry, LogStatus, LoggedOperation, OperationLog};
pub use types::{
    CollectionPlanStat, ContainerRebuild, CopyPlacement, Operation, OperationKind, OperationStatus,
    Plan, PlanSummary, RebuildEntry, SourceRef,
};
