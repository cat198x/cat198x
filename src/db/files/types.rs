/// Whether a source's content may leave it. See
/// `decisions/source-disposition.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Staging: content may leave the tree and the source be freed (moved out).
    Consume,
    /// Content is never lost from the tree — reorganised within it, copied out,
    /// but never removed. The library and reference masters are `preserve`.
    Preserve,
}

impl Disposition {
    /// The canonical lowercase string stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Consume => "consume",
            Disposition::Preserve => "preserve",
        }
    }

    /// Parse the stored string; unknown values fall back to the safe
    /// `Preserve` (a malformed disposition must never authorise removal).
    pub fn parse(s: &str) -> Disposition {
        match s {
            "consume" => Disposition::Consume,
            _ => Disposition::Preserve,
        }
    }
}

/// A source directory
#[derive(Debug, Clone)]
pub struct Source {
    pub id: i64,
    pub path: String,
    pub case_sensitive: bool,
    pub added_at: String,
    pub last_scanned: Option<String>,
    /// Whether this source may be consumed (emptied) or its content preserved.
    pub disposition: Disposition,
}

/// A content-addressed file
#[derive(Debug, Clone)]
pub struct File {
    pub sha1: String,
    pub md5: Option<String>,
    pub crc32: Option<String>,
    pub size: i64,
    pub first_seen: String,
}

/// A physical location where a file exists
#[derive(Debug, Clone)]
pub struct FileLocation {
    pub id: i64,
    pub sha1: String,
    pub source_id: i64,
    pub path: String,
    pub archive_path: Option<String>,
    pub last_seen: String,
}
