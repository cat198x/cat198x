use std::collections::{BTreeMap, HashSet};

use super::{ContainerRebuild, Plan, RebuildEntry};

/// Accumulates, across the games that repack from one staging container, the
/// spec to rebuild that container on rollback.
pub(crate) struct ContainerDrain {
    /// Archive format to rebuild the container in (`zip` or `7z`).
    pub(crate) format: String,
    /// A representative destination, for the human-readable drain reason.
    pub(crate) reason_dest: String,
    /// Where each of the container's entries was repacked to.
    pub(crate) entries: Vec<RebuildEntry>,
}

/// Source containers a repack rebuilt from and that are safe to lose afterwards.
///
/// These are recorded during per-collection archive planning and emitted as
/// deletes after every repack, so apply rebuilds destinations first and the
/// verify-before-delete net proves each entry survives before removing the
/// source container. Keyed by container path so a container feeding several
/// games is drained once.
#[derive(Default)]
pub(crate) struct ContainerDrains {
    pending: BTreeMap<String, ContainerDrain>,
}

impl ContainerDrains {
    pub(crate) fn pending_mut(&mut self) -> &mut BTreeMap<String, ContainerDrain> {
        &mut self.pending
    }

    pub(crate) fn emit_into(self, plan: &mut Plan) {
        emit_container_drains(plan, self.pending);
    }
}

pub(crate) fn container_archive_format(path: &str) -> String {
    if path.to_ascii_lowercase().ends_with(".7z") {
        "7z".to_string()
    } else {
        "zip".to_string()
    }
}

/// Emit final deletes for source containers that repacks rebuilt from. These are
/// emitted after all repacks so apply runs the rebuilds first, then relies on
/// verify-before-delete to prove each entry survives at its destination.
fn emit_container_drains(plan: &mut Plan, drain_after_repack: BTreeMap<String, ContainerDrain>) {
    for (container, drain) in drain_after_repack {
        // One entry per in-container name: a name repeated across feeding games
        // is the same content, so either destination can rebuild it on rollback.
        let mut seen = HashSet::new();
        let entries: Vec<RebuildEntry> = drain
            .entries
            .into_iter()
            .filter(|e| seen.insert(e.container_entry.clone()))
            .collect();
        let reason = format!("consolidated into {}", drain.reason_dest);
        plan.add_container_drain(
            container,
            reason,
            ContainerRebuild {
                format: drain.format,
                entries,
            },
        );
    }
}
