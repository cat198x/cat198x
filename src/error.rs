//! Error types for ROMShelf

use thiserror::Error;

/// Main error type for ROMShelf operations
#[derive(Error, Debug)]
pub enum RomShelfError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("DAT parsing error: {0}")]
    DatParse(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Source not found: {0}")]
    SourceNotFound(String),

    #[error("Collection not found: {0}")]
    CollectionNotFound(String),

    #[error("ROMShelf not initialized. Run 'romshelf init' first.")]
    NotInitialized,

    #[error("{0}")]
    Other(String),
}
