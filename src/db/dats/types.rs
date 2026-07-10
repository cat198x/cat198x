/// Merge mode for MAME-style ROM sets
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeMode {
    /// Every game contains all its ROMs (no inheritance)
    #[default]
    NonMerged,
    /// Clones only have unique ROMs; inherited ROMs come from parent
    Split,
    /// Parent contains all ROMs including clones (no separate clone archives)
    Merged,
}

/// A node in the DAT hierarchy
#[derive(Debug, Clone)]
pub struct DatNode {
    pub id: i64,
    pub version_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub node_type: String,
    pub path: String,
}

/// A game/set from a DAT
#[derive(Debug, Clone)]
pub struct DatGame {
    pub id: i64,
    pub node_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub parent_name: Option<String>,
    pub is_bios: bool,
    pub is_device: bool,
    pub is_mechanical: bool,
}

/// A ROM within a game
#[derive(Debug, Clone)]
pub struct DatRom {
    pub id: i64,
    pub game_id: i64,
    pub name: String,
    pub size: i64,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub status: String,
    pub merge_tag: Option<String>,
    /// True for a `<disk>` (CHD): `sha1` is the CHD's internal hash, the file is
    /// `<name>.chd`, and it is stored loose rather than packed.
    pub is_disk: bool,
}
