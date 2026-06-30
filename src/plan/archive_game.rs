use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::SourceRef;
use super::destinations::validate_relative_path;
use super::matching::MatchedRom;

pub(super) type ContentKey = (String, String);

pub(super) struct ArchiveGame {
    pub(super) expected: Vec<ContentKey>,
    expected_keys: Vec<ContentKey>,
    pub(super) containers: BTreeMap<String, Vec<MatchedRom>>,
    container_keys: HashMap<String, HashSet<ContentKey>>,
    holders: HashMap<ContentKey, Vec<String>>,
    name_size: HashMap<String, u64>,
}

impl ArchiveGame {
    pub(super) fn from_matches(matches: Vec<MatchedRom>) -> Self {
        let mut expected = Vec::new();
        let mut seen = HashSet::new();
        let mut containers: BTreeMap<String, Vec<MatchedRom>> = BTreeMap::new();
        let mut container_keys: HashMap<String, HashSet<ContentKey>> = HashMap::new();
        let mut holders: HashMap<ContentKey, Vec<String>> = HashMap::new();
        let mut name_size: HashMap<String, u64> = HashMap::new();

        for m in matches {
            if seen.insert((m.rom_name.clone(), m.sha1.clone())) {
                expected.push((m.rom_name.clone(), m.sha1.clone()));
            }

            let container = format!("{}/{}", m.source_root, m.source_path);
            name_size.entry(m.rom_name.clone()).or_insert(m.size as u64);

            let key = content_key(&m.rom_name, &m.sha1);
            let keys = container_keys.entry(container.clone()).or_default();
            if keys.insert(key.clone()) {
                holders.entry(key).or_default().push(container.clone());
            }
            containers.entry(container).or_default().push(m);
        }

        let expected_keys = expected
            .iter()
            .map(|(name, sha1)| content_key(name, sha1))
            .collect();

        Self {
            expected,
            expected_keys,
            containers,
            container_keys,
            holders,
            name_size,
        }
    }

    pub(super) fn is_complete(&self, path: &str) -> bool {
        self.container_keys
            .get(path)
            .is_some_and(|set| self.expected_keys.iter().all(|key| set.contains(key)))
    }

    pub(super) fn build_from(&self, dest: &str) -> Option<String> {
        if self.is_complete(dest) {
            return Some(dest.to_string());
        }

        self.expected_keys
            .iter()
            .map(|key| self.holders.get(key).map_or(&[][..], Vec::as_slice))
            .min_by_key(|paths| paths.len())
            .and_then(|candidates| {
                candidates
                    .iter()
                    .find(|path| self.is_complete(path))
                    .cloned()
            })
    }

    pub(super) fn has_shared_content(&self, shared: &HashSet<String>) -> bool {
        self.expected.iter().any(|(_, sha1)| shared.contains(sha1))
    }

    pub(super) fn expected_size(&self) -> u64 {
        self.expected
            .iter()
            .filter_map(|(name, _)| self.name_size.get(name).copied())
            .sum()
    }

    pub(super) fn source_refs_for(
        &self,
        game_name: &str,
        build_from: Option<&str>,
    ) -> Result<Vec<SourceRef>> {
        match build_from {
            Some(path) => self.containers[path]
                .iter()
                .map(source_ref_for)
                .collect::<Result<Vec<_>>>()
                .with_context(|| format!("invalid DAT path in {game_name}")),
            None => self
                .containers
                .values()
                .flatten()
                .map(source_ref_for)
                .collect::<Result<Vec<_>>>()
                .with_context(|| format!("invalid DAT path in {game_name}")),
        }
    }

    pub(super) fn feeder_roots(&self, build_from: Option<&str>) -> Vec<&str> {
        match build_from {
            Some(path) => self.containers[path]
                .iter()
                .map(|m| m.source_root.as_str())
                .collect(),
            None => self
                .containers
                .values()
                .flatten()
                .map(|m| m.source_root.as_str())
                .collect(),
        }
    }
}

fn content_key(name: &str, sha1: &str) -> ContentKey {
    (name.to_string(), sha1.to_ascii_lowercase())
}

fn source_ref_for(m: &MatchedRom) -> Result<SourceRef> {
    validate_relative_path("ROM entry name", &m.rom_name)?;
    Ok(SourceRef {
        path: format!("{}/{}", m.source_root, m.source_path),
        archive_path: m.archive_path.clone(),
        sha1: m.sha1.clone(),
        entry_name: Some(m.rom_name.clone()),
    })
}
