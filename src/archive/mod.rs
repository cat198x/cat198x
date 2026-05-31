//! Archive utilities for reading and writing ROM archives
//!
//! This module provides functionality for:
//! - Writing ZIP archives with configurable compression
//! - TorrentZIP format for deterministic, reproducible archives

mod writer;

pub use writer::{TorrentZipWriter, ZipWriter, ZipWriterOptions};
