//! Archive utilities for reading and writing ROM archives
//!
//! This module provides functionality for:
//! - Writing ZIP archives with configurable compression
//! - TorrentZIP format for deterministic, reproducible archives

mod writer;

pub use writer::{TorrentZipWriter, ZipWriter, ZipWriterOptions, is_torrentzip_stamped};
pub(crate) use writer::{extract_archive_entry, resolve_zip_entry_index};
