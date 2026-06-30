use anyhow::Result;
use std::collections::{BTreeMap, HashSet};

use super::destinations::validate_relative_path;
use super::matching::MatchedRom;
use super::{ContainerRebuild, Plan, RebuildEntry};

/// Accumulates, across the games that repack from one staging container, the
/// spec to rebuild that container on rollback.
struct ContainerDrain {
    /// Archive format to rebuild the container in (`zip` or `7z`).
    format: String,
    /// A representative destination, for the human-readable drain reason.
    reason_dest: String,
    /// Where each of the container's entries was repacked to.
    entries: Vec<RebuildEntry>,
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
    pub(crate) fn record_repack_from_container(
        &mut self,
        container: &str,
        dest: &str,
        entries: &[MatchedRom],
    ) -> Result<()> {
        let drain = self
            .pending
            .entry(container.to_string())
            .or_insert_with(|| ContainerDrain {
                format: container_archive_format(container),
                reason_dest: dest.to_string(),
                entries: Vec::new(),
            });
        for m in entries {
            if let Some(archive_entry) = &m.archive_path {
                validate_relative_path("archive entry name", archive_entry)?;
                validate_relative_path("ROM entry name", &m.rom_name)?;
                drain.entries.push(RebuildEntry {
                    dest: dest.to_string(),
                    dest_entry: m.rom_name.clone(),
                    container_entry: archive_entry.clone(),
                    sha1: m.sha1.clone(),
                });
            }
        }

        Ok(())
    }

    pub(crate) fn emit_into(self, plan: &mut Plan) {
        emit_container_drains(plan, self.pending);
    }
}

fn container_archive_format(path: &str) -> String {
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
