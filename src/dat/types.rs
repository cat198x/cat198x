//! DAT file types

/// Header information from a DAT file
#[derive(Debug, Clone, Default)]
pub struct DatHeader {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub url: Option<String>,
    pub category: Option<String>,
}

/// A game/set entry from a DAT
#[derive(Debug, Clone)]
pub struct DatGameEntry {
    pub name: String,
    pub description: Option<String>,
    pub clone_of: Option<String>,
    pub rom_of: Option<String>,
    pub is_bios: bool,
    pub is_device: bool,
    pub is_mechanical: bool,
    pub roms: Vec<DatRomEntry>,
    pub devices: Vec<String>,
}

/// A ROM entry within a game
#[derive(Debug, Clone)]
pub struct DatRomEntry {
    pub name: String,
    pub size: u64,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub status: RomStatus,
    pub merge: Option<String>,
}

/// Status of a ROM dump
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RomStatus {
    #[default]
    Good,
    BadDump,
    NoDump,
}

impl RomStatus {
    /// Parse a ROM status from a string, defaulting to Good for unknown values
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "baddump" => RomStatus::BadDump,
            "nodump" => RomStatus::NoDump,
            _ => RomStatus::Good,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            RomStatus::Good => "good",
            RomStatus::BadDump => "baddump",
            RomStatus::NoDump => "nodump",
        }
    }
}

/// Detected source type for a DAT
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatSourceType {
    NoIntro,
    Redump,
    Tosec,
    Mame,
    Custom,
}

impl DatSourceType {
    /// Try to detect the source type from header info
    pub fn detect(header: &DatHeader) -> Self {
        let name = header.name.to_lowercase();
        let author = header.author.as_deref().unwrap_or("").to_lowercase();
        let homepage = header.homepage.as_deref().unwrap_or("").to_lowercase();
        let category = header.category.as_deref().unwrap_or("").to_lowercase();

        if author.contains("no-intro") || name.contains("no-intro") {
            DatSourceType::NoIntro
        } else if author.contains("redump") || name.contains("redump") {
            DatSourceType::Redump
        } else if name.contains("tosec") || author.contains("tosec")
            || homepage.contains("tosec") || category.contains("tosec") {
            DatSourceType::Tosec
        } else if name.contains("mame") || author.contains("mame") {
            DatSourceType::Mame
        } else {
            DatSourceType::Custom
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DatSourceType::NoIntro => "nointro",
            DatSourceType::Redump => "redump",
            DatSourceType::Tosec => "tosec",
            DatSourceType::Mame => "mame",
            DatSourceType::Custom => "custom",
        }
    }
}
